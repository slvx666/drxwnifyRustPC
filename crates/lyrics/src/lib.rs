//! Lyrics providers, AI translation, romanization. First provider: LRCLIB (port of `lrclib/LrcLib.kt`, search
//! strategy: title + artist, then the free-text query; synced lyrics within ±2 s of the track, else ±5 s, else plain).

use drx_core::http::{Http, Method};
use serde_json::Value;

/// One timed line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub time_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lyrics {
    Synced(Vec<Line>),
    Plain(String),
}

impl Lyrics {
    /// Index of the line sung at `ms` (synced lyrics only).
    pub fn line_at(&self, ms: u64) -> Option<usize> {
        match self {
            Lyrics::Synced(lines) => lines.iter().rposition(|l| l.time_ms <= ms),
            Lyrics::Plain(_) => None,
        }
    }
}

/// Parses LRC: `[mm:ss.xx] text`, several time tags per line allowed; metadata tags are skipped.
pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let tag = &stripped[..end];
            if let Some(ms) = parse_tag(tag) {
                times.push(ms);
            }
            rest = stripped[end + 1..].trim_start();
        }
        for t in times {
            out.push(Line {
                time_ms: t,
                text: rest.to_string(),
            });
        }
    }
    out.sort_by_key(|l| l.time_ms);
    out
}

fn parse_tag(tag: &str) -> Option<u64> {
    let (m, s) = tag.split_once(':')?;
    let m: u64 = m.trim().parse().ok()?;
    let secs: f64 = s.trim().parse().ok()?;
    Some(m * 60_000 + (secs * 1000.0).round() as u64)
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub mod betterlyrics;
pub mod kugou;
pub use betterlyrics::BetterLyrics;
pub use kugou::KuGou;

/// Which providers are on (the phone's enableLrclib / enableBetterLyrics / enableKugou).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sources {
    pub lrclib: bool,
    pub better: bool,
    pub kugou: bool,
}

impl Default for Sources {
    fn default() -> Self {
        Sources {
            lrclib: true,
            better: true,
            kugou: true,
        }
    }
}

/// Strips common suffixes that break lyrics search: "(Remastered 2024)", "(feat. X)",
/// "[Deluxe Edition]", "(Live)", etc. Port of TitleCleaner / cleanTitleForSearch from Android.
pub fn clean_title(title: &str) -> String {
    let mut out = title.to_string();
    // Remove content in parentheses/brackets that matches known noise patterns.
    for (open, close) in [('(', ')'), ('[', ']')] {
        while let Some(start) = out.find(open) {
            let Some(end) = out[start..].find(close).map(|i| start + i) else {
                break;
            };
            let inner = out[start + 1..end].to_lowercase();
            let noise = inner.contains("remaster")
                || inner.contains("feat.")
                || inner.contains("feat ")
                || inner.contains("ft.")
                || inner.contains("ft ")
                || inner.contains("deluxe")
                || inner.contains("special edition")
                || inner.contains("bonus track")
                || inner.contains("expanded")
                || inner.contains("anniversary")
                || inner.starts_with("live")
                || inner == "acoustic"
                || inner == "demo"
                || inner.contains("remix")
                || inner.contains("radio edit");
            if noise {
                out = format!("{}{}", &out[..start], &out[end + 1..]);
            } else {
                break;
            }
        }
    }
    // " - Remastered 2024" at the end
    if let Some(i) = out.rfind(" - ") {
        let tail = out[i + 3..].to_lowercase();
        if tail.contains("remaster") {
            out = out[..i].to_string();
        }
    }
    out.trim().to_string()
}

