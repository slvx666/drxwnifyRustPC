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
#[cfg(windows)]
pub mod setup;

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

    /// A notification (new messages): the tray icon's, a toast on Windows 10/11; a click shows the window.
    pub fn notify(&self, title: &str, text: &str) {
        #[cfg(windows)]
        self.inner.notify(title, text);
        #[cfg(not(windows))]
        let _ = (title, text);
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

/// Start with Windows: the command registered for `name` (None = off).
pub fn autostart(name: &str) -> Option<String> {
    #[cfg(windows)]
    {
        windows_impl::reg_get(windows_impl::RUN_KEY, name)
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        None
    }
}

/// Turns start with Windows on (with this command line) or off.
pub fn set_autostart(name: &str, command: Option<&str>) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_impl::reg_set(windows_impl::RUN_KEY, name, command)
    }
    #[cfg(not(windows))]
    {
        let _ = (name, command);
        Err("not supported yet".into())
    }
}

/// The command a `scheme://` link starts (Windows: HKCU\Software\Classes\<scheme>\shell\open\command).
pub fn url_scheme_command(scheme: &str) -> Option<String> {
    #[cfg(windows)]
    {
        windows_impl::reg_get(&format!(r"Software\Classes\{scheme}\shell\open\command"), "")
    }
    #[cfg(not(windows))]
    {
        let _ = scheme;
        None
    }
}

/// Makes `scheme://` links start `command` (the per-user registration Windows offers without admin rights).
pub fn register_url_scheme(scheme: &str, title: &str, command: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let base = format!(r"Software\Classes\{scheme}");
        windows_impl::reg_set(&base, "", Some(title))?;
        windows_impl::reg_set(&base, "URL Protocol", Some(""))?;
        windows_impl::reg_set(&format!(r"{base}\shell\open\command"), "", Some(command))
    }
    #[cfg(not(windows))]
    {
        let _ = (scheme, title, command);
        Err("not supported yet".into())
    }
}

/// The local time's offset from UTC in seconds, daylight saving included (history times, day groups).
pub fn utc_offset_seconds() -> i64 {
    #[cfg(windows)]
    {
        windows_impl::utc_offset_seconds()
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// Starts moving the window by its own title bar (left button down): the system's move loop with Aero Snap.
pub fn start_window_move(hwnd: isize) {
    #[cfg(windows)]
    windows_impl::start_window_move(hwnd);
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// The system's double-click time in ms (500 by default).
pub fn double_click_ms() -> u64 {
    #[cfg(windows)]
    {
        windows_impl::double_click_ms()
    }
    #[cfg(not(windows))]
    {
        500
    }
}

/// The system «Open» dialog for one file: `kinds` names the filter («Плейлисты»), `patterns` its masks
/// («*.csv;*.m3u;*.m3u8»). None when cancelled.
pub fn pick_file(title: &str, kinds: &str, patterns: &str) -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        };
        use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
        use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
        use windows::core::{HSTRING, PCWSTR};
        // SAFETY: a modal COM dialog on this thread; strings outlive the calls that read them.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let (name, spec) = (HSTRING::from(kinds), HSTRING::from(patterns));
            let filters = [COMDLG_FILTERSPEC { pszName: PCWSTR(name.as_ptr()), pszSpec: PCWSTR(spec.as_ptr()) }];
            d.SetFileTypes(&filters).ok()?;
            d.SetTitle(&HSTRING::from(title)).ok()?;
            d.Show(None).ok()?;
            let item = d.GetResult().ok()?;
            let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s.map(std::path::PathBuf::from)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (title, kinds, patterns);
        None
    }
}

/// Opens a web address in the default browser (links in chat messages). Only http(s) addresses.
pub fn open_url(url: &str) -> bool {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return false;
    }
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::HSTRING;
        let r = unsafe { ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL) };
        r.0 as isize > 32
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn().is_ok()
    }
}

/// Puts text on the clipboard (invite links, codes). False when the clipboard was busy.
pub fn copy_text(text: &str) -> bool {
    #[cfg(windows)]
    {
        windows_impl::copy_text(text)
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        false
    }
}

/// The user's «Загрузки» folder (saved covers), wherever they moved it.
pub fn downloads_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        windows_impl::downloads_dir()
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Downloads"))
    }
}
