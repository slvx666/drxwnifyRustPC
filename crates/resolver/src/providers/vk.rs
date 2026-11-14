//! VK uploads as an audio source (port of `VkAudioProvider.uploads` / `stream`).
//! Search "Artist Title" (30 results), rank with the strict gate (uploads may differ by 8 s at most); a band's old
//! name is tried when nothing fits. Stream URLs come with search results; otherwise `audio.getById`.

use crate::matcher::Candidate;
use crate::provider::{
    AudioProvider, AudioQuery, AudioStream, ProviderError, ProviderId, ProviderMatch,
    UPLOAD_DURATION_TOLERANCE_S, artist_variants, ranked,
};
use drx_catalog::vk::{CLIENT_USER_AGENT, Vk, VkAudio};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct VkProvider {
    vk: Arc<Vk>,
    /// Recently seen stream URLs by full id (they expire after a while; re-fetched by id).
    urls: Mutex<HashMap<String, (String, Instant)>>,
}

const URL_TTL: Duration = Duration::from_secs(60 * 30);

impl VkProvider {
    pub fn new(vk: Arc<Vk>) -> Self {
        Self {
            vk,
            urls: Mutex::new(HashMap::new()),
        }
    }

    fn remember(&self, list: &[VkAudio]) {
        let now = Instant::now();
        let mut m = self.urls.lock().expect("urls");
        for a in list {
            if let Some(u) = &a.url {
                m.insert(full_id(&a.track.id).to_string(), (u.clone(), now));
            }
        }
    }

    fn rank(q: &AudioQuery, found: &[VkAudio]) -> Vec<crate::matcher::Matched> {
        let cands: Vec<Candidate> = found
            .iter()
            .map(|a| Candidate {
                id: full_id(&a.track.id).to_string(),
                title: a.track.title.clone(),
                artist: a.track.artist_names(),
                duration_s: Some((a.track.duration_ms / 1000) as i64).filter(|d| *d > 0),
                is_video: false,
            })
            .collect();
        ranked(q, &cands, UPLOAD_DURATION_TOLERANCE_S, 5)
    }
}

/// `src:VK:owner_audio[_key]` → `owner_audio[_key]`.
fn full_id(track_id: &str) -> &str {
    track_id.strip_prefix("src:VK:").unwrap_or(track_id)
}

fn err(e: impl std::fmt::Display) -> ProviderError {
    ProviderError(e.to_string())
}

#[async_trait::async_trait]
impl AudioProvider for VkProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Vk
    }
    fn search_timeout(&self) -> Duration {
        Duration::from_secs(8)
    }
    fn is_ready(&self) -> bool {
        self.vk.has_account()
    }

    async fn search(&self, q: &AudioQuery) -> Result<Option<ProviderMatch>, ProviderError> {
        let text = q.search_text();
        if text.trim().is_empty() {
            return Ok(None);
        }
        let mut items = self.vk.search(&text, 30).await.map_err(err)?;
        let mut best = Self::rank(q, &items);
        if best.is_empty() {
            for alias in artist_variants(q.primary_artist()) {
                let again = self
                    .vk
                    .search(&q.with_artist(&alias).search_text(), 30)
                    .await
                    .map_err(err)?;
                best = Self::rank(q, &again);
                if !best.is_empty() {
                    items = again;
                    break;
                }
            }
        }
        self.remember(&items);
        let Some(m) = best.into_iter().next() else {
            return Ok(None);
        };
        let a = items.iter().find(|a| full_id(&a.track.id) == m.id);
        Ok(Some(ProviderMatch {
            provider: ProviderId::Vk,
            track_id: m.id.clone(),
            title: m.title,
            artist: m.artist,
            duration_ms: a.map(|a| a.track.duration_ms),
            confidence: m.score,
            quality_kbps: None,
        }))
    }

    async fn stream(
        &self,
        _q: &AudioQuery,
        m: &ProviderMatch,
    ) -> Result<Option<AudioStream>, ProviderError> {
        let cached = self
            .urls
            .lock()
            .expect("urls")
            .get(&m.track_id)
            .filter(|(_, at)| at.elapsed() < URL_TTL)
            .map(|(u, _)| u.clone());
        let url = match cached {
            Some(u) => Some(u),
            None => {
                let got = self
                    .vk
                    .tracks_by_id(std::slice::from_ref(&m.track_id))
                    .await
                    .map_err(err)?;
                self.remember(&got);
                got.into_iter().next().and_then(|a| a.url)
            }
        };
        Ok(url.map(|url| AudioStream {
            url,
            user_agent: Some(CLIENT_USER_AGENT),
        }))
    }
}
