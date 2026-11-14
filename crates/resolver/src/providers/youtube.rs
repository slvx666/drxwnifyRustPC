//! YouTube Music as an audio source (InnerTube), without an account.
//!
//! - visitorData from `music.youtube.com/sw.js_data` (the first `Cgt…`/`Cgs…` string); VISIONOS and ANDROID_VR
//!   need it or they answer UNPLAYABLE (InnerTube.kt).
//! - Search: WEB_REMIX with the «songs» filter, candidates checked by the strict gate (ProviderGate/SpotifyMapper).
//! - Stream: `player` with VISIONOS first (the only client measured to serve a whole file with pre-signed URLs), then
//!   ANDROID_VR 1.65.10, then TVHTML5 (YTPlayerUtils.STREAM_FALLBACK_CLIENTS). Audio-only formats, Opus preferred.
//!   Signature-ciphered formats (age-restricted tracks) need the JS solver — not done yet, such tracks fall to other
//!   sources.

use crate::matcher::Candidate;
use crate::provider::{
    AudioProvider, AudioQuery, AudioStream, ProviderError, ProviderId, ProviderMatch, ranked,
};
use drx_core::http::{Http, Method};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const API: &str = "https://music.youtube.com/youtubei/v1/";
const FILTER_SONG: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";
const USER_AGENT_WEB: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0";
/// YouTube may answer with another length for the same recording (video edits); the general wide gate applies.
const YT_DURATION_GAP_S: i64 = crate::matcher::DURATION_TOLERANCE_WIDE_S;

struct Client {
    name: &'static str,
    version: &'static str,
    id: &'static str,
    ua: &'static str,
    os_name: Option<&'static str>,
    os_version: Option<&'static str>,
    device_make: Option<&'static str>,
    device_model: Option<&'static str>,
    sdk: Option<&'static str>,
}

const WEB_REMIX: Client = Client {
    name: "WEB_REMIX",
    version: "1.20260213.01.00",
    id: "67",
    ua: USER_AGENT_WEB,
    os_name: None,
    os_version: None,
    device_make: None,
    device_model: None,
    sdk: None,
};
const VISIONOS: Client = Client {
    name: "VISIONOS",
    version: "0.1",
    id: "101",
    ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
    os_name: Some("visionOS"),
    os_version: Some("1.3.21O771"),
    device_make: Some("Apple"),
    device_model: Some("RealityDevice14,1"),
    sdk: None,
};
const ANDROID_VR_1_65_10: Client = Client {
    name: "ANDROID_VR",
    version: "1.65.10",
    id: "28",
    ua: "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip",
    os_name: Some("Android"),
    os_version: Some("12L"),
    device_make: Some("Oculus"),
    device_model: Some("Quest 3"),
    sdk: Some("32"),
};
const TVHTML5: Client = Client {
    name: "TVHTML5",
    version: "7.20260213.00.00",
    id: "7",
    ua: "Mozilla/5.0(SMART-TV; Linux; Tizen 4.0.0.2) AppleWebkit/605.1.15 (KHTML, like Gecko) SamsungBrowser/9.2 TV Safari/605.1.15",
    os_name: None,
    os_version: None,
    device_make: None,
    device_model: None,
    sdk: None,
};
const STREAM_CLIENTS: [&Client; 3] = [&VISIONOS, &ANDROID_VR_1_65_10, &TVHTML5];
const STREAM_TTL: Duration = Duration::from_secs(60 * 60 * 5);

fn context(c: &Client, visitor: Option<&str>) -> Value {
    let mut client =
        json!({ "clientName": c.name, "clientVersion": c.version, "hl": "ru", "gl": "RU" });
    for (k, v) in [
        ("osName", c.os_name),
        ("osVersion", c.os_version),
        ("deviceMake", c.device_make),
        ("deviceModel", c.device_model),
        ("androidSdkVersion", c.sdk),
    ] {
        if let Some(v) = v {
            client[k] = json!(v);
        }
    }
    if let Some(v) = visitor {
        client["visitorData"] = json!(v);
    }
    json!({ "client": client })
}

