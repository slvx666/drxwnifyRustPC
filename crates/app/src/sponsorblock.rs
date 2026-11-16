//! SponsorBlock (Android SponsorBlockManager): parts of a YouTube video that are not music (talk, intros — the
//! `music_offtopic` category by default) are skipped. Only for tracks that really play from YouTube: a YouTube
//! track, or a catalog track the resolver matched to a video. The video id goes out as a 4-letter prefix of its
//! SHA-256 (the k-anonymous endpoint), as on the phone.

use crate::App;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

const BASE: &str = "https://sponsor.ajay.app/api/skipSegments/";
pub const ENABLED_KEY: &str = "sponsorBlockEnabled";
const CATEGORIES_KEY: &str = "sponsorBlockCategories";
const DEFAULT_CATEGORIES: &str = "music_offtopic";

/// A part to skip, seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub category: String,
}

/// Every category the phone offers (SponsorBlockSettings), with a Russian name.
pub const CATEGORIES: [(&str, &str); 8] = [
    ("music_offtopic", "Немузыкальная часть"),
    ("intro", "Заставка"),
    ("outro", "Титры"),
    ("preview", "Анонс"),
    ("sponsor", "Реклама спонсора"),
    ("selfpromo", "Самореклама"),
    ("interaction", "Призыв подписаться"),
    ("filler", "Отступления"),
];
pub const TOAST_KEY: &str = "sponsorBlockShowToast";
pub const CATEGORIES_PREF: &str = CATEGORIES_KEY;
pub const DEFAULT: &str = DEFAULT_CATEGORIES;

/// The segments of `video` among a hash-prefix answer, in `categories`, «skip» ones only, by start.
pub fn pick(answer: &Value, video: &str, categories: &[String]) -> Vec<Segment> {
    let mut out: Vec<Segment> = answer
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e.get("videoID").and_then(Value::as_str).is_some_and(|v| v.eq_ignore_ascii_case(video)))
        .flat_map(|e| e.get("segments").and_then(Value::as_array).cloned().unwrap_or_default())
        .filter(|s| s.get("actionType").and_then(Value::as_str).unwrap_or("skip") == "skip")
        .filter(|s| s.get("category").and_then(Value::as_str).is_some_and(|c| categories.iter().any(|x| x == c)))
        .filter_map(|s| {
            let seg = s.get("segment")?.as_array()?;
            let (a, b) = (seg.first()?.as_f64()?, seg.get(1)?.as_f64()?);
            let category = s.get("category").and_then(Value::as_str).unwrap_or_default().to_string();
            (b > a).then_some(Segment { start: a, end: b, category })
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

impl App {
    async fn sponsor_segments(&self, video: &str, categories: &[String]) -> Vec<Segment> {
        let hash = Sha256::digest(video.as_bytes());
        let prefix: String = hash.iter().take(2).map(|b| format!("{b:02x}")).collect();
        let cats: String = categories.iter().map(|c| format!("&category={c}")).collect();
        let url = format!("{BASE}{prefix}?actionType=skip{cats}");
        let http = &self.inner.http;
        let Ok(resp) = http.send(http.request(drx_core::http::Method::GET, &url)).await else { return Vec::new() };
        let Ok(text) = resp.text().await else { return Vec::new() };
        serde_json::from_str::<Value>(&text).map(|v| pick(&v, video, categories)).unwrap_or_default()
    }

    /// Follows the playback: for each YouTube-played track (with the switch on), its segments are fetched and the
    /// position jumps over them.
    pub fn keep_sponsorblock(&self) {
        let app = self.clone();
        self.runtime.spawn(async move {
            let mut rx = app.now_playing();
            let mut current: Option<String> = None;
            let mut segments: Vec<Segment> = Vec::new();
            loop {
                let track = rx.borrow_and_update().track.clone();
                let id = track.as_ref().map(|t| t.id.clone());
                if id != current {
                    current = id;
                    segments.clear();
                    if let Some(t) = track.filter(|_| app.pref_bool(ENABLED_KEY).unwrap_or(false)) {
                        // The resolver's match is known once the track has started.
                        for _ in 0..40 {
                            if app.clock().track.is_some() || !app.now_playing().borrow().playing {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        }
                        if let Some(video) = app.inner.playback.youtube_id_of(&t) {
                            let cats: Vec<String> = app
                                .pref_string(CATEGORIES_KEY)
                                .unwrap_or_else(|| DEFAULT_CATEGORIES.into())
                                .split(',')
                                .map(|c| c.trim().to_string())
                                .filter(|c| !c.is_empty())
                                .collect();
                            if !cats.is_empty() {
                                segments = app.sponsor_segments(&video, &cats).await;
                            }
                        }
                    }
                }
                if !segments.is_empty() {
                    let c = app.clock();
                    if c.playing
                        && let Some(s) = segments.iter().find(|s| c.position_s >= s.start && c.position_s < s.end - 0.2)
                    {
                        app.seek(s.end);
                        // «Сообщать о пропуске» (sponsorBlockShowToast): «Пропущено: заставка».
                        if app.pref_bool(TOAST_KEY).unwrap_or(false) {
                            let name = CATEGORIES.iter().find(|(k, _)| *k == s.category).map_or("вставка", |(_, n)| n);
                            let _ = app.inner.events.send(crate::AppEvent::Notice(format!("Пропущено: {}", name.to_lowercase())));
                        }
                    }
                }
                let wait = if segments.is_empty() { Duration::from_secs(3600) } else { Duration::from_millis(400) };
                let _ = tokio::time::timeout(wait, rx.changed()).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_video_and_categories() {
        let v: Value = serde_json::from_str(
            r#"[{"videoID":"abc","segments":[
                {"category":"music_offtopic","actionType":"skip","segment":[200.5,230.0]},
                {"category":"sponsor","actionType":"skip","segment":[10,20]},
                {"category":"music_offtopic","actionType":"skip","segment":[0,12.5]}]},
               {"videoID":"other","segments":[{"category":"music_offtopic","actionType":"skip","segment":[1,2]}]}]"#,
        )
        .unwrap();
        let s = pick(&v, "abc", &["music_offtopic".into()]);
        let m = |a: f64, b: f64| Segment { start: a, end: b, category: "music_offtopic".into() };
        assert_eq!(s, vec![m(0.0, 12.5), m(200.5, 230.0)]);
    }
}
