//! App icon variants per edition. Full offers a choice (setting `pcAppIcon`); Mini has only its own icons
//! (for now one) and never shows a Full icon. Files live in `assets/icons/<edition>/<id>.{png,ico}`.

/// Prefs key holding the chosen icon id.
pub const PREF_KEY: &str = "pcAppIcon";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppIcon {
    /// Stable id stored in the setting and used as the file name.
    pub id: &'static str,
    /// Name shown in the picker.
    pub name: &'static str,
}

/// Full's icons; the first one is the default.
pub const FULL: &[AppIcon] = &[
    AppIcon { id: "halo", name: "Нимб" },
    AppIcon { id: "clean", name: "Чистый" },
    AppIcon { id: "grunge", name: "Гранж" },
    AppIcon { id: "street", name: "Улица" },
    AppIcon { id: "paper", name: "Афиши" },
];

/// Mini's icons; the first one is the default.
pub const MINI: &[AppIcon] = &[AppIcon { id: "d", name: "D" }];

/// The icons an edition may use.
pub fn for_edition(full: bool) -> &'static [AppIcon] {
    if full {
        FULL
    } else {
        MINI
    }
}

/// The icon to show: the saved one when this edition has it, else the edition's default.
pub fn resolve(full: bool, saved: Option<&str>) -> AppIcon {
    let list = for_edition(full);
    saved
        .and_then(|id| list.iter().find(|i| i.id == id))
        .copied()
        .unwrap_or(list[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mini_never_takes_a_full_icon() {
        assert_eq!(resolve(false, Some("halo")).id, "d");
        assert_eq!(resolve(false, None).id, "d");
    }

    #[test]
    fn full_keeps_a_valid_choice_and_falls_back() {
        assert_eq!(resolve(true, Some("paper")).id, "paper");
        assert_eq!(resolve(true, Some("d")).id, "halo");
        assert_eq!(resolve(true, Some("gone")).id, "halo");
    }
}
