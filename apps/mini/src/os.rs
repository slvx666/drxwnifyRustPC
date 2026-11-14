//! Mini ↔ the OS (drx-platform): one instance per profile, system media controls with the media keys, tray.

use crate::MainWindow;
use drx_app::App;
use drx_platform::{EventSink, InstanceGuard, MediaInfo, Platform, PlatformEvent};
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use std::sync::Arc;

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

/// What a second launch passes along: a `drxwnify://` link, if any.
pub fn launch_argument() -> String {
    std::env::args()
        .skip(1)
        .find(|a| a.starts_with("drxwnify://"))
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
        PlatformEvent::Open(link) => tracing::info!("opened with {link}"),
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
