//! The sleep timer button in the player bar: a menu of times, «at the end of the track», fade-out and «wait for
//! the track to end» (kept in the settings); the time left next to the button while it runs.

use crate::MainWindow;
use drx_app::App;
use drx_core::prefs::PrefValue;
use slint::ComponentHandle;

const FADE_KEY: &str = "pcSleepFade";
const AFTER_KEY: &str = "pcSleepAfter";

/// «23:41», «1:05:00».
fn left(ms: i64) -> String {
    let s = (ms.max(0) + 999) / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn wire(ui: &MainWindow, app: &App) {
    ui.set_sleep_fade(app.pref_i64(FADE_KEY) == Some(1));
    ui.set_sleep_after(app.pref_i64(AFTER_KEY) == Some(1));
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_sleep_set(move |m| {
            let Some(ui) = ui_w.upgrade() else { return };
            match m {
                0 => {
                    app2.sleep_clear();
                    ui.set_toast("Таймер сна выключен".into());
                }
                -1 => {
                    app2.sleep_start(None, false, ui.get_sleep_fade());
                    ui.set_toast("Пауза в конце этого трека".into());
                }
                m => {
                    app2.sleep_start(Some(m as u32), ui.get_sleep_after(), ui.get_sleep_fade());
                    ui.set_toast(format!("Таймер сна: {} мин", m).into());
                }
            }
            refresh(&ui, &app2);
        });
    }
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        ui.on_sleep_option(move |o| {
            let Some(ui) = ui_w.upgrade() else { return };
            let (key, on) = match o.as_str() {
                "fade" => {
                    let v = !ui.get_sleep_fade();
                    ui.set_sleep_fade(v);
                    (FADE_KEY, v)
                }
                _ => {
                    let v = !ui.get_sleep_after();
                    ui.set_sleep_after(v);
                    (AFTER_KEY, v)
                }
            };
            let _ = app2.set_pref(key, PrefValue::Int(i32::from(on)));
        });
    }
    refresh(ui, app);
}

thread_local! {
    static TIMER: slint::Timer = slint::Timer::default();
}

fn refresh(ui: &MainWindow, app: &App) {
    let s = app.sleep_timer().borrow().clone();
    ui.set_sleep_on(s.active());
    let label = match s.until {
        Some(until) => {
            left(
                until
                    - std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
            ) + if s.after_song { " + трек" } else { "" }
        }
        None if s.end_of_song => "конец трека".into(),
        None => String::new(),
    };
    if ui.get_sleep_label() != label.as_str() {
        ui.set_sleep_label(label.into());
    }
    // The time left, once a second — only while the timer runs (it also notices when it fired).
    TIMER.with(|t| {
        if !s.active() {
            t.stop();
        } else if !t.running() {
            let (app, ui_w) = (app.clone(), ui.as_weak());
            t.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_secs(1),
                move || {
                    if let Some(ui) = ui_w.upgrade() {
                        refresh(&ui, &app);
                    }
                },
            );
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn time_left() {
        assert_eq!(super::left(23 * 60_000 + 41_000), "23:41");
        assert_eq!(super::left(3_900_000), "1:05:00");
        assert_eq!(super::left(400), "0:01");
    }
}
