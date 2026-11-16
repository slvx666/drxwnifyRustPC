// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Drxwnify Mini: the native, light edition. All logic lives in `drx-app`; this file only binds the UI to it.

mod artist_view;
mod friends_view;
mod history_view;
mod hotkeys;
mod home_view;
mod lazy_library;
mod lazy_songs;
mod likes_ui;
mod playlists_ui;
mod portrait_view;
mod rooms_ui;
mod sleep_ui;
mod track_menu;
mod nav;
mod os;
mod search_view;
mod settings_view;
mod stats_view;
mod themes;
mod viewer;

use drx_app::{App, AppEvent, Command, CommandOutcome, EditionProfile, LibraryKind, ListSpec};
use drx_core::paths::Profile;
use lazy_songs::LazySongs;
use slint::{ComponentHandle, ModelRc};
use std::rc::Rc;

slint::include_modules!();

/// The native window: no system frame (the top bar is the title bar), but the Windows shadow and, on Windows 11,
/// rounded corners. Developer checks while the owner uses the PC: `DRX_BACKGROUND_AT=x,y` opens the window at that
/// screen position (e.g. on the second monitor) without taking focus.
fn select_backend() {
    let at = std::env::var("DRX_BACKGROUND_AT").ok().and_then(|at| {
        let mut it = at.split(',').filter_map(|v| v.trim().parse::<i32>().ok());
        Some((it.next()?, it.next()?))
    });
    let _ = slint::BackendSelector::new()
        .with_winit_window_attributes_hook(move |a| {
            #[cfg(windows)]
            let a = {
                use slint::winit_030::winit::platform::windows::{
                    CornerPreference, WindowAttributesExtWindows,
                };
                a.with_undecorated_shadow(true)
                    .with_corner_preference(CornerPreference::Round)
            };
            match at {
                Some((x, y)) => a
                    .with_active(false)
                    .with_position(slint::winit_030::winit::dpi::PhysicalPosition::new(x, y)),
                None => a,
            }
        })
        .select();
}

