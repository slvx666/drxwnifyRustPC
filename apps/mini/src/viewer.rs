//! The picture viewer (Android CoverViewer / PhotoViewerOverlay): a page's cover, the playing track's cover or a
//! friend's avatar big over the window; Esc, a click beside it or ✕ close it, «Скачать» saves the largest
//! original into «Загрузки» (CoverSaver.save).

use crate::MainWindow;
use drx_app::App;
use slint::ComponentHandle;
use std::cell::RefCell;

/// Decoded size of the big picture (the viewer draws it at most 1000 px).
const VIEW_SIZE: u32 = 1000;

thread_local! {
    /// What is open: (address, name for the file).
    static SHOWN: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

pub fn wire(ui: &MainWindow, app: &App) {
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_view_picture(move |kind| {
            let Some(ui) = ui_w.upgrade() else { return };
            let (url, title) = match kind.as_str() {
                "page" => (
                    ui.get_view_cover_url().to_string(),
                    crate::nav::current().map(|p| p.title.to_string()).unwrap_or_default(),
                ),
                "now" => {
                    let (t, a) = (ui.get_now_title().to_string(), ui.get_now_artists().to_string());
                    (ui.get_now_cover_url().to_string(), if a.is_empty() { t } else { format!("{a} — {t}") })
                }
                "friend" => crate::friends_view::shown_avatar().unwrap_or_default(),
                _ => return,
            };
            if url.is_empty() {
                return;
            }
            // A small picture (a friend's avatar is a few hundred px) is not blown up past 1.5× its size.
            let max = drx_core::images::local_side(&url).map_or(1000.0, |s| (s as f32 * 1.5).clamp(320.0, 1000.0));
            ui.set_viewer_max(max);
            ui.set_viewer_title(title.clone().into());
            ui.set_viewer_has_image(false);
            ui.set_viewer_saving(false);
            SHOWN.with(|s| *s.borrow_mut() = Some((url, title)));
            ui.set_viewer_open(true);
            refresh(&ui, &app2);
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_viewer_closed(move || {
            SHOWN.with(|s| *s.borrow_mut() = None);
            if let Some(ui) = ui_w.upgrade() {
                ui.set_viewer_has_image(false);
                ui.set_viewer_image(slint::Image::default());
            }
        });
    }
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_viewer_download(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let Some((url, name)) = SHOWN.with(|s| s.borrow().clone()) else { return };
            let Some(dir) = drx_platform::downloads_dir() else {
                ui.set_toast("Не нашлась папка «Загрузки»".into());
                return;
            };
            ui.set_viewer_saving(true);
            let (app3, ui_w2) = (app2.clone(), ui_w.clone());
            app2.runtime().spawn(async move {
                let got = app3.save_image(&url, &name, &dir).await;
                let _ = ui_w2.upgrade_in_event_loop(move |ui| {
                    ui.set_viewer_saving(false);
                    ui.set_toast(
                        match got {
                            Ok(path) => format!(
                                "Сохранено в «Загрузки»: {}",
                                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                            ),
                            Err(e) => format!("Не сохранилось: {e}"),
                        }
                        .into(),
                    );
                });
            });
        });
    }
}

/// Shows the big picture once it is decoded: the largest variant, or the address itself when the large one
/// does not load (YouTube has no maxres thumbnail for some videos). Called again when images arrive.
pub fn refresh(ui: &MainWindow, app: &App) {
    let Some((url, _)) = SHOWN.with(|s| s.borrow().clone()) else { return };
    // Already showing it (the pixels were taken out of the core's cache, so do not ask for them again).
    if ui.get_viewer_has_image() {
        return;
    }
    let big = drx_core::images::largest(&url);
    let pick = app.take_image(&big, VIEW_SIZE).or_else(|| app.take_image(&url, VIEW_SIZE));
    match pick {
        Some(p) => {
            let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&p.rgba, p.size, p.size);
            ui.set_viewer_image(slint::Image::from_rgba8(buf));
            ui.set_viewer_has_image(true);
        }
        None => {
            app.request_image(&big, VIEW_SIZE);
            if big != url {
                app.request_image(&url, VIEW_SIZE);
            }
        }
    }
}
