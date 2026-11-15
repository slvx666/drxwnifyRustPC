//! Own playlists in Mini: «+» in the library makes one and opens it, the playlist page renames and deletes it,
//! the track menu puts a track into one («Добавить в плейлист…», pc/ Dialogs) and, on an own playlist's page,
//! moves a row up or down or takes it out.

use crate::{MainWindow, PlaylistPick};
use drx_app::App;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;

thread_local! {
    /// The own playlist the open page shows (id, name).
    static OPEN: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    /// The tracks «Добавить в плейлист» is for.
    static PENDING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub fn wire(ui: &MainWindow, app: &App) {
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_create_playlist(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        match app2.runtime().block_on(app2.create_playlist("")) {
            Ok(id) => {
                ui.invoke_library_panel_changed();
                let name = playlist_name(&app2, &id);
                crate::nav::go_playlist(&ui, &app2, &id, &name);
            }
            Err(e) => ui.set_toast(format!("Не удалось создать плейлист: {e}").into()),
        }
    });
    let ui_w = ui.as_weak();
    ui.on_rename_view(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        let Some((_, name)) = OPEN.with(|o| o.borrow().clone()) else { return };
        ui.set_dialog_title("Переименовать плейлист".into());
        ui.set_dialog_subtitle("".into());
        ui.set_dialog_text(name.into());
        ui.set_dialog_kind("rename".into());
    });
    let ui_w = ui.as_weak();
    ui.on_delete_view(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        let Some((_, name)) = OPEN.with(|o| o.borrow().clone()) else { return };
        ui.set_dialog_title("Удалить плейлист?".into());
        ui.set_dialog_subtitle(format!("«{name}» исчезнет, треки останутся в медиатеке").into());
        ui.set_dialog_kind("delete".into());
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_dialog_confirm(move |text| {
        let Some(ui) = ui_w.upgrade() else { return };
        confirm(&ui, &app2, text.to_string());
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_dialog_pick(move |id| {
        let Some(ui) = ui_w.upgrade() else { return };
        let name = ui
            .get_dialog_items()
            .iter()
            .find(|p| p.id == id)
            .map(|p| (p.name.to_string(), p.has_all));
        if let Some((name, true)) = &name {
            ui.set_toast(format!("Уже в «{name}»").into());
            return;
        }
        add_pending(&ui, &app2, id.to_string(), name.map(|n| n.0).unwrap_or_default());
    });
}

fn playlist_name(app: &App, id: &str) -> String {
    app.runtime()
        .block_on(app.playlist_choices(Vec::new()))
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.name)
        .unwrap_or_default()
}

/// A page opened: an own playlist gets rename and delete.
pub fn page_opened(ui: &MainWindow, spec: Option<&drx_app::ListSpec>, title: &str) {
    let open = match spec {
        Some(drx_app::ListSpec::Playlist { id }) if drx_app::playlists::is_editable_id(id) => {
            Some((id.clone(), title.to_string()))
        }
        _ => None,
    };
    ui.set_view_can_edit(open.is_some());
    OPEN.with(|o| *o.borrow_mut() = open);
}

/// The own playlist the open page shows.
pub fn open_playlist() -> Option<String> {
    OPEN.with(|o| o.borrow().as_ref().map(|(id, _)| id.clone()))
}

/// «Добавить в плейлист…» for a track: the dialog with the listener's playlists.
pub fn ask_add(ui: &MainWindow, app: &App, track_id: String) {
    let Some(t) = app.runtime().block_on(app.known_track(&track_id)) else {
        return;
    };
    PENDING.with(|p| *p.borrow_mut() = vec![track_id.clone()]);
    ui.set_dialog_title("Добавить в плейлист".into());
    ui.set_dialog_subtitle(format!("{} — {}", t.artist_names(), t.title).into());
    ui.set_dialog_text("".into());
    ui.set_dialog_items(ModelRc::new(VecModel::from(Vec::<PlaylistPick>::new())));
    ui.set_dialog_kind("add".into());
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let choices = app2.playlist_choices(vec![track_id]).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            if ui.get_dialog_kind() != "add" {
                return;
            }
            let items: Vec<PlaylistPick> = choices
                .into_iter()
                .map(|c| {
                    let cover = c.cover.as_deref().and_then(|u| {
                        crate::cover_r(&app2, u, crate::ROW_COVER, crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 40.0))
                    });
                    PlaylistPick {
                        id: c.id.into(),
                        name: c.name.into(),
                        count: crate::nav::tracks_word(c.count.max(0) as usize).into(),
                        has_cover: cover.is_some(),
                        cover: cover.unwrap_or_default(),
                        has_all: c.has_all,
                    }
                })
                .collect();
            ui.set_dialog_items(ModelRc::new(VecModel::from(items)));
        });
    });
}

