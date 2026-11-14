// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Drxwnify Mini: the native, light edition. All logic lives in `drx-app`; this file only binds the UI to it.

mod lazy_songs;
mod themes;

use drx_app::{App, AppEvent, Command, CommandOutcome, EditionProfile, LibraryKind, ListSpec};
use drx_core::paths::Profile;
use lazy_songs::LazySongs;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;

slint::include_modules!();

fn main() {
    let profile = match std::env::var_os("DRX_PROFILE") {
        Some(p) => Profile::at(p),
        None => Profile::default_location().expect("no profile folder (APPDATA)"),
    };
    let app = App::start(profile, EditionProfile::mini()).expect("start core");
    // Developer shortcut: `--import <backup>` imports before the window opens.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--import")
        && let Some(path) = args.get(i + 1)
    {
        let cmd = Command::ImportBackup {
            path: path.into(),
            keep_youtube_session: false,
        };
        if let Err(e) = app.runtime().block_on(app.run(cmd)) {
            eprintln!("import failed: {e}");
        }
    }
    let ui = MainWindow::new().expect("create window");

    themes::apply(&ui, &themes::current_theme_id(&app), false);

    let songs = Rc::new(LazySongs::new(app.clone(), ListSpec::LikedSongs));
    ui.set_songs(ModelRc::from(songs.clone()));
    refresh_library(&ui, &app, &songs);

    {
        let (ui_w, songs2) = (ui.as_weak(), songs.clone());
        ui.on_open_library(move |item| {
            let Some(ui) = ui_w.upgrade() else { return };
            let spec = match item.kind.as_str() {
                "playlist" => ListSpec::Playlist {
                    id: item.id.to_string(),
                },
                "liked" => ListSpec::LikedSongs,
                _ => {
                    ui.set_toast(
                        "Страницы альбомов и исполнителей появятся вместе с каталогами".into(),
                    );
                    return;
                }
            };
            songs2.reopen(spec);
            ui.set_view_title(item.title.clone());
            ui.set_view_subtitle(format!("{} треков", songs2.len()).into());
            ui.set_current_index(-1);
        });
    }
    {
        let (ui_w, songs2) = (ui.as_weak(), songs.clone());
        ui.on_play(move |i| {
            let Some(ui) = ui_w.upgrade() else { return };
            if let Some(row) = songs2.row(i as usize) {
                ui.set_current_index(i);
                ui.set_now_title(row.title.clone().into());
                ui.set_now_artists(row.artists.clone().into());
                ui.set_now_info(row.album.clone().unwrap_or_default().into());
                ui.set_duration(fmt_time(row.duration_s as f64).into());
                ui.set_toast(
                    "Поиск звука для треков каталога появится на вехе 5 (источники)".into(),
                );
            }
        });
    }
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_import_backup(move || {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Резервная копия Drxwnify")
                .add_filter("Резервная копия", &["backup", "zip"])
                .pick_file()
            else {
                return;
            };
            let Some(ui) = ui_w.upgrade() else { return };
            ui.set_toast("Импорт…".into());
            let ui_w2 = ui_w.clone();
            let app4 = app2.clone();
            app2.runtime().spawn(async move {
                let res = app4
                    .run(Command::ImportBackup {
                        path,
                        keep_youtube_session: false,
                    })
                    .await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_w2.upgrade() else { return };
                    match res {
                        Ok(CommandOutcome::Imported {
                            songs, accounts, ..
                        }) => {
                            let acc: Vec<&str> = [
                                ("Spotify", &accounts.spotify),
                                ("Яндекс", &accounts.yandex),
                                ("ВК", &accounts.vk),
                            ]
                            .into_iter()
                            .filter(|(_, a)| a.is_some())
                            .map(|(n, _)| n)
                            .collect();
                            let acc = if acc.is_empty() {
                                "нет".to_string()
                            } else {
                                acc.join(", ")
                            };
                            ui.set_toast(
                                format!("Импортировано: {songs} треков; аккаунты: {acc}").into(),
                            );
                        }
                        Ok(_) => {}
                        Err(e) => ui.set_toast(format!("Не удалось импортировать: {e}").into()),
                    }
                    ui.invoke_library_changed();
                });
            });
        });
    }
    ui.on_search(|_| {});
    ui.on_toggle(|| {});
    ui.on_next(|| {});
    ui.on_prev(|| {});
    ui.on_seek(|_| {});
    ui.on_volume_changed(|_| {});
    {
        let songs2 = songs.clone();
        ui.on_list_changed(move || songs2.invalidate());
    }
    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_library_changed(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            songs2.reopen(ListSpec::LikedSongs);
            ui.set_view_title("Любимые треки".into());
            ui.set_selected_id("liked".into());
            refresh_library(&ui, &app2, &songs2);
        });
    }

    // Core events → UI thread. Lists refresh only when their tables change; nothing polls.
    {
        let mut rx = app.events();
        let ui_w = ui.as_weak();
        app.runtime().spawn(async move {
            while let Ok(ev) = rx.recv().await {
                let ui_w = ui_w.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_w.upgrade() else { return };
                    match ev {
                        AppEvent::ListChanged { .. } | AppEvent::LibraryReplaced => {
                            ui.invoke_list_changed()
                        }
                    }
                });
            }
        });
    }

    ui.run().expect("run UI");
}

fn refresh_library(ui: &MainWindow, app: &App, songs: &LazySongs) {
    let entries = app
        .runtime()
        .block_on(app.library_entries())
        .unwrap_or_default();
    let items: Vec<LibItem> = entries
        .iter()
        .map(|e| LibItem {
            title: e.title.clone().into(),
            subtitle: e.subtitle.clone().into(),
            kind: SharedString::from(match e.kind {
                LibraryKind::Liked => "liked",
                LibraryKind::Playlist => "playlist",
                LibraryKind::Album => "album",
                LibraryKind::Artist => "artist",
            }),
            id: e.id.clone().into(),
        })
        .collect();
    ui.set_library(ModelRc::new(VecModel::from(items)));
    let n = songs.len();
    ui.set_empty_library(n == 0 && entries.len() <= 1);
    ui.set_view_subtitle(format!("{n} треков").into());
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
