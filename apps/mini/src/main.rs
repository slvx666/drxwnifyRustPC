// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Drxwnify Mini: the native, light edition. All logic lives in `drx-app`; this file only binds the UI to it.

mod home_view;
mod lazy_library;
mod lazy_songs;
mod nav;
mod os;
mod settings_view;
mod themes;

use drx_app::{App, AppEvent, Command, CommandOutcome, EditionProfile, LibraryKind, ListSpec};
use drx_core::paths::Profile;
use lazy_songs::LazySongs;
use slint::{ComponentHandle, ModelRc};
use std::rc::Rc;

slint::include_modules!();

/// Developer checks while the owner uses the PC: `DRX_BACKGROUND_AT=x,y` opens the window at that screen position
/// (e.g. on the second monitor) without taking focus.
fn background_window() {
    let Ok(at) = std::env::var("DRX_BACKGROUND_AT") else {
        return;
    };
    let mut it = at.split(',').filter_map(|v| v.trim().parse::<i32>().ok());
    let (Some(x), Some(y)) = (it.next(), it.next()) else {
        return;
    };
    let _ = slint::BackendSelector::new()
        .with_winit_window_attributes_hook(move |a| {
            a.with_active(false)
                .with_position(slint::winit_030::winit::dpi::PhysicalPosition::new(x, y))
        })
        .select();
}

