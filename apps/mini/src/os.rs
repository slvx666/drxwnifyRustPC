//! Mini ↔ the OS (drx-platform): one instance per profile, system media controls with the media keys, tray.

use crate::MainWindow;
use drx_app::App;
use drx_platform::{EventSink, InstanceGuard, MediaInfo, Platform, PlatformEvent};
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use std::rc::Rc;
use std::sync::Arc;

/// Setting: global hotkeys on (default, as in pc/) or off.
pub const HOTKEYS_KEY: &str = "pcGlobalHotkeys";

pub fn hotkeys_on(app: &App) -> bool {
    app.pref_bool(HOTKEYS_KEY).unwrap_or(true)
}

/// Registers or drops the global hotkeys per the setting. Test runs (a test copy of the profile) never grab the
/// owner's keys unless DRX_HOTKEYS is set (as pc/ does with DRXW_HOTKEYS).
pub fn apply_hotkeys(platform: &Platform, app: &App) {
    let test_run = drx_core::paths::test_run("DRX_HOTKEYS");
    crate::hotkeys::clear_taken();
    platform.set_hotkeys(if hotkeys_on(app) && !test_run {
        crate::hotkeys::all(app)
    } else {
        Vec::new()
    });
}

/// The Run value name for start with Windows.
const AUTOSTART_NAME: &str = "Drxwnify Mini";

pub fn autostart_on() -> bool {
    drx_platform::autostart(AUTOSTART_NAME).is_some()
}

/// Start with Windows on or off: this exe, opening minimised.
pub fn set_autostart(on: bool) -> Result<(), String> {
    let cmd = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .display()
        .to_string();
    let line = format!("\"{cmd}\" --minimized");
    drx_platform::set_autostart(AUTOSTART_NAME, on.then_some(line.as_str()))
}

/// Started by Windows at sign-in: the window opens minimised.
pub fn started_minimized() -> bool {
    std::env::args().any(|a| a == "--minimized")
}

/// One instance per profile folder: test profiles (DRX_PROFILE) never collide with the real one.
pub fn instance_name(profile_root: &std::path::Path) -> String {
    let h = profile_root
        .to_string_lossy()
        .to_lowercase()
        .bytes()
        .fold(1469598103934665603u64, |h, b| {
            (h ^ b as u64).wrapping_mul(1099511628211)
        });
    format!("mini-{h:016x}")
}

/// Makes `drxwnify://` links (invites, shared music; the site's «Открыть в приложении») open this Mini. Test runs
/// (a test copy of the profile) leave the registration alone unless DRX_SCHEME is set.
pub fn register_scheme() {
    if drx_core::paths::test_run("DRX_SCHEME") {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    // A test run keeps its profile for the links it opens.
    let profile = std::env::var("DRX_PROFILE").map(|p| format!(" --profile \"{p}\"")).unwrap_or_default();
    let command = format!("\"{}\"{profile} \"%1\"", exe.display());
    if drx_platform::url_scheme_command("drxwnify").as_deref() != Some(command.as_str())
        && let Err(e) = drx_platform::register_url_scheme("drxwnify", "URL:Drxwnify", &command)
    {
        tracing::warn!("drxwnify:// not registered: {e}");
    }
}

/// What a second launch passes along: a `drxwnify://` link, if any.
pub fn launch_argument() -> String {
    std::env::args()
        .skip(1)
        .find(|a| {
            a.starts_with("drxwnify://")
                || a.contains("drxwnify.pages.dev/")
                || a.contains("open.spotify.com/")
                || a.starts_with(drx_together::LINK_PREFIX)
        })
        .unwrap_or_default()
}

/// Brings the window up from the taskbar or behind other windows.
pub fn show_window(ui: &MainWindow) {
    let _ = ui.show();
    ui.window().with_winit_window(|w| {
        w.set_minimized(false);
        w.focus_window();
    });
}

/// What a hotkey does (pressed system-wide or in the window).
pub fn perform(ui: &MainWindow, app: &App, id: u32) {
    use crate::hotkeys::*;
    match id {
        TOGGLE => app.toggle_play(),
        NEXT => app.next_track(),
        PREV => app.previous_track(),
        LIKE => crate::likes_ui::toggle_now(ui, app),
        RADIO => crate::track_menu::radio(ui, app, None),
        _ => {}
    }
}

fn handle(ui: &MainWindow, app: &App, e: PlatformEvent) {
    let playing = app.now_playing().borrow().playing;
    match e {
        PlatformEvent::Play if !playing => app.toggle_play(),
        PlatformEvent::Pause | PlatformEvent::Stop if playing => app.toggle_play(),
        PlatformEvent::TogglePlay => app.toggle_play(),
        PlatformEvent::Next => app.next_track(),
        PlatformEvent::Previous => app.previous_track(),
        PlatformEvent::Seek(s) => app.seek(s),
        PlatformEvent::Show => show_window(ui),
        PlatformEvent::Quit => {
            let _ = slint::quit_event_loop();
        }
        PlatformEvent::Open(link) => {
            show_window(ui);
            ui.invoke_open_link(link.into());
        }
        PlatformEvent::Hotkey(id) => perform(ui, app, id),
        PlatformEvent::HotkeyBusy(id) => {
            crate::hotkeys::mark_taken(id);
            let name = crate::hotkeys::combo_of(app, id)
                .map(|h| crate::hotkeys::label(&h))
                .unwrap_or_default();
            ui.set_toast(format!("{name} занято другой программой — выберите другое в настройках").into());
        }
        _ => {}
    }
}

/// Starts the media controls and the tray, takes later launches from `guard`, and keeps the OS informed about
/// what plays. The returned handle must live as long as the window.
pub fn start(ui: &MainWindow, app: &App, guard: Option<&InstanceGuard>) -> Option<Arc<Platform>> {
    let (ui_w, app2) = (ui.as_weak(), app.clone());
    let sink: EventSink = Arc::new(move |e: PlatformEvent| {
        let (ui_w, app2) = (ui_w.clone(), app2.clone());
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_w.upgrade() {
                handle(&ui, &app2, e);
            }
        });
    });
    if let Some(g) = guard {
        g.listen(sink.clone());
    }
    let platform = Arc::new(Platform::start("Drxwnify", sink)?);
    apply_hotkeys(&platform, app);

    // Now playing → SMTC and the tray tooltip: on every change (track, play state), and every 5 s while playing
    // so the system timeline follows seeks. Nothing runs while paused.
    let (p, app3) = (platform.clone(), app.clone());
    let mut rx = app.now_playing();
    app.runtime().spawn(async move {
        let mut last_id = String::new();
        loop {
            let playing = rx.borrow().playing;
            if playing {
                tokio::select! {
                    r = rx.changed() => if r.is_err() { break },
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                }
            } else if rx.changed().await.is_err() {
                break;
            }
            let now = rx.borrow().clone();
            let clock = app3.clock();
            let info = match &now.track {
                Some(t) => {
                    // A new track: the clock may still hold the previous one's position.
                    let fresh = t.id != last_id;
                    last_id = t.id.clone();
                    MediaInfo {
                        title: t.title.clone(),
                        artist: t.artist_names(),
                        album: t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                        cover_url: t
                            .album
                            .as_ref()
                            .and_then(|a| drx_catalog::model::best_image(&a.images, 300))
                            .map(|i| i.url.clone()),
                        playing: now.playing,
                        duration_s: (t.duration_ms > 0)
                            .then(|| t.duration_ms as f64 / 1000.0)
                            .or(clock.duration_s),
                        position_s: if fresh { 0.0 } else { clock.position_s },
                    }
                }
                None => MediaInfo::default(),
            };
            p.set_media(info);
        }
    });
    Some(platform)
}

