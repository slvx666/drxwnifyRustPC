//! OS integration for both editions (ARCHITECTURE.md, `platform`): system media controls, tray, single instance.
//!
//! Windows: one thread with a hidden top-level window owns everything that needs a window — the system media
//! transport controls (SMTC: the volume flyout, the lock screen, the keyboard's media keys; PC-002) and the tray
//! icon with its menu (PC-003). A named pipe keeps one instance per profile: a second launch hands over and quits
//! (PC-004; a pipe, not the TCP port of pc/). Linux (MPRIS, StatusNotifierItem) comes with milestone 11; until then
//! these calls do nothing there.
//!
//! The crate knows nothing about the app: it reports `PlatformEvent`s and shows the `MediaInfo` it is given.

#[cfg(windows)]
mod windows_impl;

/// What the OS asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum PlatformEvent {
    Play,
    Pause,
    TogglePlay,
    Next,
    Previous,
    Stop,
    /// Seek to seconds (the SMTC timeline was dragged).
    Seek(f64),
    /// Bring the window up: the tray icon was clicked or the app was launched again.
    Show,
    /// «Закрыть Drxwnify» in the tray menu.
    Quit,
    /// A second launch passed something along (a `drxwnify://` link, a file).
    Open(String),
    /// A global hotkey (its id from `set_hotkeys`) was pressed — anywhere in Windows, even in a game.
    Hotkey(u32),
    /// Another program holds this hotkey's combination; it was not registered.
    HotkeyBusy(u32),
}

/// A system-wide key combination (PC-001). `key` is a Windows virtual-key code (letters/digits = ASCII upper case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub id: u32,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: u32,
}

/// What plays now, for the system media controls and the tray tooltip.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// An http(s) URL of the cover; Windows downloads it itself.
    pub cover_url: Option<String>,
    pub playing: bool,
    pub duration_s: Option<f64>,
    pub position_s: f64,
}

pub type EventSink = std::sync::Arc<dyn Fn(PlatformEvent) + Send + Sync>;

/// The running integration. Dropping it removes the tray icon and the media controls.
pub struct Platform {
    #[cfg(windows)]
    inner: windows_impl::Handle,
}

impl Platform {
    /// Starts the integration thread. `tooltip` is the tray icon's name. None when the OS part could not start
    /// (the app works without it).
    pub fn start(tooltip: &str, events: EventSink) -> Option<Platform> {
        #[cfg(windows)]
        {
            windows_impl::Handle::start(tooltip, events).map(|inner| Platform { inner })
        }
        #[cfg(not(windows))]
        {
            let _ = (tooltip, events);
            None
        }
    }

    /// Registers these global hotkeys instead of the previous ones (empty = none).
    pub fn set_hotkeys(&self, keys: Vec<Hotkey>) {
        #[cfg(windows)]
        self.inner.set_hotkeys(keys);
        #[cfg(not(windows))]
        let _ = keys;
    }

    /// Shows what plays now (title, artist, cover, play state, timeline).
    pub fn set_media(&self, info: MediaInfo) {
        #[cfg(windows)]
        self.inner.set_media(info);
        #[cfg(not(windows))]
        let _ = info;
    }
}

/// One instance per `name` (e.g. per profile folder).
pub enum Instance {
    /// This is the only one; later launches reach it through `InstanceGuard::listen`.
    First(InstanceGuard),
    /// Another one runs and was told to show itself (and given `pass`); this process should exit.
    Second,
}

pub struct InstanceGuard {
    #[cfg(windows)]
    pipe: String,
    /// The first pipe instance, created when claiming (as a raw handle; 0 = none).
    #[cfg(windows)]
    first: isize,
}

impl InstanceGuard {
    /// Delivers later launches to `events` as `Show` / `Open` (a background thread waits on the pipe).
    pub fn listen(&self, events: EventSink) {
        #[cfg(windows)]
        windows_impl::listen_pipe(self.pipe.clone(), self.first, events);
        #[cfg(not(windows))]
        let _ = events;
    }
}

/// Becomes the single instance for `name`, or hands `pass` (empty = just «show») to the one already running.
pub fn single_instance(name: &str, pass: &str) -> Instance {
    #[cfg(windows)]
    {
        windows_impl::single_instance(name, pass)
    }
    #[cfg(not(windows))]
    {
        let _ = (name, pass);
        Instance::First(InstanceGuard {})
    }
}
