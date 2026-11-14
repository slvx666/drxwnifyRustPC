//! Applies a theme from `assets/themes.json` (converted from pc/ Theme.kt, shared with Full) to the Slint globals.

use crate::{MainWindow, Theme};
use drx_app::App;
use serde_json::Value;
use slint::{Color, ComponentHandle};

const THEMES: &str = include_str!("../../../assets/themes.json");

/// Setting key for the chosen theme (PC only; Android has its own theme keys).
pub const THEME_KEY: &str = "pcTheme";

pub fn current_theme_id(app: &App) -> String {
    std::fs::read(app.profile().prefs_pb())
        .ok()
        .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
        .and_then(|p| p.get_str(THEME_KEY).map(str::to_string))
        .unwrap_or_else(|| "tategaki".into())
}

/// Setting key for the day (light) variant of the theme (PC only).
pub const DAY_KEY: &str = "pcThemeDay";

pub fn current_day(app: &App) -> bool {
    std::fs::read(app.profile().prefs_pb())
        .ok()
        .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
        .and_then(|p| p.get_bool(DAY_KEY))
        .unwrap_or(false)
}

/// Every theme for the settings page: id, name, accent and background of the night or day variant.
pub fn list(day: bool) -> Vec<(String, String, Color, Color)> {
    let Ok(specs) = serde_json::from_str::<Vec<Value>>(THEMES) else {
        return Vec::new();
    };
    specs
        .iter()
        .map(|s| {
            let t = &s[if day { "day" } else { "night" }];
            (
                s["id"].as_str().unwrap_or("").to_string(),
                t["name"].as_str().unwrap_or("").to_string(),
                color(&t["accent"]),
                color(&t["bg"]),
            )
        })
        .collect()
}

fn color(v: &Value) -> Color {
    let s = v.as_str().unwrap_or("#000000FF").trim_start_matches('#');
    let n = u32::from_str_radix(s, 16).unwrap_or(0xFF);
    let (r, g, b, a) = if s.len() == 8 {
        (n >> 24, n >> 16, n >> 8, n)
    } else {
        (n >> 16, n >> 8, n, 0xFF)
    };
    Color::from_argb_u8(
        (a & 0xFF) as u8,
        (r & 0xFF) as u8,
        (g & 0xFF) as u8,
        (b & 0xFF) as u8,
    )
}

/// Compose font ids in Theme.kt → family names of the embedded fonts.
fn family(id: &str) -> &'static str {
    match id {
        "mono" => "JetBrains Mono",
        "cormorant" => "Cormorant Garamond",
        "russo" => "Russo One",
        "vt323" => "VT323",
        "nunito" => "Nunito",
        "comfortaa" => "Comfortaa",
        "caveat" => "Caveat",
        "spectral" => "Spectral",
        "forum" => "Forum",
        "playfair" => "Playfair Display",
        "jpSerif" => "Noto Serif JP",
        _ => "Manrope",
    }
}

fn weight(w: &str) -> i32 {
    match w {
        "Normal" => 400,
        "Medium" => 500,
        "SemiBold" => 600,
        "Bold" => 700,
        _ => 800,
    }
}

pub fn apply(ui: &MainWindow, id: &str, day: bool) {
    let Ok(specs) = serde_json::from_str::<Vec<Value>>(THEMES) else {
        return;
    };
    let spec = specs
        .iter()
        .find(|s| s["id"] == id)
        .or_else(|| specs.first());
    let Some(spec) = spec else { return };
    let t = &spec[if day { "day" } else { "night" }];
    let g = ui.global::<Theme>();
    g.set_bg(color(&t["bg"]));
    g.set_bg2(color(&t["bg2"]));
    g.set_panel(color(&t["panel"]));
    g.set_panel_border(color(&t["panelBorder"]));
    g.set_fill(color(&t["fill"]));
    g.set_fill_hover(color(&t["fillHover"]));
    g.set_menu(color(&t["menu"]));
    g.set_text(color(&t["text"]));
    g.set_dim(color(&t["dim"]));
    g.set_mute(color(&t["mute"]));
    g.set_accent(color(&t["accent"]));
    g.set_accent2(color(&t["accent2"]));
    g.set_on_accent(color(&t["onAccent"]));
    g.set_glyph(color(&t["glyph"]));
    if let Some(m) = t["mist"].as_array() {
        g.set_mist1(color(&m[0]));
        g.set_mist2(color(&m[1]));
    }
    let px = |k: &str| t[k].as_f64().unwrap_or(0.0) as f32;
    g.set_panel_radius(px("panelRadius"));
    g.set_cover_radius(px("coverRadius"));
    crate::set_cover_radius(px("coverRadius"));
    g.set_chip_radius(px("chipRadius"));
    g.set_body_font(family(t["body"].as_str().unwrap_or("manrope")).into());
    g.set_heading_font(family(t["heading"].as_str().unwrap_or("manrope")).into());
    g.set_heading_weight(weight(t["headingWeight"].as_str().unwrap_or("ExtraBold")));
    g.set_mono_font(family(t["mono"].as_str().unwrap_or("mono")).into());
    g.set_look(t["look"].as_str().unwrap_or("Glass").into());
    g.set_backdrop(t["backdrop"].as_str().unwrap_or("Tategaki").into());
}
