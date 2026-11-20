//! BetterLyrics (port of `betterlyrics/BetterLyrics.kt`): `lyrics-api.boidu.dev/getLyrics` answers TTML (Apple Music
//! style, often word-by-word); here each `<p begin=…>` becomes one timed line (words are joined, background vocals
//! kept in the line as the phone's LRC conversion does).

use crate::{Line, Lyrics, enc};
use drx_core::http::{Http, Method};
use serde_json::Value;

pub struct BetterLyrics {
    http: Http,
}

impl BetterLyrics {
    pub fn new(http: Http) -> Self {
        Self { http }
    }

    pub async fn find(&self, title: &str, artist: &str, duration_s: Option<u64>) -> Option<Lyrics> {
        let mut url = format!(
            "https://lyrics-api.boidu.dev/getLyrics?s={}&a={}",
            enc(title),
            enc(artist)
        );
        if let Some(d) = duration_s.filter(|d| *d > 0) {
            url.push_str(&format!("&d={d}"));
        }
        let r = self
            .http
            .request(Method::GET, &url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .header("Accept", "application/json");
        let resp = self.http.send(r).await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        let lines = parse_ttml(v.get("ttml")?.as_str()?);
        (!lines.is_empty()).then_some(Lyrics::Synced(lines))
    }
}

/// "1:02:03.456", "02:03.456", "123.456s", "123.4" → ms.
fn time_ms(t: &str) -> Option<u64> {
    let t = t.trim();
    if let Some(s) = t.strip_suffix('s') {
        return s.parse::<f64>().ok().map(|x| (x * 1000.0).round() as u64);
    }
    let mut total = 0f64;
    for part in t.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some((total * 1000.0).round() as u64)
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    let end = tag[at..].find('"')?;
    Some(&tag[at..at + end])
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// The timed lines of a TTML document (one per `<p>`), with the document's `lyricOffset` applied.
pub fn parse_ttml(ttml: &str) -> Vec<Line> {
    let offset_ms = ttml
        .find("lyricOffset=\"")
        .and_then(|i| ttml[i + 13..].split('"').next())
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0i64, |s| (s * 1000.0) as i64);
    let mut out = Vec::new();
    let mut rest = ttml;
    while let Some(start) = rest.find("<p") {
        let after = &rest[start..];
        // `<p` must be the paragraph tag, not `<pre`/… .
        if !after[2..].starts_with([' ', '>']) {
            rest = &after[2..];
            continue;
        }
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let tag = &after[..tag_end];
        let Some(close) = after.find("</p>") else {
            break;
        };
        let inner = &after[tag_end + 1..close];
        rest = &after[close + 4..];
        let Some(begin) = attr(tag, "begin").and_then(time_ms) else {
            continue;
        };
        // Inner text without tags; spans of words are separated by real spaces in the document.
        let mut text = String::new();
        let mut in_tag = false;
        for c in inner.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => text.push(c),
                _ => {}
            }
        }
        let text = unescape(&text.split_whitespace().collect::<Vec<_>>().join(" "));
        if text.is_empty() {
            continue;
        }
        out.push(Line {
            time_ms: (begin as i64 + offset_ms).max(0) as u64,
            text,
        });
    }
    out.sort_by_key(|l| l.time_ms);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_ttml_lines() {
        let t = r#"<tt xmlns="http://www.w3.org/ns/ttml"><head><metadata><audio lyricOffset="0.5"/></metadata></head><body><div>
<p begin="00:01.000" end="00:03.000"><span begin="00:01.000">Hello</span> <span begin="00:01.500">world</span></p>
<p begin="1:02.250" end="1:04"><span>Tom &amp; Jerry</span></p></div></body></tt>"#;
        let l = super::parse_ttml(t);
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].time_ms, l[0].text.as_str()), (1500, "Hello world"));
        assert_eq!((l[1].time_ms, l[1].text.as_str()), (62750, "Tom & Jerry"));
    }
}
