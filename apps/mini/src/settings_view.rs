//! The settings page in Mini: accounts (main one, which are in the library) and the look (theme, day/night).
//! Every change is written to `settings.preferences_pb` at once and applied without a restart.

use crate::{AccountRow, MainWindow, ThemeChip};
use drx_app::App;
use drx_catalog::model::Source;
use slint::{ComponentHandle, ModelRc, VecModel};

fn key_of(s: Source) -> &'static str {
    match s {
        Source::Spotify => "SPOTIFY",
        Source::Yandex => "YANDEX",
        Source::Vk => "VK",
    }
}

pub fn source_of(key: &str) -> Option<Source> {
    match key {
        "SPOTIFY" => Some(Source::Spotify),
        "YANDEX" => Some(Source::Yandex),
        "VK" => Some(Source::Vk),
        _ => None,
    }
}

/// Fills the page from the current settings.
pub fn fill(ui: &MainWindow, app: &App) {
    let accounts: Vec<AccountRow> = app
        .accounts_state()
        .into_iter()
        .map(|a| AccountRow {
            key: key_of(a.source).into(),
            label: a.label.into(),
            linked: a.linked,
            main: a.main,
            tab: a.tab,
        })
        .collect();
    ui.set_settings_accounts(ModelRc::new(VecModel::from(accounts)));
    let day = crate::themes::current_day(app);
    let current = crate::themes::current_theme_id(app);
    let themes: Vec<ThemeChip> = crate::themes::list(day)
        .into_iter()
        .map(|(id, name, accent, bg)| ThemeChip {
            selected: id == current,
            id: id.into(),
            name: name.into(),
            accent,
            bg,
        })
        .collect();
    ui.set_settings_themes(ModelRc::new(VecModel::from(themes)));
    ui.set_day_theme(day);
    ui.set_global_hotkeys(crate::os::hotkeys_on(app));
    ui.set_autostart(crate::os::autostart_on());
    fill_flags(ui, app);
    fill_hotkeys(ui, app);
    crate::sound_ui::fill(ui, app);
}

/// The playback switches (booleans of settings.preferences_pb, the phone's keys): (key, label, hint, default).
const FLAGS: &[(&str, &str, &str, bool)] = &[
    ("autoSkipNextOnError", "Пропускать трек, если не включается", "Иначе воспроизведение останавливается на нём", false),
    ("autoLoadMore", "Продолжать похожими", "Когда очередь кончается, следом идёт радио по последнему треку", true),
    (drx_app::sponsorblock::ENABLED_KEY, "Пропускать немузыкальные вставки", "SponsorBlock: разговоры и заставки в клипах с YouTube", false),
    ("persistentQueue", "Сохранять очередь", "После перезапуска — тот же трек с того же места", true),
    (crate::discord_ui::KEY, "Статус в Discord", "Что играет — в профиле Discord (нужен запущенный Discord на этом ПК)", false),
];

fn switch(key: &str, label: &str, hint: &str, on: bool, header: &str) -> crate::FlagItem {
    crate::FlagItem {
        key: key.into(),
        label: label.into(),
        hint: hint.into(),
        on,
        header: header.into(),
        options: ModelRc::default(),
        marks: ModelRc::default(),
    }
}

/// A row with a caption and a word, without a control.
fn switch_info(label: &str, hint: &str, header: &str) -> crate::FlagItem {
    let mut it = choice("info", label, hint, &[], Vec::new(), header);
    it.options = ModelRc::default();
    it
}

fn choice(key: &str, label: &str, hint: &str, options: &[&str], marks: Vec<bool>, header: &str) -> crate::FlagItem {
    let options: Vec<slint::SharedString> = options.iter().map(|o| (*o).into()).collect();
    crate::FlagItem {
        key: key.into(),
        label: label.into(),
        hint: hint.into(),
        on: false,
        header: header.into(),
        options: ModelRc::new(VecModel::from(options)),
        marks: ModelRc::new(VecModel::from(marks)),
    }
}

