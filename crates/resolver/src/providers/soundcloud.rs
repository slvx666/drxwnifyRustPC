//! SoundCloud as an audio source, no account needed (port of `SoundCloudAudioProvider.kt`).
//!
//! - client_id: scraped from the web player's script bundles (`client_id:"<32 chars>"`, newest bundles last);
//!   re-scraped once when the API answers 401/403.
//! - Tracks with policy BLOCK/SNIP (region-blocked / Go+ preview) or without an unencrypted transcoding are skipped.
//! - Official uploads (uploader/publisher = the artist) are tried first; re-uploads must name the artist as whole
//!   words, be within 3 s and never count as a sure match (confidence ≤ 0.85).
//! - Stream preference: HLS AAC (aac_160k before 96k), progressive MP3, HLS MP3, HLS Opus. Encrypted HLS is DRM.

use crate::matcher::{self, Candidate};
use crate::provider::{
    AudioProvider, AudioQuery, AudioStream, ProviderError, ProviderId, ProviderMatch, ranked,
};
use drx_core::http::{Http, HttpError, Method, StatusCode};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const DESKTOP_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const REUPLOAD_DURATION_TOLERANCE_S: i64 = 3;
const REUPLOAD_MAX_CONFIDENCE: f64 = 0.85;
const STREAM_TTL: Duration = Duration::from_secs(20 * 60);

static EDIT_MARKERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(flip|edit|re-?edit|rework|vip|remake|extended|club mix|bass ?boost(ed)?|phonk|jersey( club)?|hardstyle|drill|amapiano|[\p{L}\p{N}]+ version)\b").unwrap()
});
const SAME_RECORDING: [&str; 4] = [
    "album version",
    "original version",
    "single version",
    "explicit version",
];
static SCRIPT_SRC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<script[^>]+src="(https://a-v2\.sndcdn\.com/assets/[^"]+\.js)""#).unwrap()
});
static CLIENT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"client_id\s*[:=]\s*"?([0-9a-zA-Z]{32})"#).unwrap());

#[derive(Clone)]
struct ScTrack {
    id: String,
    title: String,
    artist: String,
    duration_ms: u64,
    json: Value,
}

