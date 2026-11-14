//! Windows: SMTC + tray on a hidden window's thread, single instance over a named pipe.

use crate::{EventSink, Hotkey, Instance, InstanceGuard, MediaInfo, PlatformEvent};
use std::cell::RefCell;
use std::io::Write;
use std::sync::mpsc;
use windows::Foundation::{TimeSpan, TypedEventHandler, Uri};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, ReadFile,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::WinRT::{
    ISystemMediaTransportControlsInterop, RO_INIT_SINGLETHREADED, RoInitialize,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, IDI_APPLICATION,
    IMAGE_ICON, LR_DEFAULTCOLOR, LoadIconW, LoadImageW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow,
    TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, Ref, w};

const WM_CMD: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const MENU_SHOW: usize = 1;
const MENU_TOGGLE: usize = 2;
const MENU_NEXT: usize = 3;
const MENU_PREV: usize = 4;
const MENU_QUIT: usize = 9;

enum Cmd {
    Media(MediaInfo),
    Hotkeys(Vec<Hotkey>),
}

const WM_HOTKEY: u32 = 0x0312;

pub struct Handle {
    hwnd: isize,
    tx: mpsc::Sender<Cmd>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

struct State {
    events: EventSink,
    rx: mpsc::Receiver<Cmd>,
    smtc: Option<SystemMediaTransportControls>,
    icon: HICON,
    tooltip: String,
    taskbar_created: u32,
    shown: MediaInfo,
    /// Ids of the hotkeys registered now.
    hotkeys: Vec<u32>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn emit(e: PlatformEvent) {
    // Taken out of the borrow first: the sink may cause more window messages.
    let sink = STATE.with(|s| s.borrow().as_ref().map(|st| st.events.clone()));
    if let Some(sink) = sink {
        sink(e);
    }
}

impl Handle {
    pub fn start(tooltip: &str, events: EventSink) -> Option<Handle> {
        let (tx, rx) = mpsc::channel();
        let (hwnd_tx, hwnd_rx) = mpsc::sync_channel::<Option<isize>>(1);
        let tooltip = tooltip.to_string();
        std::thread::Builder::new()
            .name("platform".into())
            .spawn(move || run(tooltip, events, rx, hwnd_tx))
            .ok()?;
        let hwnd = hwnd_rx.recv().ok().flatten()?;
        Some(Handle { hwnd, tx })
    }

    pub fn set_hotkeys(&self, keys: Vec<Hotkey>) {
        if self.tx.send(Cmd::Hotkeys(keys)).is_ok() {
            unsafe {
                let _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_CMD, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn set_media(&self, info: MediaInfo) {
        if self.tx.send(Cmd::Media(info)).is_ok() {
            unsafe {
                let _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_CMD, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn run(
    tooltip: String,
    events: EventSink,
    rx: mpsc::Receiver<Cmd>,
    hwnd_tx: mpsc::SyncSender<Option<isize>>,
) {
    unsafe {
        let _ = RoInitialize(RO_INIT_SINGLETHREADED);
        let Ok(module) = GetModuleHandleW(None) else {
            let _ = hwnd_tx.send(None);
            return;
        };
        let hinstance = HINSTANCE(module.0);
        let class = w!("DrxwnifyPlatform");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // A real (hidden) top-level window: SMTC is bound to a top-level window, the tray sends its clicks here.
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Drxwnify"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("platform window: {e}");
                let _ = hwnd_tx.send(None);
                return;
            }
        };
        // The exe's own icon (resource 1, embedded at build time), else the generic one.
        let size = GetSystemMetrics(SM_CXSMICON);
        let icon = LoadImageW(
            Some(hinstance),
            PCWSTR(1 as _),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .or_else(|_| LoadIconW(None, IDI_APPLICATION))
        .unwrap_or_default();
        let smtc = start_smtc(hwnd, &events)
            .inspect_err(|e| tracing::warn!("system media controls: {e}"))
            .ok();
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                events,
                rx,
                smtc,
                icon,
                tooltip,
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                shown: MediaInfo::default(),
                hotkeys: Vec::new(),
            })
        });
        tray(hwnd, NIM_ADD);
        let _ = hwnd_tx.send(Some(hwnd.0 as isize));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        STATE.with(|s| *s.borrow_mut() = None);
    }
}

fn start_smtc(
    hwnd: HWND,
    events: &EventSink,
) -> windows::core::Result<SystemMediaTransportControls> {
    let interop = windows::core::factory::<
        SystemMediaTransportControls,
        ISystemMediaTransportControlsInterop,
    >()?;
    let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd)? };
    smtc.SetIsEnabled(true)?;
    smtc.SetIsPlayEnabled(true)?;
    smtc.SetIsPauseEnabled(true)?;
    smtc.SetIsNextEnabled(true)?;
    smtc.SetIsPreviousEnabled(true)?;
    smtc.SetIsStopEnabled(true)?;
    smtc.SetPlaybackStatus(MediaPlaybackStatus::Closed)?;
    let ev = events.clone();
    smtc.ButtonPressed(&TypedEventHandler::new(
        move |_: Ref<SystemMediaTransportControls>,
              args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
            let e = match args.ok()?.Button()? {
                SystemMediaTransportControlsButton::Play => PlatformEvent::Play,
                SystemMediaTransportControlsButton::Pause => PlatformEvent::Pause,
                SystemMediaTransportControlsButton::Next => PlatformEvent::Next,
                SystemMediaTransportControlsButton::Previous => PlatformEvent::Previous,
                SystemMediaTransportControlsButton::Stop => PlatformEvent::Stop,
                _ => return Ok(()),
            };
            ev(e);
            Ok(())
        },
    ))?;
    let ev = events.clone();
    smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(
        move |_: Ref<SystemMediaTransportControls>,
              args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
            let t = args.ok()?.RequestedPlaybackPosition()?;
            ev(PlatformEvent::Seek(t.Duration as f64 / 10_000_000.0));
            Ok(())
        },
    ))?;
    Ok(smtc)
}

