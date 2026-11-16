//! Sound settings (Android PlayerSettings and the equaliser screen): loudness normalisation, crossfade (length,
//! not between tracks of one album), stereo balance and the 10-band graphic equaliser with the phone's presets.
//! Keys are the phone's: `audioNormalization`, `crossfadeEnabled`, `crossfadeDurationFloat`, `crossfadeGapless`,
//! `stereoBalance` in settings.preferences_pb; equaliser profiles in `shared_prefs/nanosonic_eq_profiles.xml`
//! (EQProfileRepository: `eq_profiles` JSON, `active_profile_id`), so a backup carries them both ways.

use crate::App;
use crate::playback::Transitions;
use drx_audio::dsp::{Band, EqProfile, FilterType};
use drx_core::prefs::PrefValue;
use drx_core::shared_prefs::SpValue;
use serde::{Deserialize, Serialize};

pub const NORMALIZE_KEY: &str = "audioNormalization";
pub const CROSSFADE_KEY: &str = "crossfadeEnabled";
pub const CROSSFADE_LEN_KEY: &str = "crossfadeDurationFloat";
pub const CROSSFADE_ALBUM_KEY: &str = "crossfadeGapless";
pub const BALANCE_KEY: &str = "stereoBalance";

const EQ_FILE: &str = "nanosonic_eq_profiles";
const EQ_PROFILES: &str = "eq_profiles";
const EQ_ACTIVE: &str = "active_profile_id";
/// GraphicEq.PROFILE_ID: the 10-band equaliser is saved as this parametric profile.
pub const GRAPHIC_ID: &str = "meld_graphic";

/// GraphicEq.FREQUENCIES / LABELS.
pub const FREQUENCIES: [f64; 10] = [32.0, 64.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
pub const LABELS: [&str; 10] = ["32", "64", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
pub const MAX_DB: f32 = 12.0;

/// GraphicEq.Preset with the phone's Russian names.
pub const PRESETS: [(&str, [f32; 10]); 16] = [
    ("Ровно", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Бас", [7.0, 6.0, 4.0, 1.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Вокал", [-2.0, -2.0, -1.0, 0.0, 1.0, 2.5, 4.5, 3.0, 1.0, 0.0]),
    ("Гитара", [-1.0, 0.0, 1.0, -2.0, 0.0, 1.5, 3.5, 4.0, 2.0, 0.0]),
    ("Бас-гитара", [3.0, 5.0, 4.0, 2.0, 0.0, 1.5, 2.5, 0.0, -1.0, -2.0]),
    ("Ударные", [4.0, 5.0, 2.0, -2.0, -2.0, 0.0, 2.0, 4.0, 3.0, 2.0]),
    ("Фортепиано", [0.0, 1.0, 2.0, 2.0, 1.0, 0.0, 1.5, 2.5, 2.0, 1.0]),
    ("Струнные", [-1.0, 0.0, 1.0, 2.0, 2.0, 1.0, 1.0, 2.5, 3.0, 3.0]),
    ("Речь / подкасты", [-6.0, -4.0, -1.0, 1.0, 2.5, 3.5, 3.5, 2.0, 0.0, -2.0]),
    ("Метал", [4.0, 4.0, 2.0, -1.0, -3.0, -2.0, 1.0, 3.5, 4.0, 3.0]),
    ("Электроника", [6.0, 5.0, 2.0, 0.0, -1.0, 0.0, 1.0, 2.5, 4.0, 5.0]),
    ("Воздух (верха)", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 2.0, 4.0, 6.0]),
    ("Ночной (тихо)", [-4.0, -3.0, -1.0, 0.0, 1.0, 1.5, 1.0, -1.0, -3.0, -4.0]),
    ("Динамик телефона", [-12.0, -10.0, -4.0, 1.0, 2.0, 3.0, 3.0, 2.0, 1.0, 0.0]),
    ("Старое радио", [-12.0, -12.0, -8.0, -2.0, 3.0, 5.0, 3.0, -6.0, -12.0, -12.0]),
    ("Под водой", [6.0, 6.0, 4.0, 2.0, -2.0, -6.0, -10.0, -12.0, -12.0, -12.0]),
];

/// ParametricEQBand as the phone serialises it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedBand {
    pub frequency: f64,
    pub gain: f64,
    #[serde(default = "default_q")]
    pub q: f64,
    #[serde(default = "default_filter")]
    pub filter_type: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn default_q() -> f64 {
    1.41
}
fn default_filter() -> String {
    "PK".into()
}
fn yes() -> bool {
    true
}

/// SavedEQProfile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub device_model: String,
    #[serde(default)]
    pub bands: Vec<SavedBand>,
    #[serde(default)]
    pub preamp: f64,
    #[serde(default)]
    pub is_custom: bool,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub added_timestamp: i64,
}

