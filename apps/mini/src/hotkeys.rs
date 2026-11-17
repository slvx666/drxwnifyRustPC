//! Keys for the player that work anywhere in Windows (pc/ Hotkeys.kt): pause, next, previous, like, radio, each
//! with its own combination, set in the settings by pressing it. Stored in the settings file as `pcHotkey_<id>`
//! ("ctrl+alt+shift+82"; "" = off, missing or "default" = the default). One combination does one thing: setting it
//! takes it from the action that had it. With «Глобальные» off they work only while the window has focus.

use drx_app::App;
use drx_platform::Hotkey;
use std::sync::Mutex;

pub const TOGGLE: u32 = 1;
pub const NEXT: u32 = 2;
pub const PREV: u32 = 3;
pub const LIKE: u32 = 4;
pub const RADIO: u32 = 5;

/// (id, settings key, label, default key). Defaults: Ctrl+Alt+Shift — nobody presses that by accident.
pub const ACTIONS: &[(u32, &str, &str, u32)] = &[
    (TOGGLE, "playPause", "Пауза / играть", 0x20),
    (NEXT, "next", "Следующий трек", 0x27),
    (PREV, "previous", "Предыдущий трек", 0x25),
    (LIKE, "like", "В любимые / убрать из любимых", 0x4C),
    (RADIO, "radio", "Радио от текущего трека", 0x52),
];

/// Actions whose combination another program holds (filled when registering fails).
static TAKEN: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn pref_key(id: u32) -> Option<String> {
    ACTIONS
        .iter()
        .find(|a| a.0 == id)
        .map(|a| format!("pcHotkey_{}", a.1))
}

fn default_of(id: u32) -> Option<Hotkey> {
    ACTIONS.iter().find(|a| a.0 == id).map(|a| Hotkey {
        id,
        ctrl: true,
        alt: true,
        shift: true,
        key: a.3,
    })
}

pub fn encode(h: &Hotkey) -> String {
    let mut p = Vec::new();
    if h.ctrl {
        p.push("ctrl".to_string());
    }
    if h.alt {
        p.push("alt".to_string());
    }
    if h.shift {
        p.push("shift".to_string());
    }
    p.push(h.key.to_string());
    p.join("+")
}

pub fn decode(id: u32, s: &str) -> Option<Hotkey> {
    let parts: Vec<&str> = s.split('+').collect();
    let key = parts.last()?.parse().ok()?;
    let has = |m: &str| parts[..parts.len() - 1].contains(&m);
    Some(Hotkey {
        id,
        ctrl: has("ctrl"),
        alt: has("alt"),
        shift: has("shift"),
        key,
    })
}

/// The combination of an action now (None = switched off).
pub fn combo_of(app: &App, id: u32) -> Option<Hotkey> {
    let prefs = app.prefs();
    match prefs.get_str(&pref_key(id)?) {
        None | Some("default") => default_of(id),
        Some("") => None,
        Some(s) => decode(id, s),
    }
}

pub fn is_default(app: &App, id: u32) -> bool {
    let (a, b) = (combo_of(app, id), default_of(id));
    match (a, b) {
        (Some(a), Some(b)) => encode(&a) == encode(&b),
        _ => false,
    }
}

/// Every action that has a combination.
pub fn all(app: &App) -> Vec<Hotkey> {
    ACTIONS.iter().filter_map(|a| combo_of(app, a.0)).collect()
}

fn store(app: &App, id: u32, value: &str) {
    if let Some(k) = pref_key(id) {
        let _ = app.set_pref(&k, drx_core::prefs::PrefValue::String(value.to_string()));
    }
}

/// Sets (Some) or switches off (None) an action's combination; whoever had it before loses it.
pub fn set(app: &App, id: u32, combo: Option<Hotkey>) {
    if let Some(c) = &combo {
        for a in ACTIONS.iter().filter(|a| a.0 != id) {
            if combo_of(app, a.0).is_some_and(|o| encode(&o) == encode(c)) {
                store(app, a.0, "");
            }
        }
    }
    store(app, id, &combo.map(|c| encode(&c)).unwrap_or_default());
}

pub fn reset(app: &App, id: u32) {
    store(app, id, "default");
}

/// «Ctrl + Alt + Shift + R».
pub fn label(h: &Hotkey) -> String {
    let mut p: Vec<String> = Vec::new();
    if h.ctrl {
        p.push("Ctrl".into());
    }
    if h.alt {
        p.push("Alt".into());
    }
    if h.shift {
        p.push("Shift".into());
    }
    p.push(key_name(h.key));
    p.join(" + ")
}

fn key_name(vk: u32) -> String {
    match vk {
        0x20 => "Пробел".into(),
        0x25 => "←".into(),
        0x26 => "↑".into(),
        0x27 => "→".into(),
        0x28 => "↓".into(),
        0x0D => "Enter".into(),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk).map(String::from).unwrap_or_default(),
        _ => format!("#{vk}"),
    }
}

/// The Windows virtual-key code of a key Slint reports as `text`, independent of the keyboard layout (the Russian
/// layout's letters map to the keys they sit on). None for a key that can't be a hotkey.
pub fn vk_of(text: &str) -> Option<u32> {
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбю";
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.";
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let lower = c.to_lowercase().next().unwrap_or(c);
    let c = match RU.chars().position(|r| r == lower) {
        Some(i) => EN.chars().nth(i)?,
        None => lower,
    };
    match c {
        'a'..='z' => Some(c.to_ascii_uppercase() as u32),
        '0'..='9' => Some(c as u32),
        ' ' => Some(0x20),
        '\n' | '\r' => Some(0x0D),
        '\u{F700}' => Some(0x26),
        '\u{F701}' => Some(0x28),
        '\u{F702}' => Some(0x25),
        '\u{F703}' => Some(0x27),
        '\u{F704}'..='\u{F70F}' => Some(0x70 + (c as u32 - 0xF704)),
        _ => None,
    }
}

pub fn mark_taken(id: u32) {
    let mut t = TAKEN.lock().expect("taken");
    if !t.contains(&id) {
        t.push(id);
    }
}

pub fn clear_taken() {
    TAKEN.lock().expect("taken").clear();
}

pub fn is_taken(id: u32) -> bool {
    TAKEN.lock().expect("taken").contains(&id)
}

/// The action of a combination pressed in the window (when the keys are not registered system-wide).
pub fn action_for(app: &App, ctrl: bool, alt: bool, shift: bool, vk: u32) -> Option<u32> {
    if !(ctrl || alt || shift) {
        return None;
    }
    all(app)
        .into_iter()
        .find(|h| h.ctrl == ctrl && h.alt == alt && h.shift == shift && h.key == vk)
        .map(|h| h.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        let h = Hotkey { id: 4, ctrl: true, alt: false, shift: true, key: 0x4C };
        assert_eq!(encode(&h), "ctrl+shift+76");
        let d = decode(4, "ctrl+shift+76").unwrap();
        assert!(d.ctrl && !d.alt && d.shift && d.key == 0x4C);
        assert_eq!(label(&h), "Ctrl + Shift + L");
    }

    #[test]
    fn keys_are_layout_free() {
        assert_eq!(vk_of("l"), Some(0x4C));
        assert_eq!(vk_of("Д"), Some(0x4C)); // the L key on the Russian layout
        assert_eq!(vk_of("к"), Some(0x52)); // R
        assert_eq!(vk_of(" "), Some(0x20));
        assert_eq!(vk_of("\u{F703}"), Some(0x27));
        assert_eq!(vk_of("\u{F704}"), Some(0x70));
        assert_eq!(vk_of("\u{1b}"), None);
    }
}