/// "3:21" or "1:02:03" → seconds.
fn parse_clock(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() < 2
        || parts.len() > 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    Some(
        parts
            .iter()
            .fold(0i64, |acc, p| acc * 60 + p.parse::<i64>().unwrap_or(0)),
    )
}

fn runs(v: &Value) -> Vec<&Value> {
    v.get("text")
        .and_then(|t| t.get("runs"))
        .and_then(Value::as_array)
        .map(|r| r.iter().collect())
        .unwrap_or_default()
}

/// Songs from a search answer: (videoId, title, artists, duration).
pub(crate) fn parse_search(v: &Value) -> Vec<Candidate> {
    let mut out = Vec::new();
    let sections = v
        .pointer("/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for s in sections {
        let Some(items) = s
            .pointer("/musicShelfRenderer/contents")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for it in items {
            let Some(r) = it.get("musicResponsiveListItemRenderer") else {
                continue;
            };
            let video = r
                .pointer("/playlistItemData/videoId")
                .or_else(|| r.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId"))
                .and_then(Value::as_str);
            let Some(video) = video else { continue };
            let cols = r
                .get("flexColumns")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let col = |i: usize| {
                cols.get(i)
                    .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
            };
            let title = col(0)
                .map(runs)
                .and_then(|r| {
                    r.first()
                        .and_then(|x| x.get("text"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            let meta = col(1).map(runs).unwrap_or_default();
            let mut artists: Vec<String> = meta
                .iter()
                .filter(|run| {
                    run.pointer("/navigationEndpoint/browseEndpoint/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType")
                        .and_then(Value::as_str)
                        == Some("MUSIC_PAGE_TYPE_ARTIST")
                })
                .filter_map(|run| run.get("text").and_then(Value::as_str).map(str::to_string))
                .collect();
            let texts: Vec<&str> = meta
                .iter()
                .filter_map(|run| run.get("text").and_then(Value::as_str))
                .collect();
            if artists.is_empty() {
                // Without links the first group (before " • ") names the artists.
                let first = texts.split(|t| t.trim() == "•").next().unwrap_or(&[]);
                artists = first
                    .iter()
                    .filter(|t| !matches!(t.trim(), "," | "&" | ""))
                    .map(|t| t.to_string())
                    .collect();
            }
            let duration = texts.iter().rev().find_map(|t| parse_clock(t));
            out.push(Candidate {
                id: video.to_string(),
                title,
                artist: artists.join(", "),
                duration_s: duration,
                is_video: false,
            });
        }
    }
    out
}

/// The best audio-only format with a plain URL: Opus (webm) by bitrate, else AAC (mp4).
pub(crate) fn best_audio(v: &Value) -> Option<(String, u32)> {
    let formats = v
        .pointer("/streamingData/adaptiveFormats")
        .and_then(Value::as_array)?;
    formats
        .iter()
        .filter_map(|f| {
            let mime = f.get("mimeType").and_then(Value::as_str)?;
            if !mime.starts_with("audio/") {
                return None;
            }
            let url = f.get("url").and_then(Value::as_str)?; // ciphered formats have signatureCipher instead
            let opus = mime.contains("opus");
            let br = f.get("bitrate").and_then(Value::as_u64).unwrap_or(0) as u32;
            Some((opus, br, url.to_string()))
        })
        .max_by_key(|(opus, br, _)| (*opus, *br))
        .map(|(_, br, url)| (url, br / 1000))
}

pub struct YouTube {
    http: Http,
    visitor: tokio::sync::Mutex<Option<String>>,
    streams: Mutex<HashMap<String, (AudioStream, Instant)>>,
    /// loudnessDb per video, for normalisation (`playerConfig.audioConfig.loudnessDb`).
    pub loudness: Mutex<HashMap<String, f32>>,
}

impl YouTube {
    pub fn new(http: Http) -> Self {
        Self {
            http,
            visitor: tokio::sync::Mutex::new(None),
            streams: Mutex::default(),
            loudness: Mutex::default(),
        }
    }

    async fn visitor_data(&self) -> Option<String> {
        let mut v = self.visitor.lock().await;
        if let Some(x) = v.as_ref() {
            return Some(x.clone());
        }
        let r = self
            .http
            .request(Method::GET, "https://music.youtube.com/sw.js_data")
            .header("User-Agent", USER_AGENT_WEB);
        let text = self.http.send(r).await.ok()?.text().await.ok()?;
        let parsed: Value = serde_json::from_str(text.get(5..)?).ok()?;
        let found = parsed
            .pointer("/0/2")
            .and_then(Value::as_array)?
            .iter()
            .find_map(|x| {
                let s = x.as_str()?;
                (s.starts_with("Cgt") || s.starts_with("Cgs")).then(|| s.to_string())
            })?;
        *v = Some(found.clone());
        Some(found)
    }

    async fn post(&self, endpoint: &str, c: &Client, body: Value) -> Result<Value, ProviderError> {
        let visitor = self.visitor_data().await;
        let mut body = body;
        body["context"] = context(c, visitor.as_deref());
        let mut r = self
            .http
            .request(Method::POST, &format!("{API}{endpoint}?prettyPrint=false"))
            .header("User-Agent", c.ua)
            .header("X-Goog-Api-Format-Version", "1")
            .header("X-YouTube-Client-Name", c.id)
            .header("X-YouTube-Client-Version", c.version)
            .header("X-Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .json(&body);
        if let Some(v) = &visitor {
            r = r.header("X-Goog-Visitor-Id", v.as_str());
        }
        let resp = self
            .http
            .send(r)
            .await
            .map_err(|e| ProviderError(e.to_string()))?;
        resp.json().await.map_err(|e| ProviderError(e.to_string()))
    }

    async fn player(&self, video: &str) -> Option<(AudioStream, u32)> {
        for c in STREAM_CLIENTS {
            let Ok(v) = self
                .post(
                    "player",
                    c,
                    json!({ "videoId": video, "contentCheckOk": true, "racyCheckOk": true }),
                )
                .await
            else {
                continue;
            };
            if v.pointer("/playabilityStatus/status")
                .and_then(Value::as_str)
                != Some("OK")
            {
                continue;
            }
            if let Some(l) = v
                .pointer("/playerConfig/audioConfig/loudnessDb")
                .and_then(Value::as_f64)
            {
                self.loudness
                    .lock()
                    .expect("loudness")
                    .insert(video.to_string(), l as f32);
            }
            if let Some((url, kbps)) = best_audio(&v) {
                // A client's URL can point at a googlevideo server that is unreachable from here or answers 403
                // (seen with VISIONOS while ANDROID_VR's URL for the same video plays): try the next client then.
                if !self.reachable(&url, c.ua).await {
                    tracing::info!(
                        client = c.name,
                        "youtube stream URL unreachable, next client"
                    );
                    continue;
                }
                return Some((
                    AudioStream {
                        url,
                        user_agent: Some(c.ua),
                    },
                    kbps,
                ));
            }
        }
        None
    }

    /// One small ranged GET with a short timeout: does this stream URL answer?
    async fn reachable(&self, url: &str, ua: &str) -> bool {
        let req = self
            .http
            .client()
            .get(url)
            .header("User-Agent", ua)
            .header("Range", "bytes=0-1")
            .timeout(Duration::from_secs(5))
            .send();
        matches!(req.await, Ok(r) if r.status().is_success())
    }
}

#[async_trait::async_trait]
impl AudioProvider for YouTube {
    fn id(&self) -> ProviderId {
        ProviderId::Youtube
    }
    fn search_timeout(&self) -> Duration {
        Duration::from_secs(9)
    }

    fn loudness_db(&self, track_id: &str) -> Option<f32> {
        self.loudness
            .lock()
            .expect("loudness")
            .get(track_id)
            .copied()
    }

    async fn search(&self, q: &AudioQuery) -> Result<Option<ProviderMatch>, ProviderError> {
        let text = q.search_text();
        if text.trim().is_empty() {
            return Ok(None);
        }
        let v = self
            .post(
                "search",
                &WEB_REMIX,
                json!({ "query": text, "params": FILTER_SONG.replace("%3D", "=") }),
            )
            .await?;
        let cands = parse_search(&v);
        for m in ranked(q, &cands, YT_DURATION_GAP_S, 3) {
            let c = cands.iter().find(|c| c.id == m.id);
            // Only a video that really streams counts as found (an unplayable one falls to the next match).
            let Some((stream, kbps)) = self.player(&m.id).await else {
                continue;
            };
            self.streams
                .lock()
                .expect("streams")
                .insert(m.id.clone(), (stream, Instant::now()));
            return Ok(Some(ProviderMatch {
                provider: ProviderId::Youtube,
                track_id: m.id.clone(),
                title: m.title,
                artist: m.artist,
                duration_ms: c.and_then(|c| c.duration_s).map(|d| d as u64 * 1000),
                confidence: m.score,
                quality_kbps: Some(kbps.clamp(48, 320)),
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
        let got = self.player(&m.track_id).await;
        if let Some((s, _)) = &got {
            self.streams
                .lock()
                .expect("streams")
                .insert(m.track_id.clone(), (s.clone(), Instant::now()));
        }
        Ok(got.map(|(s, _)| s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks() {
        assert_eq!(parse_clock("3:21"), Some(201));
        assert_eq!(parse_clock("1:02:03"), Some(3723));
        assert_eq!(parse_clock("Song"), None);
    }

    #[test]
    fn picks_opus_audio_with_url() {
        let v = json!({"streamingData": {"adaptiveFormats": [
            {"mimeType": "video/mp4", "url": "v", "bitrate": 900000},
            {"mimeType": "audio/mp4; codecs=\"mp4a.40.2\"", "url": "aac", "bitrate": 130000},
            {"mimeType": "audio/webm; codecs=\"opus\"", "url": "opus", "bitrate": 140000},
            {"mimeType": "audio/webm; codecs=\"opus\"", "signatureCipher": "s=..", "bitrate": 160000}
        ]}});
        assert_eq!(best_audio(&v), Some(("opus".to_string(), 140)));
    }

    /// Real network: search and stream a known track. DRX_TEST_NET=1; not run in CI.
    #[test]
    fn real_search_and_stream() {
        if std::env::var("DRX_TEST_NET").is_err() {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let yt = YouTube::new(Http::new().unwrap());
            let q = AudioQuery {
                catalog_id: None,
                title: "Blinding Lights".into(),
                artists: vec!["The Weeknd".into()],
                album: None,
                duration_ms: 200_000,
                isrc: None,
                excluded: vec![],
            };
            let m = yt.search(&q).await.unwrap();
            eprintln!(
                "youtube match: {:?}",
                m.as_ref().map(|m| (m.confidence, m.quality_kbps))
            );
            assert!(m.is_some());
        });
    }

    /// Real network: the stream of one video answers a ranged GET (catches 403 on stream URLs).
    /// DRX_TEST_YT_VIDEO=<video id>; not run in CI.
    #[test]
    fn real_stream_bytes() {
        let Ok(video) = std::env::var("DRX_TEST_YT_VIDEO") else {
            return;
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let http = Http::new().unwrap();
            let yt = YouTube::new(http.clone());
            for c in STREAM_CLIENTS {
                let v = yt
                    .post(
                        "player",
                        c,
                        json!({ "videoId": video, "contentCheckOk": true, "racyCheckOk": true }),
                    )
                    .await
                    .unwrap();
                let status = v.pointer("/playabilityStatus/status").cloned();
                let audio = best_audio(&v);
                let code = match &audio {
                    Some((url, _)) => {
                        let r = http
                            .client()
                            .get(url)
                            .header("User-Agent", c.ua)
                            .header("Range", "bytes=0-1023")
                            .send()
                            .await
                            .map(|r| r.status().as_u16());
                        format!("{r:?}")
                    }
                    None => "-".into(),
                };
                eprintln!(
                    "{} {}: status={status:?} audio={} get={code}",
                    c.name,
                    c.version,
                    audio.is_some()
                );
            }
        });
    }
}
