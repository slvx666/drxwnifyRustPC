//! Audio providers: where the audio of a catalog track can come from (port of `resolver/AudioProvider.kt`).
//! Metadata always belongs to the catalog; a provider only supplies a stream for a track the catalog described.

use crate::matcher::{self, Candidate};
use drx_catalog::model::Track;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    Youtube,
    Vk,
    Soundcloud,
    Bandcamp,
    Audius,
    Soulseek,
    Lossless,
}

impl ProviderId {
    /// What the service usually delivers, kbps (lossless = 1411). AUDIUS counts 128: counted higher it "won" races
    /// and then often failed to stream (AudioProvider.kt).
    pub fn typical_kbps(self) -> u32 {
        match self {
            ProviderId::Youtube => 160,
            ProviderId::Vk => 320,
            ProviderId::Soundcloud => 160,
            ProviderId::Bandcamp => 128,
            ProviderId::Audius => 128,
            ProviderId::Soulseek => 320,
            ProviderId::Lossless => 1411,
        }
    }

    /// Parses the stored name (Android `AudioProviderId` names in `audio_fallback_match.provider`).
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "YOUTUBE" => ProviderId::Youtube,
            "VK" => ProviderId::Vk,
            "SOUNDCLOUD" => ProviderId::Soundcloud,
            "BANDCAMP" => ProviderId::Bandcamp,
            "AUDIUS" => ProviderId::Audius,
            "SOULSEEK" => ProviderId::Soulseek,
            "LOSSLESS" => ProviderId::Lossless,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            ProviderId::Youtube => "YOUTUBE",
            ProviderId::Vk => "VK",
            ProviderId::Soundcloud => "SOUNDCLOUD",
            ProviderId::Bandcamp => "BANDCAMP",
            ProviderId::Audius => "AUDIUS",
            ProviderId::Soulseek => "SOULSEEK",
            ProviderId::Lossless => "LOSSLESS",
        }
    }
}

/// Default order: SoundCloud first (AudioProviderId.DEFAULT_ORDER). The user can reorder in settings.
pub const DEFAULT_ORDER: [ProviderId; 7] = [
    ProviderId::Soundcloud,
    ProviderId::Youtube,
    ProviderId::Vk,
    ProviderId::Lossless,
    ProviderId::Bandcamp,
    ProviderId::Audius,
    ProviderId::Soulseek,
];

/// What providers search for: the catalog's own description of the track.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioQuery {
    pub catalog_id: Option<String>,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_ms: u64,
    pub isrc: Option<String>,
    /// Provider track ids known to be unplayable for this track.
    pub excluded: Vec<String>,
}

impl AudioQuery {
    pub fn from_track(t: &Track) -> Self {
        Self {
            catalog_id: (!t.id.is_empty()).then(|| t.id.clone()),
            title: t.title.clone(),
            artists: t
                .artists
                .iter()
                .map(|a| a.name.clone())
                .filter(|n| !n.is_empty())
                .collect(),
            album: t
                .album
                .as_ref()
                .map(|a| a.name.clone())
                .filter(|n| !n.is_empty()),
            duration_ms: t.duration_ms,
            isrc: t.isrc.clone(),
            excluded: Vec::new(),
        }
    }

    pub fn primary_artist(&self) -> &str {
        self.artists.first().map(String::as_str).unwrap_or("")
    }

    /// Stable key for caches (AudioQuery.cacheKey).
    pub fn cache_key(&self) -> String {
        self.catalog_id.clone().unwrap_or_else(|| {
            format!(
                "{}|{}|{}",
                self.title.to_lowercase(),
                self.primary_artist().to_lowercase(),
                self.duration_ms / 1000
            )
        })
    }

