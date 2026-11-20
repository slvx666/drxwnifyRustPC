//! The queue panel (Spotify's queue in the right sidebar, PLAY-003): what plays and everything after it; a click
//! plays from there, up / down move a track, × takes it out, «Очистить» empties what comes next.

use crate::{MainWindow, SongItem};
use drx_app::App;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;

thread_local! {
    /// The tracks of the rows on screen (covers are filled in place as they arrive).
    static SHOWN: RefCell<Vec<drx_catalog::model::Track>> = const { RefCell::new(Vec::new()) };
}

/// Rows drawn at most (the liked list can be thousands long).
const MAX_ROWS: usize = 200;

fn row(app: &App, t: &drx_catalog::model::Track, i: usize) -> SongItem {
    let cover = t
        .album
        .as_ref()
        .and_then(|a| drx_catalog::model::best_image(&a.images, 120))
        .and_then(|img| {
            crate::cover_r(
                app,
                &img.url,
                80,
                crate::theme_r(crate::ROW_COVER_R, 80, 40.0),
            )
        });
    SongItem {
        id: t.id.clone().into(),
        title: t.title.clone().into(),
        artists: t.artist_names().into(),
        duration: crate::fmt_time(t.duration_ms as f64 / 1000.0).into(),
        liked: false,
        index: i as i32 + 1,
        has_cover: cover.is_some(),
        cover: cover.unwrap_or_default(),
        artist_id: t
            .artists
            .first()
            .map(|a| a.id.clone())
            .unwrap_or_default()
            .into(),
        album_id: t
            .album
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_default()
            .into(),
        plays: "".into(),
    }
}

pub fn refresh(ui: &MainWindow, app: &App) {
    if !ui.get_queue_open() {
        return;
    }
    let (tracks, cur) = app.queue();
    match tracks.get(cur) {
        Some(t) => {
            ui.set_queue_now(row(app, t, cur));
            ui.set_queue_has_now(true);
        }
        None => ui.set_queue_has_now(false),
    }
    let shown: Vec<drx_catalog::model::Track> = tracks
        .iter()
        .skip(cur + 1)
        .take(MAX_ROWS)
        .cloned()
        .collect();
    let next: Vec<SongItem> = shown
        .iter()
        .enumerate()
        .map(|(i, t)| row(app, t, cur + 1 + i))
        .collect();
    SHOWN.with(|s| *s.borrow_mut() = shown);
    let more = tracks.len().saturating_sub(cur + 1 + MAX_ROWS);
    ui.set_queue_items(ModelRc::new(VecModel::from(next)));
    ui.set_queue_more(more as i32);
}

/// Covers that arrived: only those rows change (a new model would scroll the list back to the top).
pub fn refresh_covers(ui: &MainWindow, app: &App) {
    if !ui.get_queue_open() {
        return;
    }
    let model = ui.get_queue_items();
    SHOWN.with(|s| {
        let s = s.borrow();
        if s.len() != model.row_count() {
            return;
        }
        for (i, t) in s.iter().enumerate() {
            let Some(mut r) = model.row_data(i) else {
                continue;
            };
            if r.has_cover {
                continue;
            }
            let fresh = row(app, t, r.index as usize - 1);
            if fresh.has_cover {
                r.cover = fresh.cover;
                r.has_cover = true;
                model.set_row_data(i, r);
            }
        }
    });
    let mut now = ui.get_queue_now();
    if ui.get_queue_has_now() && !now.has_cover {
        let (tracks, cur) = app.queue();
        if let Some(t) = tracks.get(cur) {
            let fresh = row(app, t, cur);
            if fresh.has_cover {
                now.cover = fresh.cover;
                now.has_cover = true;
                ui.set_queue_now(now);
            }
        }
    }
}

pub fn wire(ui: &MainWindow, app: &App) {
    let abs = |app: &App, i: i32| -> Option<usize> {
        let (_, cur) = app.queue();
        usize::try_from(i).ok().map(|i| cur + 1 + i)
    };
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_queue_toggled(move || {
            if let Some(ui) = ui_w.upgrade() {
                refresh(&ui, &app2);
            }
        });
    }
    {
        let app2 = app.clone();
        ui.on_queue_play(move |i| {
            if let Some(i) = abs(&app2, i) {
                app2.queue_play(i);
            }
        });
    }
    {
        let app2 = app.clone();
        ui.on_queue_remove(move |i| {
            if let Some(i) = abs(&app2, i) {
                app2.queue_remove(i);
            }
        });
    }
    {
        let app2 = app.clone();
        ui.on_queue_move(move |a, b| {
            if let (Some(a), Some(b)) = (abs(&app2, a), abs(&app2, b)) {
                app2.queue_move(a, b);
            }
        });
    }
    {
        let app2 = app.clone();
        ui.on_queue_clear(move || app2.queue_clear_upcoming());
    }
    // Redraw when the queue or the playing index changes.
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let mut rx = app2.now_playing();
        let mut last = (u64::MAX, None);
        loop {
            let key = {
                let n = rx.borrow_and_update();
                (n.queue_rev, n.index)
            };
            if key != last {
                last = key;
                let app3 = app2.clone();
                let _ = ui_w.upgrade_in_event_loop(move |ui| refresh(&ui, &app3));
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
}