fn span(s: f64) -> TimeSpan {
    TimeSpan {
        Duration: (s.max(0.0) * 10_000_000.0) as i64,
    }
}

/// Shows `info` in SMTC; the title/cover part only when it changed (each update makes Windows redraw the flyout).
fn apply_media(
    smtc: &SystemMediaTransportControls,
    info: &MediaInfo,
    shown: &MediaInfo,
) -> windows::core::Result<()> {
    if info.title.is_empty() {
        smtc.SetPlaybackStatus(MediaPlaybackStatus::Closed)?;
        return Ok(());
    }
    smtc.SetPlaybackStatus(if info.playing {
        MediaPlaybackStatus::Playing
    } else {
        MediaPlaybackStatus::Paused
    })?;
    if info.title != shown.title || info.artist != shown.artist || info.cover_url != shown.cover_url
    {
        let u = smtc.DisplayUpdater()?;
        u.SetType(MediaPlaybackType::Music)?;
        let m = u.MusicProperties()?;
        m.SetTitle(&HSTRING::from(info.title.as_str()))?;
        m.SetArtist(&HSTRING::from(info.artist.as_str()))?;
        m.SetAlbumTitle(&HSTRING::from(info.album.as_str()))?;
        match info.cover_url.as_deref().filter(|u| u.starts_with("http")) {
            Some(url) => {
                let uri = Uri::CreateUri(&HSTRING::from(url))?;
                u.SetThumbnail(&RandomAccessStreamReference::CreateFromUri(&uri)?)?;
            }
            None => u.SetThumbnail(None)?,
        }
        u.Update()?;
    }
    if let Some(d) = info.duration_s.filter(|d| *d > 0.0) {
        let tl = SystemMediaTransportControlsTimelineProperties::new()?;
        tl.SetStartTime(span(0.0))?;
        tl.SetMinSeekTime(span(0.0))?;
        tl.SetEndTime(span(d))?;
        tl.SetMaxSeekTime(span(d))?;
        tl.SetPosition(span(info.position_s.min(d)))?;
        smtc.UpdateTimelineProperties(&tl)?;
    }
    Ok(())
}

/// Adds, updates or removes the tray icon (tooltip = app name, plus what plays).
fn tray(hwnd: HWND, op: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE) {
    STATE.with(|s| {
        let s = s.borrow();
        let Some(st) = s.as_ref() else { return };
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: st.icon,
            ..Default::default()
        };
        let tip = if st.shown.title.is_empty() {
            st.tooltip.clone()
        } else {
            format!("{} — {}", st.shown.title, st.shown.artist)
        };
        for (i, c) in tip.encode_utf16().take(nid.szTip.len() - 1).enumerate() {
            nid.szTip[i] = c;
        }
        unsafe {
            let _ = Shell_NotifyIconW(op, &nid);
        }
    });
}