impl SavedProfile {
    pub fn to_engine(&self) -> EqProfile {
        EqProfile {
            preamp_db: self.preamp,
            bands: self
                .bands
                .iter()
                .map(|b| Band {
                    frequency: b.frequency,
                    gain_db: b.gain,
                    q: if b.q > 0.0 { b.q } else { 1.41 },
                    kind: match b.filter_type.as_str() {
                        "LSC" => FilterType::Lsc,
                        "HSC" => FilterType::Hsc,
                        "LPQ" => FilterType::Lpq,
                        "HPQ" => FilterType::Hpq,
                        _ => FilterType::Pk,
                    },
                    enabled: b.enabled,
                })
                .collect(),
        }
    }

    /// The 10 gains when this is the graphic equaliser's profile (GraphicEq.gainsOf).
    pub fn graphic_gains(&self) -> Option<[f32; 10]> {
        if self.id != GRAPHIC_ID || self.bands.len() != 10 {
            return None;
        }
        let mut g = [0.0; 10];
        for (i, b) in self.bands.iter().enumerate() {
            g[i] = b.gain as f32;
        }
        Some(g)
    }
}

/// GraphicEq.profile: peaking filters, no preamp (the engine's soft limiter keeps boosts from clipping).
pub fn graphic_profile(gains: &[f32; 10], name: &str) -> SavedProfile {
    SavedProfile {
        id: GRAPHIC_ID.into(),
        name: name.into(),
        device_model: String::new(),
        bands: FREQUENCIES
            .iter()
            .zip(gains)
            .map(|(f, g)| SavedBand {
                frequency: *f,
                gain: f64::from(*g),
                q: 1.41,
                filter_type: "PK".into(),
                enabled: true,
            })
            .collect(),
        preamp: 0.0,
        is_custom: false,
        is_active: false,
        added_timestamp: crate::songs::now_ms(),
    }
}

/// The name a set of gains shows: the preset it equals, else «Своя».
pub fn preset_name(gains: &[f32; 10]) -> String {
    PRESETS
        .iter()
        .find(|(_, g)| g.iter().zip(gains).all(|(a, b)| (a - b).abs() < 0.05))
        .map_or_else(|| "Своя".to_string(), |(n, _)| n.to_string())
}

/// What the sound section shows.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundSettings {
    pub normalize: bool,
    pub crossfade: bool,
    pub crossfade_s: f32,
    pub crossfade_album: bool,
    /// −1..1.
    pub balance: f32,
    /// The equaliser is on (some profile is active).
    pub eq_on: bool,
    /// The graphic equaliser's gains (the saved ones even while off; flat if never set).
    pub gains: [f32; 10],
    /// An imported (AutoEq) profile is the active one: its name; the bands then belong to it.
    pub other_profile: Option<String>,
}

impl App {
    fn eq_profiles(&self) -> (Vec<SavedProfile>, Option<String>) {
        let sp = self.shared(EQ_FILE);
        let list = sp
            .get_str(EQ_PROFILES)
            .and_then(|j| serde_json::from_str::<Vec<SavedProfile>>(j).ok())
            .unwrap_or_default();
        (list, sp.get_str(EQ_ACTIVE).map(str::to_string))
    }

    fn write_eq(&self, list: &[SavedProfile], active: Option<&str>) {
        let json = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".into());
        self.edit_shared_prefs(EQ_FILE, |p| {
            p.values.insert(EQ_PROFILES.into(), SpValue::String(json));
            match active {
                Some(a) => {
                    p.values.insert(EQ_ACTIVE.into(), SpValue::String(a.into()));
                }
                None => {
                    p.values.remove(EQ_ACTIVE);
                }
            }
        });
    }

