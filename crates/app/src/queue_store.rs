//! The queue between runs (Android PersistentQueue): the tracks, the playing index and the position go to
//! `pc_queue.json` in the profile (a PC-only file: backups take known files only); the next launch shows them paused.

use crate::App;
use crate::playback::Playable;
use drx_catalog::model::Track;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const FILE: &str = "pc_queue.json";
/// The queue around the playing track kept at most (a liked list can be thousands long).
const MAX_KEPT: usize = 2_000;

#[derive(Serialize, Deserialize)]
struct Saved {
    index: usize,
    position_s: f64,
    tracks: Vec<Track>,
}

impl App {
    fn queue_file(&self) -> std::path::PathBuf {
        self.inner.profile.root().join(FILE)
    }

    /// Shows the last run's queue (paused, at its position). False when there was none.
    pub fn restore_queue(&self) -> bool {
        if !self.pref_bool("persistentQueue").unwrap_or(true) {
            return false;
        }
        let Some(saved) = std::fs::read(self.queue_file())
            .ok()
            .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
        else {
            return false;
        };
        if saved.tracks.is_empty() {
            return false;
        }
        self.remember(saved.tracks.iter());
        let items = saved
            .tracks
            .into_iter()
            .map(|track| Playable { track, url: None })
            .collect();
        self.inner
            .playback
            .restore(items, saved.index, saved.position_s);
        true
    }

    /// Writes the queue down now.
    pub fn save_queue(&self) {
        if !self.pref_bool("persistentQueue").unwrap_or(true) {
            let _ = std::fs::remove_file(self.queue_file());
            return;
        }
        let (tracks, index) = self.queue();
        if tracks.is_empty() {
            return;
        }
        // Keep a window around the playing track when the queue is huge.
        let start = index
            .saturating_sub(MAX_KEPT / 4)
            .min(tracks.len().saturating_sub(MAX_KEPT));
        let kept: Vec<Track> = tracks.into_iter().skip(start).take(MAX_KEPT).collect();
        let saved = Saved {
            index: index - start,
            position_s: self.clock().position_s,
            tracks: kept,
        };
        if let Ok(bytes) = serde_json::to_vec(&saved) {
            let _ = drx_core::paths::write_atomic(&self.queue_file(), &bytes);
        }
    }

    /// Saves on every queue change, play / pause, and every 20 s while playing (the position).
    pub fn keep_queue_saved(&self) {
        let app = self.clone();
        self.runtime.spawn(async move {
            let mut rx = app.now_playing();
            let mut last = None;
            loop {
                let key = {
                    let n = rx.borrow_and_update();
                    (n.queue_rev, n.index, n.playing)
                };
                let playing = key.2;
                if last != Some(key) {
                    last = Some(key);
                    // A burst of changes (a new list) settles first.
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    app.save_queue();
                }
                let tick = tokio::time::sleep(Duration::from_secs(20));
                tokio::select! {
                    r = rx.changed() => if r.is_err() { return },
                    _ = tick => if playing { app.save_queue() },
                }
            }
        });
    }
}