/// The app's own title bar (the window has no system frame): dragging, double press to maximise, the window
/// buttons, and the maximised state for the restore glyph.
pub fn wire_title_bar(ui: &MainWindow) {
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    let last_press: Rc<Cell<Option<Instant>>> = Rc::default();
    let ui_w = ui.as_weak();
    ui.on_window_press(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        let now = Instant::now();
        // A second press within the system's double-click time: maximise or restore, as a title bar does.
        let double = last_press
            .replace(Some(now))
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(drx_platform::double_click_ms()));
        if double {
            last_press.set(None);
            toggle_maximized(&ui);
        }
    });
    let ui_w = ui.as_weak();
    ui.on_window_drag(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        ui.window().with_winit_window(|w| {
            use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = w.window_handle()
                && let RawWindowHandle::Win32(h) = h.as_raw()
            {
                drx_platform::start_window_move(h.hwnd.get());
            } else {
                let _ = w.drag_window();
            }
        });
    });
    let ui_w = ui.as_weak();
    ui.on_window_minimize(move || {
        if let Some(ui) = ui_w.upgrade() {
            ui.window().set_minimized(true);
        }
    });
    let ui_w = ui.as_weak();
    ui.on_window_maximize(move || {
        if let Some(ui) = ui_w.upgrade() {
            toggle_maximized(&ui);
        }
    });
    let ui_w = ui.as_weak();
    ui.on_window_close(move || {
        // As the system close button did (pc/Main.kt): the app quits, it does not go to the tray.
        if let Some(ui) = ui_w.upgrade() {
            let _ = ui.hide();
        }
        let _ = slint::quit_event_loop();
    });
    // The restore glyph follows the real state (Win+Up, Aero Snap, double press).
    let ui_w = ui.as_weak();
    ui.window().on_winit_window_event(move |_, e| {
        if let slint::winit_030::winit::event::WindowEvent::Resized(_) = e
            && let Some(ui) = ui_w.upgrade()
        {
            let max = ui.window().is_maximized();
            if ui.get_window_maximized() != max {
                ui.set_window_maximized(max);
            }
            ui.set_window_inset(maximized_overhang(&ui));
        }
        slint::winit_030::EventResult::Propagate
    });
}

/// How far a maximised window reaches past its monitor (logical px): Windows keeps the resize border of a
/// frameless window outside the screen. 0 when not maximised.
fn maximized_overhang(ui: &MainWindow) -> f32 {
    if !ui.window().is_maximized() {
        return 0.0;
    }
    ui.window()
        .with_winit_window(|w| {
            let m = w.current_monitor()?;
            let outer = w.outer_position().ok()?;
            let px = (m.position().x - outer.x).max(m.position().y - outer.y).max(0);
            Some(px as f32 / w.scale_factor() as f32)
        })
        .flatten()
        .unwrap_or(0.0)
}

fn toggle_maximized(ui: &MainWindow) {
    // The look (corners, insets, resize border) follows in the Resized handler, together with the new size: set
    // here, one frame was drawn at the old size in the new look (the owner saw a «full screen» frame flash).
    ui.window().set_maximized(!ui.window().is_maximized());
}
