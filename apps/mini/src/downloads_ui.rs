//! Downloads in Mini: the top bar's download button opens the list of downloaded tracks, a label beside it shows
//! the queue while it runs; «Скачать» in the track menu and «Скачать все треки» on a page fill the queue.

use crate::MainWindow;
use drx_app::{App, ListSpec};
use slint::ComponentHandle;

pub fn wire(ui: &MainWindow, app: &App) {
    app.load_downloads();
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_open_downloads(move || {
            if let Some(ui) = ui_w.upgrade() {
                crate::nav::go_downloads(&ui, &app2);
            }
        });
    }
    // The queue: «2 · 45%» beside the button; a word when it is done.
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        app.runtime().spawn(async move {
            let mut rx = app2.download_progress();
            let mut was_busy = false;
            loop {
                let p = rx.borrow_and_update().clone();
                let busy = p.queued > 0;
                let label = if busy { format!("{} · {}%", p.queued, (p.fraction * 100.0).round() as i32) } else { String::new() };
                let finished = was_busy && !busy;
                was_busy = busy;
                let _ = ui_w.upgrade_in_event_loop(move |ui| {
                    ui.set_downloads_label(label.into());
                    if finished {
                        ui.set_toast(
                            if p.failed > 0 {
                                format!("Скачано: {}, не скачалось: {}", p.done, p.failed)
                            } else {
                                format!("Скачано: {}", p.done)
                            }
                            .into(),
                        );
                    }
                });
                if rx.changed().await.is_err() {
                    return;
                }
            }
        });
    }
    // A download finished or went: the downloads page (if open) shows the new list.
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let mut rx = app2.downloads_version();
        while rx.changed().await.is_ok() {
            let app3 = app2.clone();
            let _ = ui_w.upgrade_in_event_loop(move |ui| crate::nav::reload_if(&ui, &app3, &ListSpec::Downloads));
        }
    });
}

/// «Скачать» / «Удалить из загрузок» from the track menu.
pub fn act(ui: &MainWindow, app: &App, action: &str, id: &str) {
    if action == "undownload" {
        app.delete_download(id);
        ui.set_toast("Удалено из загрузок".into());
        return;
    }
    let Some(t) = app.runtime().block_on(app.known_track(id)) else { return };
    let title = t.title.clone();
    if app.download_tracks(vec![t]) > 0 {
        ui.set_toast(format!("Скачиваю: {title}").into());
    } else {
        ui.set_toast("Уже скачан или в очереди".into());
    }
}