/// «Засчитывать прослушивание после» (historyDuration, seconds; the phone's slider goes 1..100).
const HISTORY_SECONDS: [f32; 5] = [10.0, 20.0, 30.0, 45.0, 60.0];
/// The audiences this side offers (VERIFIED is the developer's own).
const AUDIENCES: [(drx_social::friends::Audience, &str); 3] = [
    (drx_social::friends::Audience::Nobody, "Никто"),
    (drx_social::friends::Audience::Friends, "Друзья"),
    (drx_social::friends::Audience::Everyone, "Все"),
];

fn sponsor_categories(app: &App) -> Vec<String> {
    app.pref_string(drx_app::sponsorblock::CATEGORIES_PREF)
        .unwrap_or_else(|| drx_app::sponsorblock::DEFAULT.into())
        .split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

pub fn fill_flags(ui: &MainWindow, app: &App) {
    let rows: Vec<crate::FlagItem> = FLAGS
        .iter()
        .enumerate()
        .map(|(i, (key, label, hint, default))| {
            switch(key, label, hint, app.pref_bool(key).unwrap_or(*default), if i == 0 { "ВОСПРОИЗВЕДЕНИЕ" } else { "" })
        })
        .collect();
    ui.set_settings_flags(ModelRc::new(VecModel::from(rows)));

    let mut more = Vec::new();
    let secs = app.pref_f32(drx_app::HISTORY_DURATION_KEY).unwrap_or(30.0);
    let near = HISTORY_SECONDS
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - secs).abs().total_cmp(&(b.1 - secs).abs()))
        .map_or(2, |(i, _)| i);
    more.push(switch(
        drx_app::PAUSE_HISTORY_KEY,
        "Не записывать историю прослушиваний",
        "Пока включено, прослушанное не попадает в историю и к друзьям",
        app.pref_bool(drx_app::PAUSE_HISTORY_KEY).unwrap_or(false),
        "ИСТОРИЯ",
    ));
    more.push(choice(
        drx_app::HISTORY_DURATION_KEY,
        "Засчитывать прослушивание после",
        "Короткие треки — почти целиком",
        &["10 с", "20 с", "30 с", "45 с", "60 с"],
        (0..HISTORY_SECONDS.len()).map(|i| i == near).collect(),
        "",
    ));
    more.push(switch(
        "pauseSearchHistory",
        "Не запоминать поиски",
        "Новые запросы не попадают в историю поиска",
        app.pref_bool("pauseSearchHistory").unwrap_or(false),
        "",
    ));
    more.push(switch("enableLrclib", "LRCLIB", "Синхронные тексты, основной источник", app.pref_bool("enableLrclib").unwrap_or(true), "ТЕКСТЫ"));
    more.push(switch("enableKugou", "KuGou", "Если в LRCLIB нет синхронного текста", app.pref_bool("enableKugou").unwrap_or(true), ""));
    let cats = sponsor_categories(app);
    let names: Vec<&str> = drx_app::sponsorblock::CATEGORIES.iter().map(|(_, n)| *n).collect();
    more.push(choice(
        "sb:cats",
        "Что пропускать",
        "В клипах с YouTube, когда включено «Пропускать немузыкальные вставки»",
        &names,
        drx_app::sponsorblock::CATEGORIES.iter().map(|(k, _)| cats.iter().any(|c| c == k)).collect(),
        "SPONSORBLOCK",
    ));
    more.push(switch(
        drx_app::sponsorblock::TOAST_KEY,
        "Сообщать о пропуске",
        "Короткое уведомление, что именно пропущено",
        app.pref_bool(drx_app::sponsorblock::TOAST_KEY).unwrap_or(false),
        "",
    ));
    // Last.fm (the phone's LastFMSettings): this PC's own sign-in through the browser, then its switches.
    let lf = app.lastfm_state();
    match (&lf.user, lf.available) {
        (_, false) => more.push(switch_info("Last.fm", "Нет ключа приложения Last.fm в этой сборке", "LAST.FM")),
        (None, true) => {
            more.push(choice("lastfm:account", "Аккаунт Last.fm", "Вход через браузер: подтвердите доступ на странице Last.fm", &["Войти через Last.fm"], vec![false], "LAST.FM"));
        }
        (Some(u), true) => {
            more.push(choice("lastfm:account", &format!("Аккаунт: {u}"), "", &["Выйти"], vec![false], "LAST.FM"));
            more.push(switch(drx_app::lastfm::SCROBBLE_KEY, "Скробблинг", "Прослушанное попадает в профиль Last.fm (после половины трека или 3 минут)", lf.scrobble, ""));
            more.push(switch(drx_app::lastfm::NOW_PLAYING_KEY, "«Сейчас слушает»", "Last.fm показывает, что играет прямо сейчас", lf.now_playing, ""));
            more.push(switch(drx_app::lastfm::LOVE_KEY, "Отправлять лайки", "Лайк трека — «love» в Last.fm", lf.love, ""));
        }
    }
    if let Some(f) = app.friends() {
        let p = f.privacy();
        let marks = |a: drx_social::friends::Audience| AUDIENCES.iter().map(|(x, _)| *x == a).collect::<Vec<bool>>();
        let opts: Vec<&str> = AUDIENCES.iter().map(|(_, n)| *n).collect();
        more.push(choice("privacy:now", "Кто видит, что я слушаю", "То, что публикует этот компьютер", &opts, marks(p.now), "ДРУЗЬЯ И ПРИВАТНОСТЬ"));
        more.push(choice("privacy:history", "Кто видит историю прослушиваний", "", &opts, marks(p.history), ""));
        more.push(switch("privacy:along", "Разрешить слушать вместе со мной", "Друзья могут включить «Слушать вместе»", p.listen_along, ""));
        more.push(switch("privacy:read", "Отметки «прочитано» и «печатает…»", "Друзья видят, что вы прочитали и пишете ответ", p.read_receipts, ""));
        more.push(switch("chat:notify", "Уведомления о сообщениях", "Уведомление Windows, когда окно свёрнуто", app.chat_notify_on(), ""));
    }
    ui.global::<crate::SettingsUi>().set_more(ModelRc::new(VecModel::from(more)));
}

