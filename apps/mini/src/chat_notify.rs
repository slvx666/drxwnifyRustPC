//! New messages while the window is away (Android: message notifications when the app is closed): a Windows
//! notification from the tray icon (a click opens the window); in the window, a toast unless that chat is open.
//! Messages that were there at launch say nothing.

use crate::MainWindow;
use drx_app::App;
use drx_platform::Platform;
use slint::ComponentHandle;
use std::cell::Cell;
use std::sync::Arc;

thread_local! {
    /// The newest incoming message already told about (unix ms).
    static SEEN: Cell<i64> = const { Cell::new(i64::MAX) };
}

/// «Имя» of a friend, as the friends list shows it.
fn name_of(app: &App, peer: &str) -> String {
    let Some(rx) = app.friends_state() else { return "Друг".into() };
    let st = rx.borrow();
    let name = st
        .data
        .get(peer)
        .and_then(|d| d.name.clone())
        .filter(|n| !n.trim().is_empty())
        .or_else(|| st.friends.iter().find(|f| f.pubkey == peer).map(|f| f.name.clone()))
        .unwrap_or_else(|| "Друг".into());
    crate::friends_view::plain(&name)
}

fn body_of(p: &drx_app::chats::ChatPreview) -> String {
    let text: String = drx_social::chat::plain_stickers(&p.text).split_whitespace().collect::<Vec<_>>().join(" ");
    match (&p.music, text.is_empty()) {
        (Some(m), false) => format!("♪ {m} · {text}"),
        (Some(m), true) => format!("♪ {m}"),
        (None, false) => text,
        (None, true) => "стикер".into(),
    }
}

fn check(ui: &MainWindow, app: &App, platform: Option<&Platform>) {
    let previews = app.chat_previews();
    let newest = previews.iter().filter(|p| !p.mine).map(|p| p.ts).max().unwrap_or(0);
    let seen = SEEN.with(Cell::get);
    if seen == i64::MAX {
        // The first look: what is there now is old news — and so is what the first sync brings in later (a new
        // profile fetches 30 days): only messages from about now on count.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        SEEN.with(|s| s.set(newest.max(now - 120_000)));
        return;
    }
    let fresh: Vec<&drx_app::chats::ChatPreview> =
        previews.iter().filter(|p| !p.mine && p.unread > 0 && p.ts > seen).collect();
    SEEN.with(|s| s.set(newest.max(seen)));
    if fresh.is_empty() {
        return;
    }
    use slint::winit_030::WinitWindowAccessor;
    let focused = ui.window().with_winit_window(|w| w.has_focus()).unwrap_or(false);
    for p in fresh {
        let open_here = ui.get_view_friends() && ui.get_friend_page() && crate::friends_view::open_peer().as_deref() == Some(p.peer.as_str());
        if focused && open_here {
            continue;
        }
        let (name, body) = (name_of(app, &p.peer), body_of(p));
        if focused {
            ui.set_toast(format!("{name}: {body}").into());
        } else if let Some(pl) = platform {
            pl.notify(&name, &body);
        }
    }
}

/// Follows the chats from a little after launch.
pub fn start(ui: &MainWindow, app: &App, platform: Option<Arc<Platform>>) {
    if app.friends().is_none() {
        return;
    }
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    slint::Timer::single_shot(std::time::Duration::from_secs(4), move || {
        app2.start_chats_live();
        let Some(mut rx) = app2.chats_version() else { return };
        if let Some(ui) = ui_w.upgrade() {
            check(&ui, &app2, platform.as_deref());
        }
        let (app3, ui_w2) = (app2.clone(), ui_w.clone());
        app2.runtime().spawn(async move {
            while rx.changed().await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                let (app4, ui_w3, pl) = (app3.clone(), ui_w2.clone(), platform.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_w3.upgrade() {
                        check(&ui, &app4, pl.as_deref());
                    }
                });
            }
        });
    });
}
