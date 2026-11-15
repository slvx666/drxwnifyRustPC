//! The settings page in Mini: accounts (main one, which are in the library) and the look (theme, day/night).
//! Every change is written to `settings.preferences_pb` at once and applied without a restart.

use crate::{AccountRow, MainWindow, ThemeChip};
use drx_app::App;
use drx_catalog::model::Source;
use slint::{ModelRc, VecModel};

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
    fill_hotkeys(ui, app);
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