fn set_flag(ui: &MainWindow, app: &App, key: &str, on: bool) {
    match key {
        "privacy:along" | "privacy:read" => {
            if let Some(f) = app.friends() {
                let mut p = f.privacy();
                if key == "privacy:along" {
                    p.listen_along = on;
                } else {
                    p.read_receipts = on;
                }
                app.set_privacy(p);
            }
        }
        "chat:notify" => app.set_chat_notify(on),
        _ => {
            if let Err(e) = app.set_pref(key, drx_core::prefs::PrefValue::Bool(on)) {
                ui.set_toast(format!("Не удалось сохранить: {e}").into());
            }
            app.apply_playback_prefs();
            if key == "persistentQueue" {
                app.save_queue();
            }
            if key == crate::discord_ui::KEY {
                crate::discord_ui::set_enabled(app, on);
            }
        }
    }
    fill_flags(ui, app);
}

fn pick(ui: &MainWindow, app: &App, key: &str, i: usize) {
    use drx_core::prefs::PrefValue;
    match key {
        drx_app::HISTORY_DURATION_KEY => {
            if let Some(s) = HISTORY_SECONDS.get(i) {
                let _ = app.set_pref(key, PrefValue::Float(*s));
            }
        }
        "sb:cats" => {
            if let Some((cat, _)) = drx_app::sponsorblock::CATEGORIES.get(i) {
                let mut cats = sponsor_categories(app);
                if let Some(at) = cats.iter().position(|c| c == cat) {
                    cats.remove(at);
                } else {
                    cats.push((*cat).to_string());
                }
                let _ = app.set_pref(drx_app::sponsorblock::CATEGORIES_PREF, PrefValue::String(cats.join(",")));
            }
        }
        "lastfm:account" => {
            if app.lastfm_state().user.is_some() {
                app.lastfm_sign_out();
                ui.set_toast("Вы вышли из Last.fm".into());
            } else {
                let (app2, ui_w) = (app.clone(), ui.as_weak());
                app.runtime().spawn(async move {
                    let begun = app2.lastfm_begin().await;
                    let (token, url) = match begun {
                        Ok(x) => x,
                        Err(e) => {
                            let _ = ui_w.upgrade_in_event_loop(move |ui| ui.set_toast(format!("Не получилось: {e}").into()));
                            return;
                        }
                    };
                    let _ = ui_w.upgrade_in_event_loop(move |ui| {
                        let opened = drx_platform::open_url(&url);
                        ui.set_toast(if opened { "Подтвердите вход на странице Last.fm в браузере" } else { "Не открылся браузер" }.into());
                    });
                    let done = app2.lastfm_finish(&token).await;
                    let app3 = app2.clone();
                    let _ = ui_w.upgrade_in_event_loop(move |ui| {
                        ui.set_toast(match done {
                            Ok(name) => format!("Last.fm: вы вошли как {name}"),
                            Err(e) => format!("Last.fm: {e}"),
                        }.into());
                        fill_flags(&ui, &app3);
                    });
                });
            }
        }
        "privacy:now" | "privacy:history" => {
            if let (Some(f), Some((a, _))) = (app.friends(), AUDIENCES.get(i)) {
                let mut p = f.privacy();
                if key == "privacy:now" {
                    p.now = *a;
                } else {
                    p.history = *a;
                }
                app.set_privacy(p);
            }
        }
        _ => {}
    }
    fill_flags(ui, app);
}