fn menu(hwnd: HWND) {
    let playing = STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.shown.playing));
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let item = |id: usize, text: &str| {
            let t = wide(text);
            let _ = AppendMenuW(m, MF_STRING, id, PCWSTR(t.as_ptr()));
        };
        item(MENU_SHOW, "Показать Drxwnify");
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        item(
            MENU_TOGGLE,
            if playing {
                "Пауза"
            } else {
                "Играть"
            },
        );
        item(MENU_NEXT, "Следующий трек");
        item(MENU_PREV, "Предыдущий трек");
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        item(MENU_QUIT, "Закрыть Drxwnify");
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Without this the menu does not close when clicking elsewhere (documented Win32 quirk).
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            m,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(m);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CMD => {
            let mut changed = false;
            let mut busy: Vec<u32> = Vec::new();
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                let Some(st) = s.as_mut() else { return };
                let mut last = None;
                while let Ok(cmd) = st.rx.try_recv() {
                    match cmd {
                        Cmd::Media(info) => last = Some(info),
                        Cmd::Hotkeys(keys) => busy = register_hotkeys(hwnd, &mut st.hotkeys, &keys),
                    }
                }
                if let Some(info) = last {
                    if let Some(smtc) = &st.smtc
                        && let Err(e) = apply_media(smtc, &info, &st.shown)
                    {
                        tracing::debug!("smtc update: {e}");
                    }
                    changed = info.title != st.shown.title || info.artist != st.shown.artist;
                    st.shown = info;
                }
            });
            if changed {
                tray(hwnd, NIM_MODIFY);
            }
            for id in busy {
                emit(PlatformEvent::HotkeyBusy(id));
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            emit(PlatformEvent::Hotkey(wp.0 as u32));
            LRESULT(0)
        }
        WM_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP => emit(PlatformEvent::Show),
                WM_RBUTTONUP => menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match wp.0 & 0xFFFF {
                MENU_SHOW => emit(PlatformEvent::Show),
                MENU_TOGGLE => emit(PlatformEvent::TogglePlay),
                MENU_NEXT => emit(PlatformEvent::Next),
                MENU_PREV => emit(PlatformEvent::Previous),
                MENU_QUIT => emit(PlatformEvent::Quit),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            tray(hwnd, NIM_DELETE);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m != 0
            && STATE.with(|s| {
                s.borrow()
                    .as_ref()
                    .is_some_and(|st| st.taskbar_created == m)
            }) =>
        {
            // Explorer restarted: the tray icon has to be added again.
            tray(hwnd, NIM_ADD);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Replaces the registered hotkeys; returns the ids another program already holds.
fn register_hotkeys(hwnd: HWND, current: &mut Vec<u32>, keys: &[Hotkey]) -> Vec<u32> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
        UnregisterHotKey,
    };
    for id in current.drain(..) {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), id as i32);
        }
    }
    let mut busy = Vec::new();
    for k in keys {
        let mut mods = MOD_NOREPEAT;
        if k.ctrl {
            mods |= MOD_CONTROL;
        }
        if k.alt {
            mods |= MOD_ALT;
        }
        if k.shift {
            mods |= MOD_SHIFT;
        }
        let ok =
            unsafe { RegisterHotKey(Some(hwnd), k.id as i32, HOT_KEY_MODIFIERS(mods.0), k.key) }
                .is_ok();
        if ok {
            current.push(k.id);
        } else {
            busy.push(k.id);
        }
    }
    busy
}

// ── single instance ──────────────────────────────────────────────────────────────────────────────

fn pipe_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!(r"\\.\pipe\drxwnify-{safe}")
}

fn create_pipe(name: &str, first: bool) -> Option<HANDLE> {
    let w = wide(name);
    let mut mode = PIPE_ACCESS_INBOUND;
    if first {
        mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(w.as_ptr()),
            mode,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            4096,
            0,
            None,
        )
    };
    (!h.is_invalid()).then_some(h)
}

/// Hands `pass` to a running instance; true when one took it.
fn hand_over(pipe: &str, pass: &str) -> bool {
    for _ in 0..5 {
        match std::fs::OpenOptions::new().write(true).open(pipe) {
            Ok(mut f) => {
                let _ = f.write_all(format!("{pass}\n").as_bytes());
                return true;
            }
            // 231 = ERROR_PIPE_BUSY: the running one is reading another launch right now.
            Err(e) if e.raw_os_error() == Some(231) => {
                std::thread::sleep(std::time::Duration::from_millis(150))
            }
            Err(_) => return false,
        }
    }
    false
}

pub fn single_instance(name: &str, pass: &str) -> Instance {
    let pipe = pipe_name(name);
    if hand_over(&pipe, pass) {
        return Instance::Second;
    }
    match create_pipe(&pipe, true) {
        Some(h) => Instance::First(InstanceGuard {
            pipe,
            first: h.0 as isize,
        }),
        // Someone else created it between our two calls: they are the instance.
        None if hand_over(&pipe, pass) => Instance::Second,
        None => Instance::First(InstanceGuard { pipe, first: 0 }),
    }
}

pub fn listen_pipe(pipe: String, first: isize, events: EventSink) {
    let _ = std::thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            let mut next = (first != 0).then_some(HANDLE(first as _));
            loop {
                let Some(h) = next.take().or_else(|| create_pipe(&pipe, false)) else {
                    return;
                };
                unsafe {
                    // Fails with ERROR_PIPE_CONNECTED when the client came first: that is fine.
                    let _ = ConnectNamedPipe(h, None);
                    let mut text = Vec::new();
                    let mut buf = [0u8; 1024];
                    loop {
                        let mut n = 0u32;
                        if ReadFile(h, Some(&mut buf), Some(&mut n), None).is_err() || n == 0 {
                            break;
                        }
                        text.extend_from_slice(&buf[..n as usize]);
                        if text.len() > 64 * 1024 {
                            break;
                        }
                    }
                    let _ = DisconnectNamedPipe(h);
                    let _ = CloseHandle(h);
                    let s = String::from_utf8_lossy(&text).trim().to_string();
                    if !s.is_empty() {
                        events(PlatformEvent::Open(s));
                    }
                    events(PlatformEvent::Show);
                }
            }
        });
}