fn pending_tracks(app: &App) -> Vec<drx_catalog::model::Track> {
    PENDING
        .with(|p| p.borrow().clone())
        .iter()
        .filter_map(|id| app.runtime().block_on(app.known_track(id)))
        .collect()
}

fn add_pending(ui: &MainWindow, app: &App, playlist: String, name: String) {
    let tracks = pending_tracks(app);
    ui.set_dialog_kind("".into());
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let res = app2.add_to_playlist(&playlist, tracks).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            ui.set_toast(
                match res {
                    Ok(0) => format!("Уже в «{name}»"),
                    Ok(_) => format!("Добавлено в «{name}»"),
                    Err(e) => format!("Не добавилось: {e}"),
                }
                .into(),
            );
            ui.invoke_library_panel_changed();
        });
    });
}

fn confirm(ui: &MainWindow, app: &App, text: String) {
    match ui.get_dialog_kind().as_str() {
        "add" => {
            // A new playlist with the tracks in it.
            match app.runtime().block_on(app.create_playlist(&text)) {
                Ok(id) => {
                    let name = playlist_name(app, &id);
                    add_pending(ui, app, id, name.clone());
                    ui.set_toast(format!("Создан «{name}»").into());
                }
                Err(e) => ui.set_toast(format!("Не удалось создать плейлист: {e}").into()),
            }
        }
        "rename" => {
            let Some((id, _)) = OPEN.with(|o| o.borrow().clone()) else { return };
            let name = text.trim().to_string();
            if name.is_empty() {
                return;
            }
            match app.runtime().block_on(app.rename_playlist(&id, &name)) {
                Ok(()) => {
                    ui.set_dialog_kind("".into());
                    ui.set_view_title(name.clone().into());
                    OPEN.with(|o| *o.borrow_mut() = Some((id, name)));
                    ui.invoke_library_panel_changed();
                }
                Err(e) => ui.set_toast(format!("Не переименовалось: {e}").into()),
            }
        }
        "delete" => {
            let Some((id, name)) = OPEN.with(|o| o.borrow().clone()) else { return };
            ui.set_dialog_kind("".into());
            match app.runtime().block_on(app.delete_playlist(&id)) {
                Ok(()) => {
                    ui.invoke_library_panel_changed();
                    ui.set_toast(format!("Плейлист «{name}» удалён").into());
                    crate::nav::go_home(ui, app);
                }
                Err(e) => ui.set_toast(format!("Не удалилось: {e}").into()),
            }
        }
        _ => {}
    }
}

/// Moves the row at `index` of the open own playlist by `delta` (−1 up, +1 down) or takes it out (`delta` 0).
pub fn edit_row(ui: &MainWindow, app: &App, index: i64, delta: i64) {
    let Some(id) = open_playlist() else { return };
    let res = if delta == 0 {
        app.runtime().block_on(app.remove_from_playlist(&id, index))
    } else {
        app.runtime().block_on(app.move_in_playlist(&id, index, index + delta))
    };
    match res {
        Ok(()) => ui.invoke_library_panel_changed(),
        Err(e) => ui.set_toast(format!("Не получилось: {e}").into()),
    }
}
