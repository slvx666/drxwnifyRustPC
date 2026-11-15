//! Listening along with a friend (Android `friends/ListenAlong.kt`): their «now playing» carries the track, the
//! position and when it was taken, so this app plays the same track at the same moment and keeps up with
//! play/pause, seeks and track changes as their updates arrive. No connection between the devices is needed.

use crate::App;
use crate::social::track_of;
use drx_social::friends::NowPlaying;
use std::time::Duration;

/// Further apart than this, the position jumps to the friend's.
const MAX_DRIFT_MS: i64 = 3_000;
/// How long a new track may take to start before the position is set anyway.
const LOAD_WAIT: Duration = Duration::from_secs(20);

fn key_of(n: &NowPlaying) -> String {
    let t = &n.track;
    t.catalog_id.clone().or_else(|| t.media_id.clone()).unwrap_or_else(|| format!("{}|{}", t.artist, t.title))
}

impl App {
    /// Whose playback this app follows now.
    pub fn listening_along(&self) -> Option<String> {
        self.inner.listen_along.lock().expect("listen along").as_ref().map(|(p, _)| p.clone())
    }

    /// Starts following `pubkey`. Err when they play nothing or keep listening along closed.
    pub fn start_listen_along(&self, pubkey: &str) -> crate::AppResult<()> {
        let mut rx = self.friends_state().ok_or_else(|| crate::AppError::Other("нет профиля друзей".into()))?;
        let first = rx.borrow().data.get(pubkey).and_then(|d| d.now.clone());
        let first = first.ok_or_else(|| crate::AppError::Other("друг сейчас ничего не слушает".into()))?;
        if !first.listen_along {
            return Err(crate::AppError::Other("друг закрыл «слушать вместе»".into()));
        }
        self.stop_listen_along();
        let (app, peer) = (self.clone(), pubkey.to_string());
        let task = self.runtime.spawn(async move {
            let mut current: Option<String> = None;
            follow(&app, &first, true, &mut current).await;
            let mut last = first;
            while rx.changed().await.is_ok() {
                let now = rx.borrow().data.get(&peer).and_then(|d| d.now.clone());
                let Some(now) = now else { continue };
                if now == last {
                    continue;
                }
                follow(&app, &now, false, &mut current).await;
                last = now;
            }
        });
        *self.inner.listen_along.lock().expect("listen along") = Some((pubkey.to_string(), task));
        self.listen_along_changed();
        Ok(())
    }

    pub fn stop_listen_along(&self) {
        let old = self.inner.listen_along.lock().expect("listen along").take();
        if let Some((_, task)) = old {
            task.abort();
            self.listen_along_changed();
        }
    }

    fn listen_along_changed(&self) {
        self.inner.listen_along_version.send_modify(|v| *v += 1);
    }

    /// Changes of who is followed (a counter): the UI redraws its «слушаю вместе» marks when it moves.
    pub fn listen_along_version(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.listen_along_version.subscribe()
    }
}

async fn follow(app: &App, now: &NowPlaying, force: bool, current: &mut Option<String>) {
    if !now.listen_along {
        return;
    }
    let key = key_of(now);
    let playing = || app.now_playing().borrow().playing;
    if force || current.as_deref() != Some(key.as_str()) {
        *current = Some(key);
        let track = track_of(&now.track);
        let id = track.id.clone();
        app.play_tracks(vec![track], 0);
        // Wait for the track to start, then jump to where the friend is now.
        let started = tokio::time::timeout(LOAD_WAIT, async {
            loop {
                let same = app.now_playing().borrow().track.as_ref().is_some_and(|t| t.id == id);
                let c = app.clock();
                if same && c.track.is_some() && c.playing {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .is_ok();
        if started {
            app.seek(now.position_now(crate::songs::now_ms()) as f64 / 1000.0);
        }
        if now.playing != playing() {
            app.toggle_play();
        }
        return;
    }
    let mine = (app.clock().position_s * 1000.0) as i64;
    let theirs = now.position_now(crate::songs::now_ms());
    if (mine - theirs).abs() > MAX_DRIFT_MS {
        app.seek(theirs as f64 / 1000.0);
    }
    if now.playing != playing() {
        app.toggle_play();
    }
}