/// LrcLib first; while nothing synced is found, BetterLyrics, then KuGou (synced lyrics beat plain ones), as the
/// phone's provider order.
pub async fn find_any(
    http: &Http,
    title: &str,
    artist: &str,
    duration_s: Option<u64>,
    on: Sources,
) -> Option<Lyrics> {
    let clean = clean_title(title);
    let title = if clean.is_empty() { title } else { &clean };
    let mut best = if on.lrclib {
        LrcLib::new(http.clone())
            .find(title, artist, duration_s)
            .await
    } else {
        None
    };
    if matches!(best, Some(Lyrics::Synced(_))) {
        return best;
    }
    if on.better
        && let Some(l) = BetterLyrics::new(http.clone())
            .find(title, artist, duration_s)
            .await
    {
        return Some(l);
    }
    if on.kugou
        && let Some(l) = KuGou::new(http.clone())
            .find(title, artist, duration_s)
            .await
    {
        best = Some(l);
    }
    best
}

pub struct LrcLib {
    http: Http,
}

impl LrcLib {
    pub fn new(http: Http) -> Self {
        Self { http }
    }

    async fn search(&self, query: &str) -> Vec<Value> {
        let url = format!("https://lrclib.net/api/search?{query}");
        let r = self.http.request(Method::GET, &url).header(
            "User-Agent",
            "Drxwnify (https://github.com/slvx666/drxwnifyRustPC)",
        );
        match self.http.send(r).await {
            Ok(resp) => resp
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn find(&self, title: &str, artist: &str, duration_s: Option<u64>) -> Option<Lyrics> {
        let mut found = self
            .search(&format!(
                "track_name={}&artist_name={}",
                enc(title),
                enc(artist)
            ))
            .await;
        if found.is_empty() {
            found = self
                .search(&format!("q={}", enc(&format!("{artist} {title}"))))
                .await;
        }
        pick(&found, duration_s)
    }
}

/// Synced within ±2 s, then synced within ±5 s, then plain within ±5 s (unknown length: first synced, then plain).
pub fn pick(found: &[Value], duration_s: Option<u64>) -> Option<Lyrics> {
    let off = |v: &Value| {
        duration_s
            .map(|d| (v.get("duration").and_then(Value::as_f64).unwrap_or(0.0) - d as f64).abs())
    };
    let synced = |v: &Value| {
        v.get("syncedLyrics")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    };
    let plain = |v: &Value| {
        v.get("plainLyrics")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    };
    for limit in [2.0, 5.0] {
        if let Some(s) = found
            .iter()
            .filter(|v| off(v).is_none_or(|o| o <= limit))
            .find_map(synced)
        {
            let lines = parse_lrc(&s);
            if !lines.is_empty() {
                return Some(Lyrics::Synced(lines));
            }
        }
    }
    found
        .iter()
        .filter(|v| off(v).is_none_or(|o| o <= 5.0))
        .find_map(plain)
        .map(Lyrics::Plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lrc_parsing() {
        let l = parse_lrc("[ar:Someone]\n[00:12.50] First\n[00:05.00][01:00.00] Chorus\n");
        assert_eq!(l.len(), 3);
        assert_eq!(
            l[0],
            Line {
                time_ms: 5000,
                text: "Chorus".into()
            }
        );
        assert_eq!(l[1].time_ms, 12_500);
        assert_eq!(Lyrics::Synced(l).line_at(13_000), Some(1));
    }

    #[test]
    fn picks_by_duration() {
        let found = [
            json!({"duration": 300.0, "syncedLyrics": "[00:01.00] wrong"}),
            json!({"duration": 201.0, "syncedLyrics": "[00:01.00] right"}),
        ];
        match pick(&found, Some(200)) {
            Some(Lyrics::Synced(l)) => assert_eq!(l[0].text, "right"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            pick(
                &[json!({"duration": 200.0, "plainLyrics": "words"})],
                Some(203)
            ),
            Some(Lyrics::Plain("words".into()))
        );
    }

    #[test]
    fn clean_title_removes_noise() {
        assert_eq!(
            clean_title("Love Exchange Failure (Remastered 2024)"),
            "Love Exchange Failure"
        );
        assert_eq!(clean_title("Song Title (feat. Someone)"), "Song Title");
        assert_eq!(clean_title("In the End [Deluxe Edition]"), "In the End");
        assert_eq!(clean_title("Numb - Remastered 2020"), "Numb");
    }
}