fn main() {
    background_window();
    let profile = match std::env::var_os("DRX_PROFILE") {
        Some(p) => Profile::at(p),
        None => Profile::default_location().expect("no profile folder (APPDATA)"),
    };
    // One instance per profile: a second launch brings the running window up and quits.
    let guard = match drx_platform::single_instance(
        &os::instance_name(profile.root()),
        &os::launch_argument(),
    ) {
        drx_platform::Instance::First(g) => g,
        drx_platform::Instance::Second => return,
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

    // Developer check of other themes: `DRX_THEME=<id>` or `DRX_THEME=<id>:day`.
    let (theme, day) = match std::env::var("DRX_THEME") {
        Ok(v) => (v.trim_end_matches(":day").to_string(), v.ends_with(":day")),
        Err(_) => (themes::current_theme_id(&app), themes::current_day(&app)),
    };
    themes::apply(&ui, &theme, day);
    ui.set_day_theme(day);

    let songs = Rc::new(LazySongs::new(app.clone(), ListSpec::LikedSongs));
    ui.set_songs(ModelRc::from(songs.clone()));
    nav::init(&songs);
    refresh_library(&ui, &app, &songs);
    ui.set_view_subtitle(format!("{} треков", songs.len()).into());

    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_open_library(move |item| {
            let Some(ui) = ui_w.upgrade() else { return };
            let spec = match item.kind.as_str() {
                "playlist" => ListSpec::Playlist {
                    id: item.id.to_string(),
                },
                "liked" => ListSpec::LikedSongs,
                "album" => ListSpec::Album {
                    id: item.id.to_string(),
                },
                "artist" => ListSpec::Artist {
                    id: item.id.to_string(),
                },
                "remote" => match ListSpec::from_entry_id(item.id.as_str()) {
                    Some(spec) => spec,
                    None => return,
                },
                _ => return,
            };
            let page = nav::Page {
                spec,
                title: item.title.to_string(),
                subtitle: String::new(),
                selected: item.id.to_string(),
                home: false,
                settings: false,
            };
            nav::go(&ui, &app2, &songs2, page);
        });
    }
    // Artist and album links (rows, now playing, player bar): the database page when the library has it,
    // otherwise the catalog page.
    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_open_artist(move |id| {
            let Some(ui) = ui_w.upgrade() else { return };
            if id.is_empty() {
                return;
            }
            let spec = app2.runtime().block_on(app2.artist_page(&id));
            let page = nav::Page {
                spec,
                title: String::new(),
                subtitle: String::new(),
                selected: String::new(),
                home: false,
                settings: false,
            };
            nav::go(&ui, &app2, &songs2, page);
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_open_album(move |id| {
            let Some(ui) = ui_w.upgrade() else { return };
            if id.is_empty() {
                return;
            }
            let spec = app2.runtime().block_on(app2.album_page(&id));
            let page = nav::Page {
                spec,
                title: String::new(),
                subtitle: String::new(),
                selected: String::new(),
                home: false,
                settings: false,
            };
            nav::go(&ui, &app2, &songs2, page);
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_back(move || {
            if let Some(ui) = ui_w.upgrade() {
                nav::step(&ui, &app2, &songs2, -1);
            }
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_forward(move || {
            if let Some(ui) = ui_w.upgrade() {
                nav::step(&ui, &app2, &songs2, 1);
            }
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_home(move || {
            if let Some(ui) = ui_w.upgrade() {
                nav::go(&ui, &app2, &songs2, nav::Page::home());
            }
        });
    }
    // Settings: every change is saved at once and applied without a restart.
    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_open_settings(move || {
            if let Some(ui) = ui_w.upgrade() {
                nav::go(&ui, &app2, &songs2, nav::Page::settings());
            }
        });
        let accounts_changed = {
            let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
            Rc::new(move || {
                let Some(ui) = ui_w.upgrade() else { return };
                settings_view::fill(&ui, &app2);
                refresh_library(&ui, &app2, &songs2);
                let a = app2.clone();
                app2.runtime().spawn(async move {
                    let _ = a.refresh_remote_library().await;
                });
            })
        };
        let (app2, ui_w, changed) = (app.clone(), ui.as_weak(), accounts_changed.clone());
        ui.on_set_main_account(move |k| {
            if let Some(s) = settings_view::source_of(&k) {
                match app2.set_main_account(s) {
                    Ok(()) => changed(),
                    Err(e) => {
                        if let Some(ui) = ui_w.upgrade() {
                            ui.set_toast(format!("Не удалось сохранить: {e}").into());
                        }
                    }
                }
            }
        });
        let (app2, changed) = (app.clone(), accounts_changed);
        ui.on_set_account_tab(move |k, on| {
            if let Some(s) = settings_view::source_of(&k)
                && app2.set_account_tab(s, on).is_ok()
            {
                changed();
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_set_theme(move |id| {
            let Some(ui) = ui_w.upgrade() else { return };
            let _ = app2.set_pref(
                themes::THEME_KEY,
                drx_core::prefs::PrefValue::String(id.to_string()),
            );
            settings_view::apply_theme(&ui, &app2);
            settings_view::fill(&ui, &app2);
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_set_day(move |day| {
            let Some(ui) = ui_w.upgrade() else { return };
            let _ = app2.set_pref(themes::DAY_KEY, drx_core::prefs::PrefValue::Bool(day));
            settings_view::apply_theme(&ui, &app2);
            if ui.get_view_settings() {
                settings_view::fill(&ui, &app2);
            }
        });
    }
    {
        let app2 = app.clone();
        ui.on_home_play(move |sec, card| {
            home_view::play(&app2, sec.max(0) as usize, card.max(0) as usize)
        });
    }
    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_play(move |i| {
            let Some(ui) = ui_w.upgrade() else { return };
            ui.set_current_index(i);
            if let Some(r) = songs2.row(i.max(0) as usize) {
                ui.set_now_id(r.id.into());
            }
            let (app3, h) = (app2.clone(), songs2.handle());
            app2.runtime().spawn(async move {
                let _ = app3.play_list(h, i.max(0) as usize).await;
            });
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
    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_search(move |q| {
            let Some(ui) = ui_w.upgrade() else { return };
            let q = q.trim().to_string();
            if q.is_empty() {
                return;
            }
            let page = nav::Page {
                spec: ListSpec::Search { query: q.clone() },
                title: format!("Поиск: {q}"),
                subtitle: String::new(),
                selected: String::new(),
                home: false,
                settings: false,
            };
            nav::go(&ui, &app2, &songs2, page);
        });
    }
    {
        let a = app.clone();
        ui.on_toggle(move || a.toggle_play());
        let a = app.clone();
        ui.on_next(move || a.next_track());
        let a = app.clone();
        ui.on_prev(move || a.previous_track());
        let a = app.clone();
        ui.on_seek(move |f| {
            if let Some(d) = a.clock().duration_s {
                a.seek(d * f as f64);
            }
        });
        let a = app.clone();
        ui.on_volume_changed(move |v| a.set_volume(v));
    }

    // Now playing → UI. The progress timer runs only while something plays (Mini: no redraws when idle).
    let progress = Rc::new(slint::Timer::default());
    {
        let mut rx = app.now_playing();
        let ui_w = ui.as_weak();
        app.runtime().spawn(async move {
            while rx.changed().await.is_ok() {
                let now = rx.borrow().clone();
                let ui_w = ui_w.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_w.upgrade() else { return };
                    if let Some(t) = &now.track {
                        ui.set_now_title(t.title.clone().into());
                        ui.set_now_id(t.id.clone().into());
                        ui.set_now_artists(t.artist_names().into());
                        ui.set_now_artist_id(
                            t.artists
                                .first()
                                .map(|a| a.id.clone())
                                .unwrap_or_default()
                                .into(),
                        );
                        ui.set_now_album_id(
                            t.album
                                .as_ref()
                                .map(|a| a.id.clone())
                                .unwrap_or_default()
                                .into(),
                        );
                        ui.set_now_info(
                            t.album
                                .as_ref()
                                .map(|a| a.name.clone())
                                .unwrap_or_default()
                                .into(),
                        );
                        ui.set_duration(fmt_time(t.duration_ms as f64 / 1000.0).into());
                        let url = t
                            .album
                            .as_ref()
                            .and_then(|a| drx_catalog::model::best_image(&a.images, 600))
                            .map(|i| i.url.clone());
                        ui.set_now_cover_url(url.unwrap_or_default().into());
                        ui.invoke_images_changed();
                    }
                    if let Some(i) = now.index {
                        ui.set_current_index(i as i32);
                    }
                    let up: Vec<QueueItem> = now
                        .upcoming
                        .iter()
                        .take(3)
                        .map(|t| QueueItem {
                            title: t.title.clone().into(),
                            artists: t.artist_names().into(),
                        })
                        .collect();
                    ui.set_upcoming(ModelRc::new(slint::VecModel::from(up)));
                    LYRICS.with(|l| *l.borrow_mut() = now.lyrics.clone());
                    match now.lyrics.as_deref() {
                        Some(drx_lyrics::Lyrics::Plain(text)) => {
                            ui.set_has_lyrics(true);
                            ui.set_lyric_now(
                                text.lines()
                                    .take(3)
                                    .collect::<Vec<_>>()
                                    .join(
                                        "
",
                                    )
                                    .into(),
                            );
                            ui.set_lyric_next("".into());
                        }
                        Some(drx_lyrics::Lyrics::Synced(_)) => ui.set_has_lyrics(true),
                        None => ui.set_has_lyrics(false),
                    }
                    ui.set_playing(now.playing);
                    if let Some(e) = &now.error {
                        ui.set_toast(format!("Не удалось включить: {e}").into());
                    }
                    ui.invoke_playing_changed();
                });
            }
        });
        let (a, ui_w) = (app.clone(), ui.as_weak());
        let timer = progress.clone();
        ui.on_playing_changed(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            if ui.get_playing() {
                let (a, ui_w) = (a.clone(), ui_w.clone());
                timer.start(
                    slint::TimerMode::Repeated,
                    std::time::Duration::from_millis(500),
                    move || {
                        let Some(ui) = ui_w.upgrade() else { return };
                        let c = a.clock();
                        ui.set_position(fmt_time(c.position_s).into());
                        if let Some(d) = c.duration_s.filter(|d| *d > 0.0) {
                            ui.set_duration(fmt_time(d).into());
                            ui.set_progress((c.position_s / d).clamp(0.0, 1.0) as f32);
                        }
                    },
                );
            } else {
                timer.stop();
            }
        });
    }
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
            ui.set_view_subtitle(format!("{} треков", songs2.len()).into());
        });
    }

    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_library_panel_changed(move || {
            if let Some(ui) = ui_w.upgrade() {
                refresh_library(&ui, &app2, &songs2);
            }
        });
    }
    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        let pending = Rc::new(std::cell::Cell::new(false));
        ui.on_images_changed(move || {
            // Many covers arrive together: redraw once per 150 ms.
            if pending.replace(true) {
                return;
            }
            let (app3, ui_w2, songs3, pending2) =
                (app2.clone(), ui_w.clone(), songs2.clone(), pending.clone());
            slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                pending2.set(false);
                let Some(ui) = ui_w2.upgrade() else { return };
                songs3.refresh_covers();
                library_model(&ui, &app3).refresh_covers();
                let url = ui.get_now_cover_url().to_string();
                match (!url.is_empty())
                    .then(|| cover(&app3, &url, NOW_COVER))
                    .flatten()
                {
                    Some(c) => {
                        ui.set_now_cover(c);
                        ui.set_has_now_cover(true);
                    }
                    None => ui.set_has_now_cover(false),
                }
                nav::refresh_releases(&ui, &app3);
                home_view::refresh(&ui, &app3);
                let url = ui.get_view_cover_url().to_string();
                if !url.is_empty()
                    && let Some(c) = cover(&app3, &url, HEADER_COVER)
                {
                    ui.set_view_cover(c);
                    ui.set_has_view_cover(true);
                }
            });
        });
    }
    // The accounts' playlists load in the background and appear in the library panel when ready.
    {
        let a = app.clone();
        app.runtime().spawn(async move {
            let _ = a.refresh_remote_library().await;
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
                        AppEvent::LibraryChanged => ui.invoke_library_panel_changed(),
                        AppEvent::ImageReady { .. } => ui.invoke_images_changed(),
                    }
                });
            }
        });
    }

    // Developer check without clicking (no automation needed): DRX_DEMO=<library id> opens that list and plays
    // the first track at volume 0 (`vk` = VK «Моя музыка», `album`/`artist` = the first saved one). Not a user feature.
    // Developer checks with real clicks stay silent: DRX_MUTE=1.
    if std::env::var("DRX_MUTE").is_ok() {
        app.set_volume(0.0);
    }
    if std::env::var("DRX_DEMO").is_err() {
        nav::go(&ui, &app, &songs, nav::Page::home());
    }
    if let Ok(demo) = std::env::var("DRX_DEMO") {
        app.set_volume(0.0);
        let entries = app
            .runtime()
            .block_on(app.library_entries())
            .unwrap_or_default();
        let found = match demo.as_str() {
            "vk" => entries.iter().find(|e| e.id == "vk:liked"),
            "album" => entries.iter().find(|e| e.kind == LibraryKind::Album),
            "artist" => entries.iter().find(|e| e.kind == LibraryKind::Artist),
            id => entries.iter().find(|e| e.id == id),
        };
        // The list's own title and subtitle, so screenshots look real.
        let item = match found {
            Some(e) => LibItem {
                title: e.title.clone().into(),
                subtitle: e.subtitle.clone().into(),
                kind: lazy_library::kind_name(e.kind).into(),
                id: e.id.clone().into(),
                cover: Default::default(),
                has_cover: false,
            },
            None => LibItem {
                title: demo.clone().into(),
                subtitle: "".into(),
                kind: "remote".into(),
                id: demo.into(),
                cover: Default::default(),
                has_cover: false,
            },
        };
        ui.set_selected_id(item.id.clone());
        ui.invoke_open_library(item);
        // DRX_DEMO_THEN=artist|album: once the track plays, follow its artist or album link.
        let then = std::cell::Cell::new(std::env::var("DRX_DEMO_THEN").ok());
        let started = std::cell::Cell::new(false);
        let (ui_w, songs2) = (ui.as_weak(), songs.clone());
        let t = slint::Timer::default();
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(500),
            move || {
                let Some(ui) = ui_w.upgrade() else { return };
                if !started.get()
                    && songs2.len() > 0
                    && !ui.get_playing()
                    && ui.get_current_index() < 0
                {
                    started.set(true);
                    ui.invoke_play(0);
                }
                let artist = ui.get_now_artist_id();
                let album = ui.get_now_album_id();
                match then.take().as_deref() {
                    Some("artist") if !artist.is_empty() => ui.invoke_open_artist(artist),
                    Some("album") if !album.is_empty() => ui.invoke_open_album(album),
                    other => then.set(other.map(str::to_string)),
                }
            },
        );
        std::mem::forget(t);
    }

    // System media controls (media keys, volume flyout, lock screen) and the tray.
    let platform = os::start(&ui, &app, Some(&guard));
    {
        let (app2, ui_w, platform) = (app.clone(), ui.as_weak(), platform.clone());
        ui.on_set_hotkeys(move |on| {
            let _ = app2.set_pref(os::HOTKEYS_KEY, drx_core::prefs::PrefValue::Bool(on));
            if let Some(p) = &platform {
                os::apply_hotkeys(p, &app2);
            }
            if let Some(ui) = ui_w.upgrade() {
                settings_view::fill(&ui, &app2);
            }
        });
    }

    ui.run().expect("run UI");
}