    /// "Artist Title" search text; an ellipsis-cut title is searched without the dots (ProviderGate.searchText).
    pub fn search_text(&self) -> String {
        let title = matcher::title_core(&self.title).trim_end_matches(['.', '…', ' ']);
        [self.primary_artist(), title]
            .iter()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn with_artist(&self, artist: &str) -> Self {
        Self {
            artists: vec![artist.to_string()],
            ..self.clone()
        }
    }
}

/// A provider-side track that cleared the strict gates.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderMatch {
    pub provider: ProviderId,
    /// Enough to fetch a fresh stream later (survives restarts).
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: Option<u64>,
    pub confidence: f64,
    pub quality_kbps: Option<u32>,
}

impl ProviderMatch {
    pub fn expected_kbps(&self) -> u32 {
        self.quality_kbps
            .unwrap_or_else(|| self.provider.typical_kbps())
    }
}

/// A playable stream: an http(s) URL; HLS when it ends in .m3u8.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioStream {
    pub url: String,
    pub user_agent: Option<&'static str>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(pub String);

#[async_trait::async_trait]
pub trait AudioProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    /// Upper bound for one search, so a dead service can never stall resolution.
    fn search_timeout(&self) -> Duration;
    /// False when enabled but unable to work yet (VK without a login).
    fn is_ready(&self) -> bool {
        true
    }
    async fn search(&self, q: &AudioQuery) -> Result<Option<ProviderMatch>, ProviderError>;
    /// The track's loudness (dB) when the service reports it (YouTube `loudnessDb`), for normalisation.
    fn loudness_db(&self, _track_id: &str) -> Option<f32> {
        None
    }
    /// A fresh stream for a previously found match, or None if it can no longer be played.
    async fn stream(
        &self,
        q: &AudioQuery,
        m: &ProviderMatch,
    ) -> Result<Option<AudioStream>, ProviderError>;
}

/// Max length gap for user-upload services (SoundCloud/VK): re-uploads are often edits (ProviderGate).
pub const UPLOAD_DURATION_TOLERANCE_S: i64 = 8;

/// Every candidate that clears the strict gates, best first (ProviderGate.ranked).
pub fn ranked(
    q: &AudioQuery,
    candidates: &[Candidate],
    max_gap_s: i64,
    limit: usize,
) -> Vec<matcher::Matched> {
    let wanted = (q.duration_ms / 1000) as i64;
    let mut pool: Vec<Candidate> = candidates
        .iter()
        .filter(|c| {
            !q.excluded.contains(&c.id)
                && (wanted <= 0 || c.duration_s.is_none_or(|d| (d - wanted).abs() <= max_gap_s))
        })
        .cloned()
        .collect();
    let all = q.artists.join(", ");
    let mut out = Vec::new();
    while out.len() < limit && !pool.is_empty() {
        let Some(chosen) = matcher::select_best(
            &q.title,
            q.primary_artist(),
            &all,
            q.duration_ms as i64,
            &pool,
        ) else {
            break;
        };
        pool.retain(|c| c.id != chosen.id);
        out.push(chosen);
    }
    out
}

/// Other names to search the artist under when the first search found nothing (ProviderGate.artistVariants).
pub fn artist_variants(artist: &str) -> Vec<String> {
    let latin: String = artist
        .chars()
        .filter(|c| (*c as u32) < 0x250)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mixed = latin.chars().count() >= 2
        && latin != artist.trim()
        && latin.chars().any(char::is_alphabetic);
    let mut v = matcher::artist_aliases(artist);
    if mixed && !v.contains(&latin) {
        v.push(latin);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_text_drops_ellipsis_and_variant() {
        let q = AudioQuery {
            catalog_id: None,
            title: "ThisIsKillingMe... - Remastered".into(),
            artists: vec!["A".into()],
            album: None,
            duration_ms: 0,
            isrc: None,
            excluded: vec![],
        };
        assert_eq!(q.search_text(), "A ThisIsKillingMe");
        assert_eq!(artist_variants("Vile卑劣"), vec!["Vile".to_string()]);
    }
}
