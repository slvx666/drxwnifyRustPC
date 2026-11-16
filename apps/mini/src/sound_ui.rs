//! The settings' sound section in Mini (Android PlayerSettings and the equaliser screen): normalisation, crossfade,
//! balance and the 10-band graphic equaliser with the phone's presets. Every change is saved and heard at once.

use crate::{MainWindow, SoundData, SoundUi};
use drx_app::App;
use drx_app::sound::{LABELS, PRESETS, preset_name};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

pub fn fill(ui: &MainWindow, app: &App) {
    let s = app.sound_settings();
    let g = ui.global::<SoundUi>();
    g.set_data(SoundData {
        normalize: s.normalize,
        crossfade: s.crossfade,
        crossfade_s: s.crossfade_s,
        album: s.crossfade_album,
        balance: s.balance,
        eq_on: s.eq_on,
        gains: ModelRc::new(VecModel::from(s.gains.to_vec())),
        eq_name: preset_name(&s.gains).into(),
        other: s.other_profile.unwrap_or_default().into(),
    });
    let presets: Vec<SharedString> = PRESETS.iter().map(|(n, _)| (*n).into()).collect();
    g.set_presets(ModelRc::new(VecModel::from(presets)));
    let labels: Vec<SharedString> = LABELS.iter().map(|l| (*l).into()).collect();
    g.set_labels(ModelRc::new(VecModel::from(labels)));
}

pub fn wire(ui: &MainWindow, app: &App) {
    let g = ui.global::<SoundUi>();
    {
        let (app, ui_w) = (app.clone(), ui.as_weak());
        g.on_set_bool(move |key, on| {
            if key == "eq" {
                app.set_eq_on(on);
            } else {
                app.set_sound_bool(&key, on);
            }
            if let Some(ui) = ui_w.upgrade() {
                fill(&ui, &app);
            }
        });
    }
    {
        let (app, ui_w) = (app.clone(), ui.as_weak());
        g.on_set_num(move |key, v| {
            app.set_sound_f32(&key, v);
            if let Some(ui) = ui_w.upgrade() {
                fill(&ui, &app);
            }
        });
    }
    {
        let (app, ui_w) = (app.clone(), ui.as_weak());
        g.on_set_gain(move |i, v| {
            let mut gains = app.sound_settings().gains;
            if let Some(x) = gains.get_mut(i as usize) {
                *x = v;
            }
            app.set_eq_gains(gains);
            if let Some(ui) = ui_w.upgrade() {
                fill(&ui, &app);
            }
        });
    }
    let (app, ui_w) = (app.clone(), ui.as_weak());
    g.on_preset(move |i| {
        if let Some((_, gains)) = PRESETS.get(i as usize) {
            app.set_eq_gains(*gains);
        }
        if let Some(ui) = ui_w.upgrade() {
            fill(&ui, &app);
        }
    });
}
