//! Hearts in Mini (pc/ Library.toggleLike / toggleSaved): song rows, the player bar and «Сейчас играет», the page
//! heading's «в медиатеку» / «подписаться», and the like hotkey. The heart flips at once; the core writes the app
//! and the accounts behind it, and an account that refused is reported in a toast (the heart stays as set, as
//! on Android).

use crate::MainWindow;
use drx_app::{App, ListSpec};
use drx_catalog::model::Source;
use slint::{ComponentHandle, Model, ModelRc};
use std::cell::RefCell;

/// What the open page's heading heart saves.
#[derive(Clone)]
enum Saveable {
    Album(String),
    Artist(String),
}

thread_local! {
    static PAGE: RefCell<Option<Saveable>> = const { RefCell::new(None) };
}

pub fn wire(ui: &MainWindow, app: &App) {
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_like_song(move |id| {
        if let Some(ui) = ui_w.upgrade() {
            toggle(&ui, &app2, id.to_string());
        }
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_toggle_now_like(move || {
        if let Some(ui) = ui_w.upgrade() {
            toggle_now(&ui, &app2);
        }
    });
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_toggle_view_save(move || {
        if let Some(ui) = ui_w.upgrade() {
            toggle_save(&ui, &app2);
        }
    });
}

/// The heart of the playing track (player buttons and the hotkey).
pub fn toggle_now(ui: &MainWindow, app: &App) {
    let id = ui.get_now_id().to_string();
    if !id.is_empty() {
        toggle(ui, app, id);
    }
}

/// Flips a track's heart everywhere it shows, then lets the core write it.
pub fn toggle(ui: &MainWindow, app: &App, id: String) {
    let liked = !shown_liked(ui, &id);
    apply(ui, &id, liked);
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let res = match app2.known_track(&id).await {
            Some(t) => app2.set_track_liked(&t, liked).await,
            None => Err(drx_app::AppError::Other("трек не найден".into())),
        };
        if let Err(e) = res {
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_w.upgrade() {
                    ui.set_toast(format!("Аккаунт не принял лайк: {e}").into());
                }
            });
        }
    });
}

/// The state the UI shows for `id` now (the player's heart for the playing track, else its row).
fn shown_liked(ui: &MainWindow, id: &str) -> bool {
    if ui.get_now_id() == id {
        return ui.get_now_liked();
    }
    for m in models(ui) {
        if let Some(r) = m.iter().find(|r| r.id == id) {
            return r.liked;
        }
    }
    crate::nav::row_liked(id).unwrap_or(false)
}

fn models(ui: &MainWindow) -> [ModelRc<crate::SongItem>; 3] {
    [ui.get_search_tracks(), ui.get_artist_tracks(), ui.get_stats_tracks()]
}

/// Shows the new state in the player, every list model and the history.
fn apply(ui: &MainWindow, id: &str, liked: bool) {
    if ui.get_now_id() == id {
        ui.set_now_liked(liked);
    }
    for m in models(ui) {
        for i in 0..m.row_count() {
            if let Some(mut r) = m.row_data(i)
                && r.id == id
                && r.liked != liked
            {
                r.liked = liked;
                m.set_row_data(i, r);
            }
        }
    }
    crate::nav::set_row_liked(id, liked);
    crate::history_view::set_liked(id, liked);
}

/// The playing track changed: its heart from the library.
pub fn now_changed(ui: &MainWindow, app: &App, id: String) {
    ui.set_now_can_like(!id.is_empty());
    ui.set_now_liked(false);
    if id.is_empty() {
        return;
    }
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let liked = app2.is_track_liked(&id).await;
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_w.upgrade()
                && ui.get_now_id() == id.as_str()
            {
                ui.set_now_liked(liked);
            }
        });
    });
}

/// A page opened: albums of a linked catalog can be saved, artists followed.
pub fn page_opened(ui: &MainWindow, app: &App, spec: Option<&ListSpec>) {
    let what = match spec {
        Some(ListSpec::CatalogAlbum { id }) if app.has_account(Source::of_id(id)) => {
            Some(Saveable::Album(id.clone()))
        }
        Some(ListSpec::Artist { id }) | Some(ListSpec::CatalogArtist { id }) => {
            Some(Saveable::Artist(id.clone()))
        }
        _ => None,
    };
    PAGE.with(|p| *p.borrow_mut() = what.clone());
    ui.set_view_can_save(what.is_some());
    ui.set_view_saved(false);
    let Some(what) = what else { return };
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    let name = ui.get_view_title().to_string();
    app.runtime().spawn(async move {
        let saved = match &what {
            Saveable::Album(id) => app2.is_album_saved(id).await.unwrap_or(false),
            Saveable::Artist(id) => app2.is_artist_followed(id, &name).await,
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let same = PAGE.with(|p| match (&*p.borrow(), &what) {
                (Some(Saveable::Album(a)), Saveable::Album(b)) => a == b,
                (Some(Saveable::Artist(a)), Saveable::Artist(b)) => a == b,
                _ => false,
            });
            if same {
                ui.set_view_saved(saved);
            }
        });
    });
}

fn toggle_save(ui: &MainWindow, app: &App) {
    let Some(what) = PAGE.with(|p| p.borrow().clone()) else {
        return;
    };
    let saved = !ui.get_view_saved();
    ui.set_view_saved(saved);
    let name = ui.get_view_title().to_string();
    let photo = Some(ui.get_view_cover_url().to_string()).filter(|u| !u.is_empty());
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let res = match &what {
            Saveable::Album(id) => app2.set_album_saved(id, saved).await,
            Saveable::Artist(id) => app2.set_artist_followed(id, &name, photo, saved).await,
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            match res {
                Ok(()) => ui.set_toast(
                    match (&what, saved) {
                        (Saveable::Album(_), true) => "Альбом в медиатеке",
                        (Saveable::Album(_), false) => "Альбом убран из медиатеки",
                        (Saveable::Artist(_), true) => "Вы подписаны",
                        (Saveable::Artist(_), false) => "Подписка отменена",
                    }
                    .into(),
                ),
                Err(e) => {
                    ui.set_view_saved(!saved);
                    ui.set_toast(format!("Не получилось: {e}").into());
                }
            }
        });
    });
}