thread_local! {
    static LYRICS: std::cell::RefCell<Option<std::sync::Arc<drx_lyrics::Lyrics>>> = const { std::cell::RefCell::new(None) };
    static LIBRARY: std::cell::RefCell<Option<Rc<lazy_library::LazyLibrary>>> = const { std::cell::RefCell::new(None) };
}

fn library_model(ui: &MainWindow, app: &App) -> Rc<lazy_library::LazyLibrary> {
    LIBRARY.with(|l| {
        l.borrow_mut()
            .get_or_insert_with(|| {
                let m = Rc::new(lazy_library::LazyLibrary::new(app.clone()));
                ui.set_library(ModelRc::from(m.clone()));
                m
            })
            .clone()
    })
}

fn refresh_library(ui: &MainWindow, app: &App, songs: &LazySongs) {
    let entries = app
        .runtime()
        .block_on(app.library_entries())
        .unwrap_or_default();
    let lib = library_model(ui, app);
    lib.set(entries);
    ui.set_empty_library(songs.len() == 0 && lib.len() <= 1);
}

/// Cover size in list rows (px; decoded once at this size).
pub const ROW_COVER: u32 = 84;
pub const NOW_COVER: u32 = 560;
/// Cover next to an album/artist page title.
pub const HEADER_COVER: u32 = 240;
/// Release cards on an artist page.
pub const RELEASE_COVER: u32 = 256;

/// A cover as a Slint image when it is cached; otherwise asks the core to load it (ImageReady follows).
pub fn cover(app: &App, url: &str, size: u32) -> Option<slint::Image> {
    match app.image(url, size) {
        Some(p) => {
            let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &p.rgba, p.size, p.size,
            );
            Some(slint::Image::from_rgba8(buf))
        }
        None => {
            app.request_image(url, size);
            None
        }
    }
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