    pub fn sound_settings(&self) -> SoundSettings {
        let (list, active) = self.eq_profiles();
        let active_p = active.as_deref().and_then(|a| list.iter().find(|p| p.id == a));
        let gains = list.iter().find(|p| p.id == GRAPHIC_ID).and_then(SavedProfile::graphic_gains).unwrap_or([0.0; 10]);
        SoundSettings {
            normalize: self.pref_bool(NORMALIZE_KEY).unwrap_or(true),
            crossfade: self.pref_bool(CROSSFADE_KEY).unwrap_or(false),
            crossfade_s: self.pref_f32(CROSSFADE_LEN_KEY).unwrap_or(5.0).clamp(1.0, 15.0),
            crossfade_album: self.pref_bool(CROSSFADE_ALBUM_KEY).unwrap_or(true),
            balance: self.pref_f32(BALANCE_KEY).unwrap_or(0.0).clamp(-1.0, 1.0),
            eq_on: active_p.is_some(),
            gains,
            other_profile: active_p.filter(|p| p.id != GRAPHIC_ID).map(|p| p.name.clone()),
        }
    }

    /// Hands every sound setting to the player.
    pub fn apply_sound(&self) {
        let s = self.sound_settings();
        let pb = &self.inner.playback;
        pb.normalize.store(s.normalize, std::sync::atomic::Ordering::Relaxed);
        pb.set_transitions(Transitions {
            crossfade_s: if s.crossfade { s.crossfade_s } else { 0.0 },
            album_gapless: s.crossfade_album,
        });
        let (list, active) = self.eq_profiles();
        let eq = active.and_then(|a| list.into_iter().find(|p| p.id == a)).map(|p| p.to_engine());
        pb.set_effects(eq, s.balance);
    }

    pub fn set_sound_bool(&self, key: &str, on: bool) {
        let _ = self.set_pref(key, PrefValue::Bool(on));
        self.apply_sound();
    }

    pub fn set_sound_f32(&self, key: &str, v: f32) {
        let _ = self.set_pref(key, PrefValue::Float(v));
        self.apply_sound();
    }

    /// The graphic equaliser with these gains, switched on (EQViewModel: save the profile and make it active).
    pub fn set_eq_gains(&self, gains: [f32; 10]) {
        let gains = gains.map(|g| (g * 2.0).round() / 2.0).map(|g| g.clamp(-MAX_DB, MAX_DB));
        let (mut list, _) = self.eq_profiles();
        let p = graphic_profile(&gains, &preset_name(&gains));
        match list.iter_mut().find(|x| x.id == GRAPHIC_ID) {
            Some(x) => *x = p,
            None => list.push(p),
        }
        self.write_eq(&list, Some(GRAPHIC_ID));
        self.apply_sound();
    }

    /// Equaliser on (the graphic one, with its saved gains) or off (no active profile).
    pub fn set_eq_on(&self, on: bool) {
        if on {
            let gains = self.sound_settings().gains;
            self.set_eq_gains(gains);
        } else {
            let (list, _) = self.eq_profiles();
            self.write_eq(&list, None);
            self.apply_sound();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_profile_json_reads() {
        let j = r#"[{"id":"meld_graphic","name":"Бас","deviceModel":"","bands":[{"frequency":32.0,"gain":7.0,"q":1.41,"filterType":"PK","enabled":true}],"preamp":0.0,"isCustom":false,"isActive":false,"addedTimestamp":1}]"#;
        let v: Vec<SavedProfile> = serde_json::from_str(j).unwrap();
        assert_eq!(v[0].bands[0].gain, 7.0);
        let e = v[0].to_engine();
        assert_eq!(e.bands[0].kind, FilterType::Pk);
        let back = serde_json::to_string(&v).unwrap();
        assert!(back.contains("\"filterType\":\"PK\"") && back.contains("\"addedTimestamp\":1"));
    }

    #[test]
    fn presets_are_named() {
        assert_eq!(preset_name(&PRESETS[1].1), "Бас");
        assert_eq!(preset_name(&[1.0; 10]), "Своя");
    }
}
