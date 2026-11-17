//! Content settings shared with the phone (settings.preferences_pb): audio quality, the content language and
//! country of YouTube Music, content filters, and which audio sources are used and in what order.

use crate::App;
use drx_core::prefs::PrefValue;

pub const AUDIO_QUALITY_KEY: &str = "audioQuality";
pub const CONTENT_LANGUAGE_KEY: &str = "contentLanguage";
pub const CONTENT_COUNTRY_KEY: &str = "contentCountry";
pub const HIDE_EXPLICIT_KEY: &str = "hideExplicit";
pub const HIDE_VIDEO_KEY: &str = "hideVideoSongs";
pub const SOURCE_ORDER_KEY: &str = "audioSourceOrder";
/// The phone's SYSTEM_DEFAULT.
pub const SYSTEM_DEFAULT: &str = "SYSTEM_DEFAULT";

/// AudioQuality: AUTO, HIGH, LOW.
pub const QUALITIES: [(&str, &str); 3] = [("AUTO", "Авто"), ("HIGH", "Высокое"), ("LOW", "Низкое")];
/// Content languages offered (hl).
pub const LANGUAGES: [(&str, &str); 5] =
    [(SYSTEM_DEFAULT, "Как в системе"), ("ru", "Русский"), ("en", "English"), ("uk", "Українська"), ("kk", "Қазақша")];
/// Content countries offered (gl).
pub const COUNTRIES: [(&str, &str); 7] = [
    (SYSTEM_DEFAULT, "Как в системе"),
    ("RU", "Россия"),
    ("US", "США"),
    ("UA", "Украина"),
    ("KZ", "Казахстан"),
    ("BY", "Беларусь"),
    ("DE", "Германия"),
];

/// Audio sources this PC has (AudioProviderId name, switch key, label), in the phone's default order.
pub const SOURCES: [(&str, &str, &str); 3] = [
    ("SOUNDCLOUD", "audioSourceSoundCloud", "SoundCloud"),
    ("YOUTUBE", "audioSourceYouTube", "YouTube"),
    ("VK", "audioSourceVk", "ВКонтакте"),
];

impl App {
    /// Applies quality and content locale (`system`: the computer's language and country for SYSTEM_DEFAULT).
    pub fn apply_content_prefs(&self, system: Option<(String, String)>) {
        let low = self.pref_string(AUDIO_QUALITY_KEY).as_deref() == Some("LOW");
        drx_resolver::providers::youtube::LOW_QUALITY.store(low, std::sync::atomic::Ordering::Relaxed);
        let (sys_hl, sys_gl) = system.unwrap_or_else(|| ("ru".into(), "RU".into()));
        let hl = self.pref_string(CONTENT_LANGUAGE_KEY).filter(|v| !v.is_empty() && v != SYSTEM_DEFAULT).unwrap_or(sys_hl);
        let gl = self.pref_string(CONTENT_COUNTRY_KEY).filter(|v| !v.is_empty() && v != SYSTEM_DEFAULT).unwrap_or(sys_gl);
        drx_resolver::providers::youtube::set_content_locale(&hl, &gl);
    }

    pub fn hide_explicit(&self) -> bool {
        self.pref_bool(HIDE_EXPLICIT_KEY).unwrap_or(false)
    }

    pub fn hide_video(&self) -> bool {
        self.pref_bool(HIDE_VIDEO_KEY).unwrap_or(false)
    }

    /// The audio sources in order: (id, label, on). Unknown names in the phone's list (Bandcamp, Soulseek…) are
    /// kept in the setting but not shown.
    pub fn source_order(&self) -> Vec<(&'static str, &'static str, bool)> {
        let saved = self.pref_string(SOURCE_ORDER_KEY).unwrap_or_default();
        let mut ids: Vec<&'static str> = saved
            .split(',')
            .filter_map(|n| SOURCES.iter().find(|s| s.0 == n.trim()).map(|s| s.0))
            .collect();
        for s in SOURCES {
            if !ids.contains(&s.0) {
                ids.push(s.0);
            }
        }
        ids.into_iter()
            .filter_map(|id| SOURCES.iter().find(|s| s.0 == id))
            .map(|(id, key, label)| (*id, *label, self.pref_bool(key).unwrap_or(true)))
            .collect()
    }

    /// Switches a source on or off; the player picks the new set at once.
    pub fn set_source_on(&self, id: &str, on: bool) {
        if let Some((_, key, _)) = SOURCES.iter().find(|s| s.0 == id) {
            let _ = self.set_pref(key, PrefValue::Bool(on));
            self.install_providers();
        }
    }

    /// Moves a source up (-1) or down (+1); the phone's other names stay at the end of the saved list.
    pub fn move_source(&self, id: &str, delta: i32) {
        let mut ids: Vec<&str> = self.source_order().into_iter().map(|s| s.0).collect();
        let Some(at) = ids.iter().position(|x| *x == id) else { return };
        let to = (at as i32 + delta).clamp(0, ids.len() as i32 - 1) as usize;
        let x = ids.remove(at);
        ids.insert(to, x);
        let saved = self.pref_string(SOURCE_ORDER_KEY).unwrap_or_default();
        let others: Vec<&str> = saved.split(',').map(str::trim).filter(|n| !n.is_empty() && !ids.contains(n)).collect();
        let all: Vec<&str> = ids.into_iter().chain(others).collect();
        let _ = self.set_pref(SOURCE_ORDER_KEY, PrefValue::String(all.join(",")));
        self.install_providers();
    }
}