pub fn wire(ui: &MainWindow, app: &App) {

    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_set_settings_flag(move |key, on| {
            if let Some(ui) = ui_w.upgrade() {
                set_flag(&ui, &app2, &key, on);
            }
        });
    }
    let g = ui.global::<crate::SettingsUi>();
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        g.on_set_flag(move |key, on| {
            if let Some(ui) = ui_w.upgrade() {
                set_flag(&ui, &app2, &key, on);
            }
        });
    }
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    g.on_pick(move |key, i| {
        if let Some(ui) = ui_w.upgrade() {
            pick(&ui, &app2, &key, i.max(0) as usize);
        }
    });
}

/// The hotkey rows: each action's combination and how it works now.
pub fn fill_hotkeys(ui: &MainWindow, app: &App) {
    use crate::hotkeys;
    let global = crate::os::hotkeys_on(app);
    let rows: Vec<crate::HotkeyRow> = hotkeys::ACTIONS
        .iter()
        .map(|(id, _, label, _)| {
            let combo = hotkeys::combo_of(app, *id);
            let taken = global && hotkeys::is_taken(*id);
            let note = match &combo {
                None => "выключено",
                Some(_) if taken => "сочетание занято другой программой — выберите другое",
                Some(_) if global => "работает везде",
                Some(_) => "работает в окне приложения",
            };
            crate::HotkeyRow {
                id: *id as i32,
                label: (*label).into(),
                combo: combo.as_ref().map(hotkeys::label).unwrap_or_default().into(),
                note: note.into(),
                is_default: hotkeys::is_default(app, *id),
                taken,
            }
        })
        .collect();
    ui.set_hotkey_rows(ModelRc::new(VecModel::from(rows)));
}

/// Re-applies the theme from the settings (after a theme or day/night change).
pub fn apply_theme(ui: &MainWindow, app: &App) {
    let day = crate::themes::current_day(app);
    crate::themes::apply(ui, &crate::themes::current_theme_id(app), day);
    ui.set_day_theme(day);
}
