//! KuGou lyrics (android kugou/KuGou.kt, from ViMusic): find the song by «title - artist», take the lyrics candidate
//! of a version within 8 s of the length, download it as LRC (base64), drop the credit lines at the head and tail.

use crate::{Lyrics, enc, parse_lrc};
use base64::Engine;
use drx_core::http::{Http, Method};
use serde_json::Value;

const PAGE_SIZE: u32 = 8;
const DURATION_TOLERANCE_S: i64 = 8;
const HEAD_CUT_LIMIT: usize = 30;

pub struct KuGou {
    http: Http,
}

/// Brackets and what is in them go from a title («(Remastered)», «（Live）», «《…》»…) — KuGou.normalizeTitle.
fn normalize_title(t: &str) -> String {
    let mut out = t.to_string();
    for (open, close) in [
        ('(', ')'),
        ('（', '）'),
        ('「', '」'),
        ('『', '』'),
        ('<', '>'),
        ('《', '》'),
        ('〈', '〉'),
        ('＜', '＞'),
    ] {
        // Greedy like the phone's regex: from the first opening to the last closing.
        if let (Some(a), Some(b)) = (out.find(open), out.rfind(close))
            && a < b
        {
            out.replace_range(a..b + close.len_utf8(), "");
        }
    }
    out.trim().to_string()
}

fn normalize_artist(a: &str) -> String {
    let s = a
        .replace(", ", "、")
        .replace(" & ", "、")
        .replace('.', "")
        .replace('和', "、");
    normalize_title(&s)
}

fn is_lrc_line(l: &str) -> bool {
    // [mm:ss.xx] or [mm:ss.xxx]
    let b = l.as_bytes();
    b.len() >= 9
        && b[0] == b'['
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3] == b':'
        && b[4].is_ascii_digit()
        && b[5].is_ascii_digit()
        && b[6] == b'.'
        && l[7..].find(']').is_some_and(|e| {
            (2..=3).contains(&e) && l[7..7 + e].bytes().all(|c| c.is_ascii_digit())
        })
}

/// «[00:01.00]作词 : Someone» — a credit line, not lyrics (BANNED_REGEX `.+].+[:：].+`).
fn is_credit(l: &str) -> bool {
    match l.find(']') {
        Some(e) => {
            let text = &l[e + 1..];
            text.find([':', '：'])
                .is_some_and(|i| i > 0 && i + 1 < text.len())
        }
        None => false,
    }
}

/// KuGou's String.normalize: LRC lines only, without the credit block at the start and at the end.
pub fn clean(lrc: &str) -> String {
    let lines: Vec<&str> = lrc
        .lines()
        .map(str::trim)
        .filter(|l| is_lrc_line(l))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let last = lines.len() - 1;
    let mut head = 0;
    for i in (0..=HEAD_CUT_LIMIT.min(last)).rev() {
        if is_credit(lines[i]) {
            head = i + 1;
            break;
        }
    }
    let mut tail = 0;
    let from = lines.len().saturating_sub(HEAD_CUT_LIMIT).min(last);
    for i in (0..=from).rev() {
        if is_credit(lines[last - i]) {
            tail = i + 1;
            break;
        }
    }
    let kept = &lines[head.min(lines.len())..];
    let kept = &kept[..kept.len().saturating_sub(tail)];
    kept.join("\n")
}

impl KuGou {
    pub fn new(http: Http) -> Self {
        Self { http }
    }

    async fn get(&self, url: &str) -> Option<Value> {
        let resp = self
            .http
            .send(self.http.request(Method::GET, url))
            .await
            .ok()?;
        let text = resp.text().await.ok()?;
        serde_json::from_str(&text).ok()
    }

    pub async fn find(&self, title: &str, artist: &str, duration_s: Option<u64>) -> Option<Lyrics> {
        let keyword = format!("{} - {}", normalize_title(title), normalize_artist(artist));
        let dur = duration_s.map(|d| d as i64);
        let near = |d: i64| dur.is_none_or(|want| (d - want).abs() <= DURATION_TOLERANCE_S);
        let mut candidate: Option<(i64, String)> = None;
        let songs = self
            .get(&format!(
                "https://mobileservice.kugou.com/api/v3/search/song?version=9108&plat=0&pagesize={PAGE_SIZE}&showtype=0&keyword={}",
                enc(&keyword)
            ))
            .await;
        let infos = songs
            .as_ref()
            .and_then(|v| v.pointer("/data/info"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for song in infos {
            let d = song.get("duration").and_then(Value::as_i64).unwrap_or(-1);
            let Some(hash) = song.get("hash").and_then(Value::as_str) else {
                continue;
            };
            if !near(d) {
                continue;
            }
            let found = self
                .get(&format!(
                    "https://lyrics.kugou.com/search?ver=1&man=yes&client=pc&hash={}",
                    enc(hash)
                ))
                .await;
            if let Some(c) = first_candidate(found.as_ref()) {
                candidate = Some(c);
                break;
            }
        }
        if candidate.is_none() {
            let by_dur = dur
                .map(|d| format!("&duration={}", d * 1000))
                .unwrap_or_default();
            let found = self
                .get(&format!(
                    "https://lyrics.kugou.com/search?ver=1&man=yes&client=pc{by_dur}&keyword={}",
                    enc(&keyword)
                ))
                .await;
            candidate = first_candidate(found.as_ref());
        }
        let (id, key) = candidate?;
        let got = self
            .get(&format!(
                "https://lyrics.kugou.com/download?fmt=lrc&charset=utf8&client=pc&ver=1&id={id}&accesskey={}",
                enc(&key)
            ))
            .await?;
        let b64 = got.get("content").and_then(Value::as_str)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .ok()?;
        let text = clean(&String::from_utf8_lossy(&bytes));
        let lines = parse_lrc(&text);
        (!lines.is_empty()).then_some(Lyrics::Synced(lines))
    }
}

fn first_candidate(v: Option<&Value>) -> Option<(i64, String)> {
    let c = v?.get("candidates")?.as_array()?.first()?;
    let id = c.get("id").and_then(|x| {
        x.as_i64()
            .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
    })?;
    let key = c.get("accesskey").and_then(Value::as_str)?.to_string();
    Some((id, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_credits() {
        // A real song is longer than the 30 lines the credit search looks at, at each end.
        let body: Vec<String> = (10..50)
            .map(|i| format!("[00:{i:02}.00]Line {i}"))
            .collect();
        let lrc = format!(
            "[00:00.00]Song - Artist\n[00:01.00]作词 : A\n[00:02.00]作曲 : B\n{}\nnot lrc\n[03:00.00]制作人：C\n",
            body.join("\n")
        );
        assert_eq!(clean(&lrc), body.join("\n"));
        assert_eq!(normalize_title("Song (Remastered 2011)"), "Song");
        assert_eq!(normalize_artist("A, B & C"), "A、B、C");
        assert!(is_lrc_line("[01:02.345]x"));
        assert!(!is_lrc_line("[ar:Someone]"));
    }

    #[tokio::test]
    #[ignore]
    async fn live_kugou() {
        let http = drx_core::http::Http::new().expect("http");
        let l = KuGou::new(http)
            .find("Never Gonna Give You Up", "Rick Astley", Some(213))
            .await;
        match l {
            Some(Lyrics::Synced(lines)) => {
                println!("{} lines, first: {:?}", lines.len(), lines.first());
                assert!(lines.len() > 10);
            }
            other => panic!("no synced lyrics: {other:?}"),
        }
    }
}
