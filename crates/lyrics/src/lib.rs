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

pub mod kugou;
pub use kugou::KuGou;

/// LrcLib first; when it has nothing synced, KuGou (synced lyrics beat plain ones), as the phone's provider order.
pub async fn find_any(
    http: &Http,
    title: &str,
    artist: &str,
    duration_s: Option<u64>,
    lrclib: bool,
    kugou: bool,
) -> Option<Lyrics> {
    let first = if lrclib { LrcLib::new(http.clone()).find(title, artist, duration_s).await } else { None };
    if matches!(first, Some(Lyrics::Synced(_))) || !kugou {
        return first;
    }
    match KuGou::new(http.clone()).find(title, artist, duration_s).await {
        Some(l) => Some(l),
        None => first,
    }
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
}
