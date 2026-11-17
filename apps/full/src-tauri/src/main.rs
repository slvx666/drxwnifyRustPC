// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Drxwnify Full: Tauri shell. Commands here are thin wrappers over `drx-app`; no logic of their own.

use drx_app::{App, EditionProfile};
use drx_core::paths::Profile;
use drx_core::app_icons;
use serde::Serialize;
use tauri::Manager;

/// Full's icon files (assets/icons/full), in app_icons::FULL order.
const ICON_PNGS: &[(&str, &[u8])] = &[
    ("halo", include_bytes!("../../../../assets/icons/full/halo.png")),
    ("clean", include_bytes!("../../../../assets/icons/full/clean.png")),
    ("grunge", include_bytes!("../../../../assets/icons/full/grunge.png")),
    ("street", include_bytes!("../../../../assets/icons/full/street.png")),
    ("paper", include_bytes!("../../../../assets/icons/full/paper.png")),
];

fn saved_icon(app: &App) -> Option<String> {
    app.pref_string(app_icons::PREF_KEY)
}

/// Sets the window (and so taskbar) icon to the chosen variant.
fn apply_icon(window: &tauri::WebviewWindow, id: &str) {
    if let Some((_, png)) = ICON_PNGS.iter().find(|(i, _)| *i == id)
        && let Ok(img) = tauri::image::Image::from_bytes(png)
    {
        let _ = window.set_icon(img);
    }
}

#[derive(Serialize)]
struct IconChoice {
    id: &'static str,
    name: &'static str,
    selected: bool,
}

/// The icon picker's rows.
#[tauri::command]
fn app_icons(app: tauri::State<'_, App>) -> Vec<IconChoice> {
    let current = app_icons::resolve(true, saved_icon(&app).as_deref());
    app_icons::FULL
        .iter()
        .map(|i| IconChoice { id: i.id, name: i.name, selected: i.id == current.id })
        .collect()
}

/// Saves the chosen icon and shows it at once.
#[tauri::command]
fn set_app_icon(id: String, app: tauri::State<'_, App>, handle: tauri::AppHandle) -> Result<(), String> {
    let icon = app_icons::resolve(true, Some(&id));
    app.set_pref(app_icons::PREF_KEY, drx_core::prefs::PrefValue::String(icon.id.to_owned()))
        .map_err(|e| e.to_string())?;
    for w in handle.webview_windows().values() {
        apply_icon(w, icon.id);
    }
    Ok(())
}

#[derive(Serialize)]
struct LibraryCounts {
    songs: i64,
    liked: i64,
    playlists: i64,
    artists: i64,
    history_events: i64,
}

#[tauri::command]
fn library_counts(app: tauri::State<'_, App>) -> LibraryCounts {
    let c = app.state().borrow().library.clone();
    LibraryCounts {
        songs: c.songs,
        liked: c.liked,
        playlists: c.playlists,
        artists: c.artists,
        history_events: c.history_events,
    }
}

fn main() {
    let profile = match std::env::var_os("DRX_PROFILE") {
        Some(p) => Profile::at(p),
        None => Profile::default_location().expect("no profile folder (APPDATA)"),
    };
    let core = App::start(profile, EditionProfile::full()).expect("start core");
    let icon = app_icons::resolve(true, saved_icon(&core).as_deref()).id;
    tauri::Builder::default()
        .manage(core)
        .setup(move |app| {
            for w in app.webview_windows().values() {
                apply_icon(w, icon);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![library_counts, app_icons, set_app_icon])
        .run(tauri::generate_context!())
        .expect("run Tauri");
}
