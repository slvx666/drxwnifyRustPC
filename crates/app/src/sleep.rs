//! The sleep timer (Android playback/SleepTimer.kt): pause after N minutes or at the end of the track; optionally
//! wait for the track to end once the time is up, and fade the volume out over the last minute.

use crate::App;
use std::time::Duration;
use tokio::sync::watch;

/// The volume fades out over this last stretch (Android FADE_OUT_WINDOW_MS).
const FADE_WINDOW_MS: i64 = 60_000;

/// What the timer is set to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SleepState {
    /// When the time is up (local ms); None for «at the end of the track».
    pub until: Option<i64>,
    /// Pause when the playing track ends (set directly, or when the time ran out with `after_song`).
    pub end_of_song: bool,
    /// When the time is up, wait for the track to end.
    pub after_song: bool,
    pub fade: bool,
}

impl SleepState {
    pub fn active(&self) -> bool {
        self.until.is_some() || self.end_of_song
    }
}

/// Android computeSleepTimerVolumeMultiplier: full volume until the last minute, then down to zero.
pub fn fade_multiplier(remaining_ms: i64) -> f32 {
    if remaining_ms >= FADE_WINDOW_MS {
        1.0
    } else {
        (remaining_ms as f32 / FADE_WINDOW_MS as f32).clamp(0.0, 1.0)
    }
}

impl App {
    pub fn sleep_timer(&self) -> watch::Receiver<SleepState> {
        self.inner.sleep.subscribe()
    }

    /// Starts the timer: `minutes` None = at the end of this track.
    pub fn sleep_start(&self, minutes: Option<u32>, after_song: bool, fade: bool) {
        let state = SleepState {
            until: minutes.map(|m| crate::songs::now_ms() + i64::from(m) * 60_000),
            end_of_song: minutes.is_none(),
            after_song: minutes.is_some() && after_song,
            fade,
        };
        self.inner.playback.set_fade(1.0);
        self.inner.sleep.send_replace(state);
        let mut task = self.inner.sleep_task.lock().expect("sleep task");
        if let Some(t) = task.take() {
            t.abort();
        }
        *task = Some(self.runtime.spawn(run(self.clone())));
    }

    pub fn sleep_clear(&self) {
        if let Some(t) = self.inner.sleep_task.lock().expect("sleep task").take() {
            t.abort();
        }
        self.inner.playback.set_fade(1.0);
        self.inner
            .playback
            .hold_next
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.inner.sleep.send_replace(SleepState::default());
    }

    fn sleep_fire(&self) {
        if self.now_playing().borrow().playing {
            self.toggle_play();
        }
        self.inner
            .playback
            .hold_next
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.inner.playback.set_fade(1.0);
        self.inner.sleep.send_replace(SleepState::default());
    }
}

async fn run(app: App) {
    let mut song: Option<String> = app
        .now_playing()
        .borrow()
        .track
        .as_ref()
        .map(|t| t.id.clone());
    loop {
        let s = app.inner.sleep.borrow().clone();
        if !s.active() {
            return;
        }
        // Waiting for the track's end: no gapless start or crossfade into the next one.
        app.inner
            .playback
            .hold_next
            .store(s.end_of_song, std::sync::atomic::Ordering::Relaxed);
        let now = crate::songs::now_ms();
        let current = app
            .now_playing()
            .borrow()
            .track
            .as_ref()
            .map(|t| t.id.clone());
        if let Some(until) = s.until {
            let remaining = until - now;
            if remaining <= 0 {
                if s.after_song {
                    app.inner.sleep.send_modify(|x| {
                        x.until = None;
                        x.end_of_song = true;
                        x.after_song = false;
                    });
                    song = current;
                    continue;
                }
                app.sleep_fire();
                return;
            }
            if s.fade && !s.after_song {
                app.inner.playback.set_fade(fade_multiplier(remaining));
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
            continue;
        }
        // At the end of the track: pause just before it ends (the next one must not start), or right away if a
        // new track started meanwhile.
        if current != song && song.is_some() {
            app.sleep_fire();
            return;
        }
        song = current;
        let c = app.clock();
        let remaining = c.duration_s.map(|d| ((d - c.position_s) * 1000.0) as i64);
        if let Some(r) = remaining {
            if s.fade {
                app.inner.playback.set_fade(fade_multiplier(r));
            }
            if c.playing && r <= 350 {
                app.sleep_fire();
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_over_the_last_minute() {
        assert_eq!(fade_multiplier(120_000), 1.0);
        assert_eq!(fade_multiplier(60_000), 1.0);
        assert!((fade_multiplier(30_000) - 0.5).abs() < 1e-6);
        assert_eq!(fade_multiplier(0), 0.0);
        assert_eq!(fade_multiplier(-5), 0.0);
    }
}