fn main() {
    // `--profile <folder>`: the same as DRX_PROFILE (a drxwnify:// link starts the app without the environment).
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--profile")
        && let Some(p) = args.get(i + 1)
    {
        // SAFETY: the very start of main, before any other thread exists.
        unsafe { std::env::set_var("DRX_PROFILE", p) };
    }
    select_backend();
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
            let page = nav::Page::list(
                spec,
                &item.title,
                &item.id,
                Some(item.thumb.to_string()).filter(|t| !t.is_empty()),
            );
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
            let page = nav::Page::list(spec, "", "", None);
            nav::go(&ui, &app2, &songs2, page);
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_open_album(move |id| {
            let Some(ui) = ui_w.upgrade() else { return };
            if id.is_empty() {
                return;
            }
            let spec = app2.runtime().block_on(app2.album_page(&id));
            let page = nav::Page::list(spec, "", "", None);
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
        let (ui_w, songs2) = (ui.as_weak(), songs.clone());
        ui.on_artist_more(move || {
            if let Some(ui) = ui_w.upgrade() {
                artist_view::toggle_more(&ui, &songs2);
            }
        });
    }
    // History and statistics.
    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_open_history(move || {
            if let Some(ui) = ui_w.upgrade() {
                nav::go(&ui, &app2, &songs2, nav::Page::history());
            }
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_open_stats(move || {
            if let Some(ui) = ui_w.upgrade() {
                nav::go(&ui, &app2, &songs2, nav::Page::stats());
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_history_play(move |i| {
            if let Some(id) = history_view::play(&app2, i.max(0) as usize)
                && let Some(ui) = ui_w.upgrade()
            {
                ui.set_now_id(id.into());
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_history_remove(move |e| {
            if let Some(ui) = ui_w.upgrade() {
                history_view::remove(&ui, &app2, e as i64);
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_stats_period(move |i| {
            if let Some(ui) = ui_w.upgrade() {
                stats_view::set_period(&ui, &app2, i.max(0) as usize);
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_stats_play(move |i| {
            if let Some(id) = stats_view::play(&app2, i.max(0) as usize)
                && let Some(ui) = ui_w.upgrade()
            {
                ui.set_now_id(id.into());
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_stats_more(move || {
            if let Some(ui) = ui_w.upgrade() {
                stats_view::toggle_more(&ui, &app2);
            }
        });
    }
    // Search: suggestions, history, results.
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_search_typing(move |t| {
            if let Some(ui) = ui_w.upgrade() {
                search_view::typing(&ui, &app2, t.to_string());
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_forget_search(move |q| {
            let (app3, ui_w2, q) = (app2.clone(), ui_w.clone(), q.to_string());
            app2.runtime().spawn(async move {
                let _ = app3.forget_search(&q).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_w2.upgrade() {
                        let text = ui.get_search_text().to_string();
                        search_view::typing(&ui, &app3, text);
                    }
                });
            });
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_clear_search_history(move || {
            let app3 = app2.clone();
            app2.runtime().spawn(async move {
                let _ = app3.clear_search_history().await;
            });
            if let Some(ui) = ui_w.upgrade() {
                search_view::close_suggestions(&ui);
            }
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_search_source(move |i| {
            let Some(ui) = ui_w.upgrade() else { return };
            if let (Some(src), Some((q, _))) = (
                search_view::source_at(i.max(0) as usize),
                search_view::current(),
            ) {
                nav::go(&ui, &app2, &songs2, nav::Page::search(&q, src));
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_search_play(move |i| {
            if let Some(id) = search_view::play(&app2, i.max(0) as usize)
                && let Some(ui) = ui_w.upgrade()
            {
                ui.set_now_id(id.into());
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_search_top(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            match search_view::top(&ui) {
                Some(search_view::Top::Artist(id)) => ui.invoke_open_artist(id.into()),
                Some(search_view::Top::Track) => {
                    if let Some(id) = search_view::play(&app2, 0) {
                        ui.set_now_id(id.into());
                    }
                }
                None => {}
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_search_more(move || {
            if let Some(ui) = ui_w.upgrade() {
                search_view::toggle_expanded(&ui, &app2);
            }
        });
        let ui_w = ui.as_weak();
        ui.on_search_artist(move |i| {
            if let (Some(ui), Some(id)) =
                (ui_w.upgrade(), search_view::artist_id(i.max(0) as usize))
            {
                ui.invoke_open_artist(id.into());
            }
        });
        let ui_w = ui.as_weak();
        ui.on_search_album(move |i| {
            if let (Some(ui), Some(id)) = (ui_w.upgrade(), search_view::album_id(i.max(0) as usize))
            {
                ui.invoke_open_album(id.into());
            }
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_search_playlist(move |i| {
            let Some(ui) = ui_w.upgrade() else { return };
            let Some((id, title, image, library)) = search_view::playlist(i.max(0) as usize) else {
                return;
            };
            let page = nav::Page::list(app2.playlist_page(&id, library), &title, "", image);
            nav::go(&ui, &app2, &songs2, page);
        });
    }
    // Page heading buttons and the player's shuffle / repeat.
    {
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_play_all(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            if let Some(r) = songs2.row(0) {
                ui.set_now_id(r.id.into());
            }
            let (app3, h) = (app2.clone(), songs2.handle());
            app2.runtime().spawn(async move {
                let _ = app3.play_list(h, 0).await;
            });
        });
        let (app2, songs2) = (app.clone(), songs.clone());
        ui.on_shuffle_all(move || {
            let (app3, h) = (app2.clone(), songs2.handle());
            app2.runtime().spawn(async move {
                let _ = app3.play_list_shuffled(h).await;
            });
        });
        let app2 = app.clone();
        ui.on_toggle_shuffle(move || {
            let on = app2.now_playing().borrow().shuffle;
            app2.set_shuffle(!on);
        });
        let app2 = app.clone();
        ui.on_cycle_repeat(move || {
            let r = app2.now_playing().borrow().repeat;
            app2.set_repeat(r.next());
        });
        ui.set_now_shuffle(app.now_playing().borrow().shuffle);
        ui.set_now_repeat(app.now_playing().borrow().repeat as i32);
    }
    // Library panel: chips, order, search (LibraryMixScreen).
    {
        use drx_app::{LibraryGroup, MixSort};
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_lib_set_filter(move |f| {
            let Some(ui) = ui_w.upgrade() else { return };
            change_library_view(&ui, &app2, &songs2, |v| {
                v.filter = match f.as_str() {
                    "playlists" => Some(LibraryGroup::Playlists),
                    "albums" => Some(LibraryGroup::Albums),
                    "artists" => Some(LibraryGroup::Artists),
                    _ => None,
                }
            });
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_lib_set_sort(move |s| {
            let Some(ui) = ui_w.upgrade() else { return };
            change_library_view(&ui, &app2, &songs2, |v| {
                v.sort = match s.as_str() {
                    "name" => MixSort::Name,
                    "updated" => MixSort::Updated,
                    _ => MixSort::Added,
                };
                // Names read А→Я by default, dates newest first.
                v.descending = v.sort != MixSort::Name;
            });
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_lib_set_descending(move |d| {
            let Some(ui) = ui_w.upgrade() else { return };
            change_library_view(&ui, &app2, &songs2, |v| v.descending = d);
        });
        let (app2, ui_w, songs2) = (app.clone(), ui.as_weak(), songs.clone());
        ui.on_lib_query(move |q| {
            let Some(ui) = ui_w.upgrade() else { return };
            change_library_view(&ui, &app2, &songs2, |v| v.query = q.to_string());
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
            // The place chosen last stays (the main catalog at first).
            let source = search_view::current()
                .map(|(_, s)| s)
                .unwrap_or(drx_app::SearchSource::Main);
            nav::go(&ui, &app2, &songs2, nav::Page::search(&q, source));
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
        let ui_w = ui.as_weak();
        ui.on_seek(move |f| {
            let Some(ui) = ui_w.upgrade() else { return };
            // Not started yet (restored after launch): the track's own length; it starts there when played.
            let d = a.clock().duration_s.or_else(|| {
                let d = ui.get_duration_s() as f64;
                (d > 0.0).then_some(d)
            });
            if let Some(d) = d {
                let s = d * f as f64;
                a.seek(s);
                // The bar stays where it was let go (also paused, when no timer moves it).
                ui.set_position(fmt_time(s).into());
                ui.set_progress(f);
            }
        });
        let a = app.clone();
        ui.on_volume_changed(move |v| a.set_volume(v));
    }

    // Side column widths the owner dragged (PC-only keys; the phone ignores them).
    {
        let grid = ui.global::<Grid>();
        if let Some(w) = app.pref_i64("pcLibraryWidth").filter(|w| (200..=480).contains(w)) {
            grid.set_lib_width(w as f32);
        }
        if let Some(w) = app.pref_i64("pcSideWidth").filter(|w| (240..=520).contains(w)) {
            grid.set_side_width(w as f32);
        }
        let (a, ui_w) = (app.clone(), ui.as_weak());
        ui.on_panels_resized(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let grid = ui.global::<Grid>();
            let _ = a.set_pref("pcLibraryWidth", drx_core::prefs::PrefValue::Int(grid.get_lib_width().round() as i32));
            let _ = a.set_pref("pcSideWidth", drx_core::prefs::PrefValue::Int(grid.get_side_width().round() as i32));
        });
    }

    // A toast stays 4 s after its last change (a newer one starts its own 4 s).
    {
        let ui_w = ui.as_weak();
        let seq = Rc::new(std::cell::Cell::new(0u64));
        ui.on_toast_shown(move |_| {
            let n = seq.get() + 1;
            seq.set(n);
            let (ui_w, seq) = (ui_w.clone(), seq.clone());
            slint::Timer::single_shot(std::time::Duration::from_secs(4), move || {
                if seq.get() == n
                    && let Some(ui) = ui_w.upgrade()
                {
                    ui.set_toast("".into());
                }
            });
        });
    }

    // Now playing → UI. The progress timer runs only while something plays (Mini: no redraws when idle).
    let progress = Rc::new(slint::Timer::default());
    {
        let mut rx = app.now_playing();
        let ui_w = ui.as_weak();
        let app_now0 = app.clone();
        app.runtime().spawn(async move {
            while rx.changed().await.is_ok() {
                let now = rx.borrow().clone();
                let ui_w = ui_w.clone();
                let app_now = app_now0.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_w.upgrade() else { return };
                    if let Some(t) = &now.track {
                        ui.set_now_title(t.title.clone().into());
                        if ui.get_now_id() != t.id.as_str() {
                            ui.set_now_id(t.id.clone().into());
                            likes_ui::now_changed(&ui, &app_now, t.id.clone());
                        }
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
                        ui.set_duration_s(t.duration_ms as f32 / 1000.0);
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
                    ui.set_now_shuffle(now.shuffle);
                    ui.set_now_repeat(now.repeat as i32);
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
                            ui.set_duration_s(d as f32);
                            ui.set_progress((c.position_s / d).clamp(0.0, 1.0) as f32);
                        }
                    },
                );
            } else {
                timer.stop();
                // Paused (or a restored track not started yet): the bar and its hover time still know the length.
                let c = a.clock();
                ui.set_position(fmt_time(c.position_s).into());
                if let Some(d) = c.duration_s.filter(|d| *d > 0.0) {
                    ui.set_duration(fmt_time(d).into());
                    ui.set_duration_s(d as f32);
                    ui.set_progress((c.position_s / d).clamp(0.0, 1.0) as f32);
                }
            }
        });
    }
    {
        let (app2, songs2) = (app.clone(), songs.clone());
        let ui_w = ui.as_weak();
        ui.on_list_changed(move || {
            songs2.invalidate();
            if let Some(ui) = ui_w.upgrade() {
                artist_view::set_tracks(&ui, &songs2);
                nav::refresh_header(&ui, &app2);
            }
        });
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
                if ui.get_view_friends() {
                    friends_view::refresh(&ui, &app3);
                }
                let url = ui.get_now_cover_url().to_string();
                match (!url.is_empty())
                    .then(|| cover_r(&app3, &url, NOW_COVER, theme_r(100.0, NOW_COVER, 288.0)))
                    .flatten()
                {
                    Some(c) => {
                        ui.set_now_cover(c);
                        ui.set_has_now_cover(true);
                        if let Some(small) = cover_r(&app3, &url, 104, theme_r(ROW_COVER_R, 104, 52.0)) {
                            ui.set_now_cover_small(small);
                        }
                    }
                    None => ui.set_has_now_cover(false),
                }
                nav::refresh_releases(&ui, &app3);
                home_view::refresh(&ui, &app3);
                search_view::refresh_covers(&ui, &app3);
                artist_view::refresh_covers(&ui, &app3, &songs3);
                history_view::refresh_covers(&ui);
                stats_view::refresh_covers(&ui, &app3);
                viewer::refresh(&ui, &app3);
                let url = ui.get_view_cover_url().to_string();
                if !url.is_empty()
                    && let Some(c) = cover_r(
                        &app3,
                        &url,
                        HEADER_COVER,
                        theme_r(100.0, HEADER_COVER, 132.0),
                    )
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
    let demo_page = std::env::var("DRX_DEMO").ok();
    if demo_page.as_deref() == Some("history") {
        nav::go(&ui, &app, &songs, nav::Page::history());
    } else if demo_page.as_deref() == Some("stats") {
        nav::go(&ui, &app, &songs, nav::Page::stats());
    } else if let Some(q) = std::env::var("DRX_DEMO")
        .ok()
        .and_then(|d| d.strip_prefix("search:").map(str::to_string))
    {
        nav::go(
            &ui,
            &app,
            &songs,
            nav::Page::search(&q, drx_app::SearchSource::Main),
        );
    } else if let Ok(demo) = std::env::var("DRX_DEMO") {
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
                thumb: e.thumbnail.clone().unwrap_or_default().into(),
            },
            None => LibItem {
                title: demo.clone().into(),
                subtitle: "".into(),
                kind: "remote".into(),
                id: demo.into(),
                cover: Default::default(),
                has_cover: false,
                thumb: Default::default(),
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
    os::wire_title_bar(&ui);
    likes_ui::wire(&ui, &app);
    track_menu::wire(&ui, &app);
    playlists_ui::wire(&ui, &app);
    friends_view::wire(&ui, &app);
    viewer::wire(&ui, &app);
    portrait_view::wire(&ui, &app);
    rooms_ui::wire(&ui, &app);
    sleep_ui::wire(&ui, &app);
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_open_friends(move || {
            if let Some(ui) = ui_w.upgrade() {
                nav::go_page(&ui, &app2, nav::Page::friends());
            }
        });
    }
    // «Поделиться» on a page: its link to the clipboard (the phone's AppLinks format).
    {
        let ui_w = ui.as_weak();
        ui.on_share_view(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let Some(page) = nav::current() else { return };
            let Some((kind, id)) = nav::share_target(&page) else { return };
            let by = page.subtitle.rsplit(" · ").next().filter(|_| kind != "artist").unwrap_or("").to_string();
            track_menu::copy_share(&ui, &drx_app::share_text(kind, &id, &page.title, &by));
        });
    }
    // Links: an invite opens Friends with the code filled in (adding stays the owner's click, as on the phone where
    // the invite asks first); music goes through the search, which knows every link the phone shares.
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_open_link(move |link| {
            let Some(ui) = ui_w.upgrade() else { return };
            let link = link.trim().to_string();
            // A room link (the phone's metrolist.meowery.eu/listen?code=…, or drxwnify://listen?code=…): join it.
            if link.starts_with(drx_together::LINK_PREFIX) || link.starts_with("drxwnify://listen") {
                let code = drx_together::normalize_code(&link);
                if code.len() >= 4 {
                    nav::go_page(&ui, &app2, nav::Page::friends());
                    app2.room_start(&ui.get_friends_me_name(), Some(&code));
                    return;
                }
            }
            let invite = link
                .strip_prefix("drxwnify://add/")
                .or_else(|| link.strip_prefix(drx_social::friends::INVITE_BASE))
                .map(|rest| rest.split(['?', '#', '/']).next().unwrap_or("").to_string())
                .filter(|code| !code.is_empty());
            match invite {
                Some(code) => {
                    nav::go_page(&ui, &app2, nav::Page::friends());
                    ui.set_friends_add_text(code.into());
                    ui.set_toast("Приглашение в друзья: нажмите «Добавить», чтобы отправить заявку".into());
                }
                None => match drx_app::link_page(&link) {
                    Some(("album", id)) => ui.invoke_open_album(id.into()),
                    Some((_, id)) => ui.invoke_open_artist(id.into()),
                    None => nav::go_page(&ui, &app2, nav::Page::search(&link, drx_app::SearchSource::Main)),
                },
            }
        });
    }
    let first_link = os::launch_argument();
    if !first_link.is_empty() {
        let ui_w = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(600), move || {
            if let Some(ui) = ui_w.upgrade() {
                ui.invoke_open_link(first_link.into());
            }
        });
    }
    os::register_scheme();
    let platform = os::start(&ui, &app, Some(&guard));
    {
        let (app2, ui_w, platform) = (app.clone(), ui.as_weak(), platform.clone());
        let (app3, ui_w3) = (app.clone(), ui.as_weak());
        ui.on_set_autostart(move |on| {
            let Some(ui) = ui_w3.upgrade() else { return };
            if let Err(e) = os::set_autostart(on) {
                ui.set_toast(format!("Не удалось изменить автозапуск: {e}").into());
            }
            settings_view::fill(&ui, &app3);
        });
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

    // Own hotkey combinations (settings): recording lets the keys through, then they are registered again.
    {
        let p = platform.clone();
        ui.on_hotkey_start(move |_| {
            if let Some(p) = &p {
                p.set_hotkeys(Vec::new());
            }
        });
        let reapply = {
            let (app2, p) = (app.clone(), platform.clone());
            move |ui: &MainWindow| {
                if let Some(p) = &p {
                    os::apply_hotkeys(p, &app2);
                }
                settings_view::fill_hotkeys(ui, &app2);
            }
        };
        let (app2, ui_w, re) = (app.clone(), ui.as_weak(), reapply.clone());
        ui.on_hotkey_captured(move |id, text, ctrl, alt, shift| {
            let Some(ui) = ui_w.upgrade() else { return };
            match hotkeys::vk_of(&text) {
                Some(key) if ctrl || alt || shift => {
                    hotkeys::set(&app2, id as u32, Some(drx_platform::Hotkey { id: id as u32, ctrl, alt, shift, key }));
                    ui.set_hotkey_recording(-1);
                    re(&ui);
                }
                Some(_) => ui.set_toast("Нужен Ctrl, Alt или Shift вместе с клавишей".into()),
                None => ui.set_toast("Эту клавишу нельзя назначить".into()),
            }
        });
        let (ui_w, re) = (ui.as_weak(), reapply.clone());
        ui.on_hotkey_cancel(move || {
            if let Some(ui) = ui_w.upgrade() {
                re(&ui);
            }
        });
        let (app2, ui_w, re) = (app.clone(), ui.as_weak(), reapply.clone());
        ui.on_hotkey_reset(move |id| {
            hotkeys::reset(&app2, id as u32);
            if let Some(ui) = ui_w.upgrade() {
                re(&ui);
            }
        });
        let (app2, ui_w, re) = (app.clone(), ui.as_weak(), reapply);
        ui.on_hotkey_off(move |id| {
            hotkeys::set(&app2, id as u32, None);
            if let Some(ui) = ui_w.upgrade() {
                re(&ui);
            }
        });
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_window_key(move |text, ctrl, alt, shift| {
            let Some(ui) = ui_w.upgrade() else { return false };
            if os::hotkeys_on(&app2) {
                return false; // registered system-wide: Windows hands them over itself
            }
            match hotkeys::vk_of(&text).and_then(|vk| hotkeys::action_for(&app2, ctrl, alt, shift, vk)) {
                Some(id) => {
                    os::perform(&ui, &app2, id);
                    true
                }
                None => false,
            }
        });
    }

    // Started with Windows: open minimised (after the window exists).
    if os::started_minimized() {
        let ui_w = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
            if let Some(ui) = ui_w.upgrade() {
                use slint::winit_030::WinitWindowAccessor;
                ui.window().with_winit_window(|w| w.set_minimized(true));
            }
        });
    }

    ui.run().expect("run UI");
}

thread_local! {
    static LYRICS: std::cell::RefCell<Option<std::sync::Arc<drx_lyrics::Lyrics>>> = const { std::cell::RefCell::new(None) };
    static LIBRARY: std::cell::RefCell<Option<Rc<lazy_library::LazyLibrary>>> = const { std::cell::RefCell::new(None) };
    static LIB_VIEW: std::cell::RefCell<Option<drx_app::LibraryView>> = const { std::cell::RefCell::new(None) };
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
    let view = LIB_VIEW.with(|v| {
        v.borrow_mut()
            .get_or_insert_with(|| app.library_view_saved())
            .clone()
    });
    let entries = app
        .runtime()
        .block_on(app.library_view(&view))
        .unwrap_or_default();
    let lib = library_model(ui, app);
    lib.set(entries);
    sync_library_view(ui, &view);
    // «Empty library» means nothing at all, not an empty filter or search.
    let filtered = view.filter.is_some() || !view.query.is_empty();
    ui.set_empty_library(!filtered && songs.len() == 0 && lib.len() <= 1);
}

fn sync_library_view(ui: &MainWindow, v: &drx_app::LibraryView) {
    use drx_app::{LibraryGroup, MixSort};
    ui.set_lib_filter(
        match v.filter {
            Some(LibraryGroup::Playlists) => "playlists",
            Some(LibraryGroup::Albums) => "albums",
            Some(LibraryGroup::Artists) => "artists",
            _ => "",
        }
        .into(),
    );
    ui.set_lib_sort(
        match v.sort {
            MixSort::Added => "added",
            MixSort::Name => "name",
            MixSort::Updated => "updated",
        }
        .into(),
    );
    ui.set_lib_descending(v.descending);
}

/// Changes the library view, saves it (except the search) and redraws the panel.
fn change_library_view(
    ui: &MainWindow,
    app: &App,
    songs: &LazySongs,
    f: impl FnOnce(&mut drx_app::LibraryView),
) {
    let view = LIB_VIEW.with(|v| {
        let mut v = v.borrow_mut();
        let view = v.get_or_insert_with(|| app.library_view_saved());
        f(view);
        view.clone()
    });
    let _ = app.save_library_view(&view);
    refresh_library(ui, app, songs);
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
    cover_r(app, url, size, 0.0)
}

/// Largest corner of covers in rows and the player bar (logical px), as `Grid.row-cover-radius`.
pub const ROW_COVER_R: f32 = 5.0;

/// The covers' radius (logical px), set when a theme is applied: covers get their corners from it.
static COVER_RADIUS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(12f32.to_bits());

pub fn set_cover_radius(r: f32) {
    COVER_RADIUS.store(r.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// Corner radius in decoded pixels for a cover decoded at `decoded` px and drawn at `shown` logical px:
/// the theme's radius, capped at `max` (small covers get smaller corners), 0 for square themes.
pub fn theme_r(max: f32, decoded: u32, shown: f32) -> f32 {
    let r = f32::from_bits(COVER_RADIUS.load(std::sync::atomic::Ordering::Relaxed)).min(max);
    r * decoded as f32 / shown
}

/// A cover with rounded corners baked into its pixels (`radius` in decoded px; size/2 = a circle). The software
/// renderer does not clip images to rounded rectangles, so the corners are made transparent here, antialiased.
pub fn cover_r(app: &App, url: &str, size: u32, radius: f32) -> Option<slint::Image> {
    match app.image(url, size) {
        Some(p) => {
            let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &p.rgba, p.size, p.size,
            );
            if radius >= 0.5 {
                round_corners(
                    buf.make_mut_slice(),
                    p.size as usize,
                    radius.min(p.size as f32 / 2.0),
                );
            }
            Some(slint::Image::from_rgba8(buf))
        }
        None => {
            app.request_image(url, size);
            None
        }
    }
}

fn round_corners(px: &mut [slint::Rgba8Pixel], n: usize, r: f32) {
    let span = r.ceil() as usize;
    for y in 0..span.min(n) {
        for x in 0..span.min(n) {
            // Distance from the pixel centre to the corner circle's centre.
            let dx = r - (x as f32 + 0.5);
            let dy = r - (y as f32 + 0.5);
            let d = (dx * dx + dy * dy).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            if a >= 1.0 {
                continue;
            }
            for (cx, cy) in [
                (x, y),
                (n - 1 - x, y),
                (x, n - 1 - y),
                (n - 1 - x, n - 1 - y),
            ] {
                let p = &mut px[cy * n + cx];
                p.a = (p.a as f32 * a) as u8;
            }
        }
    }
}

/// «1 234 567» (no-break spaces between thousands).
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{a0}');
        }
        out.push(c);
    }
    out
}

pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
