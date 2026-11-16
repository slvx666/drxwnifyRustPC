//! The track menu (right click on a row, pc/ trackMenu) and the radio: «Играть дальше», «Добавить в очередь»,
//! «Радио по треку», the heart, and the artist's and album's pages.

use crate::MainWindow;
use drx_app::App;
use slint::ComponentHandle;

pub fn wire(ui: &MainWindow, app: &App) {
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_menu_opening(move |id, index| {
        let Some(ui) = ui_w.upgrade() else { return };
        // On an own playlist's page, a row of its list can move and be taken out.
        let (row, len) = crate::nav::row_at(index.max(0) as usize);
        let in_playlist = crate::playlists_ui::open_playlist().is_some()
            && index >= 0
            && row.as_deref() == Some(id.as_str());
        ui.set_menu_in_playlist(in_playlist);
        ui.set_menu_can_up(in_playlist && index > 0);
        ui.set_menu_can_down(in_playlist && (index as usize) + 1 < len);
        let t = app2.runtime().block_on(app2.known_track(&id));
        ui.set_menu_liked(crate::likes_ui::shown_liked(&ui, &id));
        ui.set_menu_has_artist(
            t.as_ref()
                .and_then(|t| t.artists.first())
                .is_some_and(|a| !a.id.is_empty()),
        );
        ui.set_menu_has_album(
            t.as_ref()
                .and_then(|t| t.album.as_ref())
                .is_some_and(|a| !a.id.is_empty()),
        );
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_track_menu_action(move |action, id| {
        let Some(ui) = ui_w.upgrade() else { return };
        act(&ui, &app2, action.as_str(), id.to_string());
    });
}

fn act(ui: &MainWindow, app: &App, action: &str, id: String) {
    match action {
        "like" => crate::likes_ui::toggle(ui, app, id),
        "radio" => radio(ui, app, Some(id)),
        "add" => crate::playlists_ui::ask_add(ui, app, id),
        "up" => crate::playlists_ui::edit_row(ui, app, ui.get_menu_index() as i64, -1),
        "down" => crate::playlists_ui::edit_row(ui, app, ui.get_menu_index() as i64, 1),
        "remove" => crate::playlists_ui::edit_row(ui, app, ui.get_menu_index() as i64, 0),
        "next" | "queue" => {
            let Some(t) = app.runtime().block_on(app.known_track(&id)) else {
                return;
            };
            let title = t.title.clone();
            app.enqueue_track(t, action == "next");
            ui.set_toast(
                if action == "next" {
                    format!("Дальше: {title}")
                } else {
                    format!("В очереди: {title}")
                }
                .into(),
            );
        }
        "send" => crate::friends_view::ask_send(ui, app, id.to_string()),
        "suggest" => {
            let Some(t) = app.runtime().block_on(app.known_track(&id)) else {
                return;
            };
            app.room_suggest(&t);
            ui.set_toast("Предложено хосту комнаты".into());
        }
        "share" => {
            let Some(t) = app.runtime().block_on(app.known_track(&id)) else {
                return;
            };
            if t.id.starts_with("local:") {
                ui.set_toast("У файла с компьютера нет ссылки".into());
                return;
            }
            let by = t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
            copy_share(ui, &drx_app::share_text("track", &t.id, &t.title, &by));
        }
        "artist" | "album" => {
            let Some(t) = app.runtime().block_on(app.known_track(&id)) else {
                return;
            };
            if action == "artist" {
                if let Some(a) = t.artists.first().filter(|a| !a.id.is_empty()) {
                    ui.invoke_open_artist(a.id.clone().into());
                }
            } else if let Some(a) = t.album.as_ref().filter(|a| !a.id.is_empty()) {
                ui.invoke_open_album(a.id.clone().into());
            }
        }
        _ => {}
    }
}

/// Puts a shared link on the clipboard and says so.
pub fn copy_share(ui: &MainWindow, text: &str) {
    ui.set_toast(
        if drx_platform::copy_text(text) { "Ссылка скопирована" } else { "Буфер обмена занят — попробуйте ещё раз" }.into(),
    );
}

/// Radio from a track (None: the playing one) — pc/ Player.radioAfterCurrent / radio: the playing track keeps
/// playing and the radio replaces what was queued after it; another track starts and its radio follows.
pub fn radio(ui: &MainWindow, app: &App, id: Option<String>) {
    let now = ui.get_now_id().to_string();
    let id = id.unwrap_or_else(|| now.clone());
    if id.is_empty() {
        return;
    }
    let Some(t) = app.runtime().block_on(app.known_track(&id)) else {
        return;
    };
    ui.set_toast(format!("Собираю радио от «{}»…", t.title).into());
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    let current = id == now;
    app.runtime().spawn(async move {
        let res = if current {
            app2.radio_after_current().await
        } else {
            app2.play_radio(t).await
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            ui.set_toast(
                match res {
                    Ok(0) => "Для радио ничего не нашлось".to_string(),
                    Ok(n) => format!("Радио: {} после этого", crate::nav::tracks_word(n)),
                    Err(e) => format!("Радио не собралось: {e}"),
                }
                .into(),
            );
        });
    });
}
