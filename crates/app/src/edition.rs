//! What differs between Full and Mini, passed into the core as data (never `cfg(feature)` in domain crates).
//! The table of differences is in the brief and docs/PARITY.md.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    Full,
    Mini,
}

/// How the YouTube PoToken is obtained (resolver needs it only for some clients; normal playback works without).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoTokenMode {
    /// A hidden webview inside the app (Full, Tauri).
    Webview,
    /// A short-lived helper process with WebView2, started on demand (Mini).
    Helper,
    /// Never; rely on clients that need no token.
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditionProfile {
    pub edition: Edition,
    /// Animated lyrics (glow/slide/karaoke by word). Mini: line highlight only (LYR-011, LYR-002).
    pub animated_lyrics: bool,
    /// Spectrogram and bitrate check computed automatically. Mini: on request only (PLAY-303/304).
    pub auto_spectrum: bool,
    /// Story-style month review and Wrapped with animations. Mini: static pages (HOME-105, V23-09).
    pub story_animations: bool,
    pub po_token: PoTokenMode,
    /// Decoded cover cache limit in bytes.
    pub image_cache_bytes: usize,
}

impl EditionProfile {
    pub fn full() -> Self {
        Self {
            edition: Edition::Full,
            animated_lyrics: true,
            auto_spectrum: true,
            story_animations: true,
            po_token: PoTokenMode::Webview,
            image_cache_bytes: 64 << 20,
        }
    }

    pub fn mini() -> Self {
        Self {
            edition: Edition::Mini,
            animated_lyrics: false,
            auto_spectrum: false,
            story_animations: false,
            po_token: PoTokenMode::Helper,
            // Only covers not yet picked up by the UI stay here (Mini keeps the finished pictures, covers.rs).
            image_cache_bytes: 12 << 20,
        }
    }

    pub fn worker_threads(&self) -> usize {
        match self.edition {
            Edition::Full => 4,
            Edition::Mini => 2,
        }
    }
}
