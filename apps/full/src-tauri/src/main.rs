// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Drxwnify Full: Tauri shell. Commands here are thin wrappers over `drx-app`; no logic of their own.

use drx_app::{App, EditionProfile};
use drx_core::paths::Profile;
use serde::Serialize;

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
    tauri::Builder::default()
        .manage(core)
        .invoke_handler(tauri::generate_handler![library_counts])
        .run(tauri::generate_context!())
        .expect("run Tauri");
}