fn parse_track(o: &Value) -> Option<ScTrack> {
    let id = match o.get("id")? {
        Value::Number(n) => n.to_string(),
        Value::String(s) if !s.is_empty() => s.clone(),
        _ => return None,
    };
    if o.get("kind").and_then(Value::as_str).unwrap_or("track") != "track" {
        return None;
    }
    let policy = o.get("policy").and_then(Value::as_str).unwrap_or("");
    if policy.eq_ignore_ascii_case("BLOCK") || policy.eq_ignore_ascii_case("SNIP") {
        return None;
    }
    if !o.get("streamable").and_then(Value::as_bool).unwrap_or(true) {
        return None;
    }
    let publisher = o
        .get("publisher_metadata")
        .and_then(|p| p.get("artist"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let uploader = o
        .get("user")
        .and_then(|u| u.get("username"))
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(ScTrack {
        id,
        title: o
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        artist: if publisher.trim().is_empty() {
            uploader
        } else {
            publisher
        }
        .to_string(),
        duration_ms: o
            .get("full_duration")
            .or_else(|| o.get("duration"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        json: o.clone(),
    })
}

fn transcodings(o: &Value) -> &[Value] {
    o.get("media")
        .and_then(|m| m.get("transcodings"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn has_open_transcoding(t: &ScTrack) -> bool {
    transcodings(&t.json).iter().any(|x| {
        let p = x
            .get("format")
            .and_then(|f| f.get("protocol"))
            .and_then(Value::as_str);
        !x.get("snipped").and_then(Value::as_bool).unwrap_or(false)
            && matches!(p, Some("progressive") | Some("hls"))
    })
}

fn norm_name(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// An edit / flip / someone's "version" the catalog title doesn't name.
fn is_edit_of(catalog: &str, upload: &str) -> bool {
    let set = |s: &str| -> std::collections::HashSet<String> {
        EDIT_MARKERS
            .find_iter(&s.to_lowercase())
            .map(|m| m.as_str().to_string())
            .collect()
    };
    let mut up = set(upload);
    up.retain(|m| !SAME_RECORDING.contains(&m.as_str()));
    !up.is_empty() && !up.is_subset(&set(catalog))
}

/// The artist as whole words in the upload's title or the uploader name (or transliterated).
fn names_artist(q: &AudioQuery, c: &Candidate) -> bool {
    let title_words = words(&c.title);
    let uploader = norm_name(&c.artist);
    let named = q.artists.iter().any(|a| {
        let key = norm_name(a);
        let aw = words(a);
        !key.is_empty()
            && (key == uploader
                || (!aw.is_empty() && title_words.windows(aw.len()).any(|w| w == aw.as_slice())))
    });
    if named {
        return true;
    }
    let tokens = matcher::artist_tokens(&q.artists.join(", "));
    let non_latin = !tokens.iter().any(|t| {
        t.chars()
            .any(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    non_latin && matcher::artist_transliteration_matches(&c.title, &c.artist, &tokens)
}

pub struct SoundCloud {
    http: Http,
    client_id: tokio::sync::Mutex<Option<String>>,
    tracks: Mutex<HashMap<String, Value>>,
    streams: Mutex<HashMap<String, (AudioStream, Instant)>>,
}

impl SoundCloud {
    pub fn new(http: Http) -> Self {
        Self {
            http,
            client_id: tokio::sync::Mutex::new(None),
            tracks: Mutex::default(),
            streams: Mutex::default(),
        }
    }

    async fn get_text(&self, url: &str) -> Result<String, HttpError> {
        let r = self
            .http
            .request(Method::GET, url)
            .header("User-Agent", DESKTOP_UA)
            .header("Accept-Language", "en");
        Ok(self.http.send(r).await?.text().await?)
    }

    async fn scrape_client_id(&self) -> Option<String> {
        let html = self.get_text("https://soundcloud.com/").await.ok()?;
        let scripts: Vec<String> = SCRIPT_SRC
            .captures_iter(&html)
            .map(|c| c[1].to_string())
            .collect();
        for src in scripts.iter().rev() {
            if let Ok(js) = self.get_text(src).await
                && let Some(c) = CLIENT_ID.captures(&js)
            {
                return Some(c[1].to_string());
            }
        }
        None
    }

    async fn cid(&self, refresh: bool) -> Option<String> {
        let mut c = self.client_id.lock().await;
        if !refresh && let Some(x) = c.as_ref() {
            return Some(x.clone());
        }
        let fresh = self.scrape_client_id().await?;
        *c = Some(fresh.clone());
        Some(fresh)
    }

    /// GET an api-v2 URL with the client id (one re-scrape on 401/403).
    async fn api(&self, url: &str) -> Result<Value, ProviderError> {
        for refresh in [false, true] {
            let cid = self
                .cid(refresh)
                .await
                .ok_or_else(|| ProviderError("no SoundCloud client id".into()))?;
            let sep = if url.contains('?') { '&' } else { '?' };
            match self.get_text(&format!("{url}{sep}client_id={cid}")).await {
                Ok(t) => return serde_json::from_str(&t).map_err(|e| ProviderError(e.to_string())),
                Err(HttpError::Status { status, .. })
                    if (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
                        && !refresh =>
                {
                    continue;
                }
                Err(e) => return Err(ProviderError(e.to_string())),
            }
        }
        Err(ProviderError("SoundCloud refused the client id".into()))
    }

    async fn search_tracks(&self, text: &str) -> Result<Vec<ScTrack>, ProviderError> {
        let q: String = url_encode(text);
        let v = self
            .api(&format!(
                "https://api-v2.soundcloud.com/search/tracks?q={q}&limit=25&offset=0&app_locale=en"
            ))
            .await?;
        Ok(v.get("collection")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(parse_track)
            .collect())
    }

    fn rank(q: &AudioQuery, tracks: &[ScTrack]) -> Vec<matcher::Matched> {
        let cands: Vec<Candidate> = tracks
            .iter()
            .filter(|t| !is_edit_of(&q.title, &t.title))
            .map(|t| Candidate {
                id: t.id.clone(),
                title: t.title.clone(),
                artist: t.artist.clone(),
                duration_s: Some((t.duration_ms / 1000) as i64).filter(|d| *d > 0),
                is_video: false,
            })
            .collect();
        let key = norm_name(q.primary_artist());
        let (official, others): (Vec<Candidate>, Vec<Candidate>) = cands
            .into_iter()
            .partition(|c| !key.is_empty() && norm_name(&c.artist) == key);
        let reuploads: Vec<Candidate> = others.into_iter().filter(|c| names_artist(q, c)).collect();
        let mut out = ranked(
            q,
            &official,
            crate::provider::UPLOAD_DURATION_TOLERANCE_S,
            3,
        );
        out.extend(
            ranked(q, &reuploads, REUPLOAD_DURATION_TOLERANCE_S, 3)
                .into_iter()
                .map(|mut m| {
                    m.score = m.score.min(REUPLOAD_MAX_CONFIDENCE);
                    m
                }),
        );
        out.truncate(4);
        out
    }

    async fn resolve_stream(&self, track: &Value) -> Option<AudioStream> {
        let auth = track
            .get("track_authorization")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let mut options: Vec<(u8, u8, &Value)> = transcodings(track)
            .iter()
            .filter(|t| !t.get("snipped").and_then(Value::as_bool).unwrap_or(false))
            .filter_map(|t| {
                let f = t.get("format")?;
                let protocol = f.get("protocol").and_then(Value::as_str)?;
                let mime = f
                    .get("mime_type")
                    .and_then(Value::as_str)?
                    .split(';')
                    .next()?
                    .trim();
                let rank = match (protocol, mime) {
                    ("hls", "audio/mp4") => 0,
                    ("progressive", "audio/mpeg") => 1,
                    ("hls", "audio/mpeg") => 2,
                    ("hls", "audio/ogg") => 3,
                    _ => return None,
                };
                let low = t
                    .get("preset")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .contains("96") as u8;
                Some((rank, low, t))
            })
            .collect();
        options.sort_by_key(|(r, low, _)| (*r, *low));
        for (_, _, t) in options {
            let Some(url) = t.get("url").and_then(Value::as_str) else {
                continue;
            };
            let full = match auth {
                Some(a) => format!("{url}?track_authorization={a}"),
                None => url.to_string(),
            };
            if let Ok(v) = self.api(&full).await
                && let Some(u) = v
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|u| !u.is_empty())
            {
                return Some(AudioStream {
                    url: u.to_string(),
                    user_agent: None,
                });
            }
        }
        None
    }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[async_trait::async_trait]
impl AudioProvider for SoundCloud {
    fn id(&self) -> ProviderId {
        ProviderId::Soundcloud
    }
    fn search_timeout(&self) -> Duration {
        Duration::from_secs(9)
    }

    async fn search(&self, q: &AudioQuery) -> Result<Option<ProviderMatch>, ProviderError> {
        let text = q.search_text();
        if text.trim().is_empty() {
            return Ok(None);
        }
        let mut tracks: Vec<ScTrack> = self
            .search_tracks(&text)
            .await?
            .into_iter()
            .filter(has_open_transcoding)
            .collect();
        let mut best = Self::rank(q, &tracks);
        if best.is_empty() {
            // Uploads often mistag the artist into the title: retry with the bare title.
            tracks = self
                .search_tracks(matcher::title_core(&q.title))
                .await?
                .into_iter()
                .filter(has_open_transcoding)
                .collect();
            best = Self::rank(q, &tracks);
        }
        for m in best {
            let Some(t) = tracks.iter().find(|t| t.id == m.id) else {
                continue;
            };
            self.tracks
                .lock()
                .expect("tracks")
                .insert(t.id.clone(), t.json.clone());
            // Label releases are often DRM-only: only a real, unencrypted stream counts as found.
            let Some(s) = self.resolve_stream(&t.json).await else {
                continue;
            };
            self.streams
                .lock()
                .expect("streams")
                .insert(t.id.clone(), (s, Instant::now()));
            return Ok(Some(ProviderMatch {
                provider: ProviderId::Soundcloud,
                track_id: t.id.clone(),
                title: t.title.clone(),
                artist: t.artist.clone(),
                duration_ms: Some(t.duration_ms),
                confidence: m.score,
                quality_kbps: None,
            }));
        }
        Ok(None)
    }

    async fn stream(
        &self,
        _q: &AudioQuery,
        m: &ProviderMatch,
    ) -> Result<Option<AudioStream>, ProviderError> {
        if let Some((s, at)) = self.streams.lock().expect("streams").get(&m.track_id)
            && at.elapsed() < STREAM_TTL
        {
            return Ok(Some(s.clone()));
        }
        let json = self
            .tracks
            .lock()
            .expect("tracks")
            .get(&m.track_id)
            .cloned();
        let json = match json {
            Some(j) => j,
            None => {
                self.api(&format!(
                    "https://api-v2.soundcloud.com/tracks/{}",
                    m.track_id
                ))
                .await?
            }
        };
        let s = self.resolve_stream(&json).await;
        if let Some(s) = &s {
            self.streams
                .lock()
                .expect("streams")
                .insert(m.track_id.clone(), (s.clone(), Instant::now()));
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_are_not_the_recording() {
        assert!(is_edit_of("One Kiss", "One Kiss (SYLS Edit)"));
        assert!(is_edit_of("Timeless", "Timeless (Elias Version)"));
        assert!(!is_edit_of("Song", "Song (Album Version)"));
        assert!(!is_edit_of("Song (Extended Mix)", "Song (Extended Mix)"));
    }

    #[test]
    fn region_blocked_and_previews_skipped() {
        let blocked =
            serde_json::json!({"id": 1, "kind": "track", "policy": "BLOCK", "title": "x"});
        assert!(parse_track(&blocked).is_none());
        let snip = serde_json::json!({"id": 2, "policy": "SNIP"});
        assert!(parse_track(&snip).is_none());
        let ok =
            serde_json::json!({"id": 3, "title": "T", "user": {"username": "U"}, "duration": 1000});
        assert_eq!(parse_track(&ok).unwrap().artist, "U");
    }

    #[test]
    fn query_encoding() {
        assert_eq!(url_encode("Кино a&b"), "%D0%9A%D0%B8%D0%BD%D0%BE%20a%26b");
    }

    /// Real network: finds a well-known track. DRX_TEST_NET=1; not run in CI.
    #[test]
    fn real_search() {
        if std::env::var("DRX_TEST_NET").is_err() {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let sc = SoundCloud::new(Http::new().unwrap());
            let q = AudioQuery {
                catalog_id: None,
                title: "goodbye".into(),
                artists: vec!["Sewerslvt".into()],
                album: None,
                duration_ms: 1_019_000,
                isrc: None,
                excluded: vec![],
            };
            let m = sc.search(&q).await.unwrap();
            eprintln!(
                "soundcloud: {:?}",
                m.as_ref().map(|m| (m.confidence, m.duration_ms))
            );
            if let Some(m) = m {
                let s = sc.stream(&q, &m).await.unwrap();
                eprintln!("stream is hls={:?}", s.map(|s| s.url.contains(".m3u8")));
            }
        });
    }
}
