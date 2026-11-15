//! The listening history, as Android's `MusicService.trackListen`/`saveListen`: a play is saved as soon as enough
//! of it was heard (`Listening.neededMs`: half the track, at least the "add to history after" setting, 30 s, at most
//! four minutes; a short track nearly whole; one of unknown length after a minute), and when the track is left its
//! row gets the whole play time, which is also added to the song's `totalPlayTime`. "Pause history" stops it.

use crate::App;
use crate::songs::{ensure_song, now_ms};
use std::time::{Duration, Instant};

/// Settings (PreferenceKeys.kt).
pub const HISTORY_DURATION_KEY: &str = "historyDuration";
pub const PAUSE_HISTORY_KEY: &str = "pauseListenHistory";

const DEFAULT_FLOOR_MS: i64 = 30_000;
const CEILING_MS: i64 = 4 * 60_000;
const UNKNOWN_LENGTH_MS: i64 = 60_000;

/// How much of a track must be heard for it to count (`Listening.neededMs`).
pub fn needed_ms(duration_ms: i64, floor_ms: i64) -> i64 {
    if duration_ms <= 0 {
        UNKNOWN_LENGTH_MS.max(floor_ms)
    } else if duration_ms < floor_ms {
        duration_ms * 9 / 10
    } else {
        (duration_ms / 2).clamp(floor_ms, CEILING_MS.max(floor_ms))
    }
}

/// One play of a track as the history counts it.
struct Listen {
    /// The player's id for this play (a replay is a new play).
    play: drx_audio::player::TrackId,
    track: drx_catalog::model::Track,
    played_ms: i64,
    needed_ms: i64,
    /// The history row once saved, with its song id.
    saved: Option<(i64, String)>,
}

impl App {
    fn history_floor_ms(&self) -> (i64, bool) {
        let p = self.prefs_now();
        let floor = match p.get(HISTORY_DURATION_KEY) {
            Some(drx_core::prefs::PrefValue::Float(s)) => (*s * 1000.0) as i64,
            _ => DEFAULT_FLOOR_MS,
        };
        (floor, p.get_bool(PAUSE_HISTORY_KEY).unwrap_or(false))
    }

    /// Samples the player twice a second for the whole run of the app.
    pub(crate) fn start_history(&self) {
        let weak = std::sync::Arc::downgrade(&self.inner);
        let runtime = self.runtime.clone();
        let _ = std::thread::Builder::new()
            .name("history".into())
            .spawn(move || {
                let mut cur: Option<Listen> = None;
                let mut sampled = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let Some(inner) = weak.upgrade() else { return };
                    let app = App {
                        inner,
                        runtime: runtime.clone(),
                    };
                    let elapsed = (sampled.elapsed().as_millis() as i64).clamp(0, 1_000);
                    sampled = Instant::now();
                    let clock = app.inner.playback.clock();
                    let track = app.inner.playback.now.borrow().track.clone();
                    let play = clock.track;
                    let same = matches!((&cur, play), (Some(l), Some(p)) if l.play == p);
                    if !same {
                        if let Some(done) = cur.take() {
                            app.finish_listen(done);
                        }
                        if let (Some(p), Some(t)) = (play, track) {
                            let (floor, _) = app.history_floor_ms();
                            cur = Some(Listen {
                                play: p,
                                needed_ms: needed_ms(t.duration_ms as i64, floor),
                                track: t,
                                played_ms: 0,
                                saved: None,
                            });
                        }
                        continue;
                    }
                    let Some(l) = cur.as_mut() else { continue };
                    if !clock.playing {
                        continue;
                    }
                    l.played_ms += elapsed;
                    if l.saved.is_none() && l.played_ms >= l.needed_ms {
                        let (_, paused) = app.history_floor_ms();
                        if paused {
                            // Not saved, and not asked again for this play.
                            l.needed_ms = i64::MAX;
                            continue;
                        }
                        l.saved = app.save_listen(&l.track, l.played_ms);
                    }
                }
            });
    }

    /// Writes the listen (and the song row when the track has none yet). None when it could not be written.
    fn save_listen(&self, t: &drx_catalog::model::Track, played_ms: i64) -> Option<(i64, String)> {
        let t = t.clone();
        let res = self.store().call_blocking(move |c| {
            let song = ensure_song(c, &t)?;
            c.execute(
                "INSERT INTO event (songId, timestamp, playTime) VALUES (?1, ?2, ?3)",
                rusqlite::params![song, now_ms(), played_ms],
            )?;
            Ok((c.last_insert_rowid(), song))
        });
        match res {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("listen not saved: {e}");
                None
            }
        }
    }

    /// The track was left: its row gets the whole play time, and the song's total grows by it.
    fn finish_listen(&self, l: Listen) {
        let Some((event, song)) = l.saved else { return };
        let played = l.played_ms;
        let _ = self.store().call_blocking(move |c| {
            c.execute(
                "UPDATE event SET playTime = ?1 WHERE id = ?2",
                rusqlite::params![played, event],
            )?;
            c.execute(
                "UPDATE song SET totalPlayTime = totalPlayTime + ?1 WHERE id = ?2",
                rusqlite::params![played, song],
            )?;
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::needed_ms;

    #[test]
    fn thresholds_match_android() {
        // Listening.kt: 2:30 → 75 s, 3:30 → 1:45, 6:00 → 3:00, 12:00 → 4:00, 0:45 → 30 s, 0:20 → 18 s.
        let f = 30_000;
        assert_eq!(needed_ms(150_000, f), 75_000);
        assert_eq!(needed_ms(210_000, f), 105_000);
        assert_eq!(needed_ms(360_000, f), 180_000);
        assert_eq!(needed_ms(720_000, f), 240_000);
        assert_eq!(needed_ms(45_000, f), 30_000);
        assert_eq!(needed_ms(20_000, f), 18_000);
        assert_eq!(needed_ms(0, f), 60_000);
    }
}
