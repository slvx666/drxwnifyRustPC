//! The Drxwnify Mini installer: one exe with Mini inside. Installs (or updates) Mini for the current user — no
//! administrator rights — with a Start menu shortcut and an entry in «Apps» for removal; asks about a desktop shortcut.
//! `--quiet`: no questions, no launch (checks; with DRX_SETUP_ROOT everything goes into a test folder).
#![cfg_attr(not(test), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use drx_platform::setup::{APP_NAME, Layout, ask, install, tell};
    use std::io::Read;
    const MINI: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/mini.xz"));
    let version = env!("CARGO_PKG_VERSION");
    let quiet = std::env::args().any(|a| a == "--quiet");
    let Some(layout) = Layout::current() else {
        if !quiet {
            tell(APP_NAME, "Не удалось найти папки пользователя Windows.");
        }
        std::process::exit(1);
    };
    let update = layout.installed_version();
    if !quiet {
        let q = match &update {
            Some(old) => format!("Обновить {APP_NAME} с версии {old} до {version}?"),
            None => format!(
                "Установить {APP_NAME} {version} для этого пользователя Windows?\n\nПрограмма появится в меню «Пуск». Права администратора не нужны."
            ),
        };
        if !ask(APP_NAME, &q) {
            return;
        }
    }
    let desktop = if update.is_some() || quiet {
        layout.desktop_link.exists()
    } else {
        ask(APP_NAME, "Создать ярлык на рабочем столе?")
    };
    let mut exe = Vec::new();
    if let Err(e) = liblzma::read::XzDecoder::new(MINI).read_to_end(&mut exe) {
        if !quiet {
            tell(APP_NAME, &format!("Установщик повреждён: {e}"));
        }
        std::process::exit(1);
    }
    if let Err(e) = install(&layout, &exe, version, desktop) {
        if !quiet {
            tell(APP_NAME, &format!("Не удалось установить: {e}"));
        }
        std::process::exit(1);
    }
    if quiet {
        return;
    }
    let done = format!(
        "{APP_NAME} {version} установлен.\n\nСсылки drxwnify:// с сайта (приглашения друзей, музыка) открывают программу после её первого запуска. Если Chrome уже был открыт, перезапустите его полностью: меню → «Выход», затем снова открыть.\n\nЗапустить {APP_NAME} сейчас?"
    );
    if ask(APP_NAME, &done) {
        let _ = std::process::Command::new(&layout.exe).spawn();
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the Mini installer is for Windows");
}
