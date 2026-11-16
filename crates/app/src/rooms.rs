//! «Слушать вместе» rooms (android ListenTogetherManager) on top of `drx-together`: the host's playback goes to
//! the room (track changes, play / pause, seeks), a guest's playback follows what the room says. Guests keep their
//! own buttons; the next word from the host puts them back in step (and «Синхронизировать» asks for it at once).

use crate::App;
use crate::social::{ref_of, track_of};
use drx_catalog::model::Track;
use drx_social::friends::TrackRef;
use drx_together::{Client, Event, PlaybackAction, Role, Room, TrackInfo};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Further apart than this while playing, a guest jumps to the room's position (Android 3 s).
const PLAYING_TOLERANCE_MS: i64 = 3_000;
/// Paused, the position is set more eagerly (Android 2 s).
const PAUSED_TOLERANCE_MS: i64 = 2_000;
/// How long a new track may take to start before the room's state is applied anyway.
const LOAD_WAIT: Duration = Duration::from_secs(20);
/// How often the host looks at its own playback.
const HOST_TICK: Duration = Duration::from_millis(400);

/// A running room: the connection and its tasks.
pub struct RoomSession {
    client: Client,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

fn now_ms() -> i64 {
    crate::songs::now_ms()
}

/// The room's form of a track: the phone's song id, title, artists, album, ms, cover.
pub fn track_info(t: &Track) -> TrackInfo {
    let r = ref_of(t);
    TrackInfo {
        id: r.media_id.clone().or(r.catalog_id.clone()).unwrap_or_else(|| t.id.clone()),
        title: t.title.clone(),
        artist: t.artist_names(),
        album: t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
        duration: if t.duration_ms > 0 { t.duration_ms as i64 } else { 180_000 },
        thumbnail: r.cover.unwrap_or_default(),
        suggested_by: String::new(),
    }
}

/// A room's track as one this app plays.
pub fn track_of_info(t: &TrackInfo) -> Track {
    let mut track = track_of(&TrackRef {
        title: t.title.clone(),
        artist: t.artist.clone(),
        catalog_id: None,
        media_id: Some(t.id.clone()).filter(|i| !i.is_empty()),
        cover: Some(t.thumbnail.clone()).filter(|c| c.starts_with("http")),
        duration_ms: t.duration,
        at: 0,
    });
    if let Some(a) = track.album.as_mut() {
        a.name = t.album.clone();
    }
    track
}

/// What a guest should be playing: the track, playing or not, the position at `at` (local ms).
#[derive(Debug, Clone, PartialEq, Default)]
struct Desired {
    track: Option<TrackInfo>,
    playing: bool,
    position: i64,
    at: i64,
}

impl Desired {
    fn position_now(&self) -> i64 {
        if self.playing { self.position + (now_ms() - self.at).max(0) } else { self.position }
    }
}

impl App {
    /// The room, while in one (or joining).
    pub fn room(&self) -> Option<watch::Receiver<Room>> {
        self.inner.room.lock().expect("room").as_ref().map(|s| s.client.room())
    }

    /// Changes of being in a room or not (a counter): the UI starts or stops watching the room on it.
    pub fn room_version(&self) -> watch::Receiver<u64> {
        self.inner.room_version.subscribe()
    }

    fn room_client(&self) -> Option<Client> {
        self.inner.room.lock().expect("room").as_ref().map(|s| s.client.clone())
    }

    /// Creates a room (`code` None) or asks to join one, as `username`.
    pub fn room_start(&self, username: &str, code: Option<&str>) {
        self.room_leave();
        self.stop_listen_along();
        let _guard = self.runtime.enter();
        let (tx, rx) = mpsc::unbounded_channel();
        // The profile's name (the phone's), when the screen did not have it yet.
        let own = || {
            let st = self.friends_state()?;
            let st = st.borrow();
            let me = st.me.clone()?;
            st.data.get(&me).and_then(|d| d.name.clone())
        };
        let name = match username.trim() {
            "" => own().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Drxwnify PC".into()),
            n => n.to_string(),
        };
        let name = name.as_str();
        let client = Client::start(drx_together::DEFAULT_SERVER, name, code, tx);
        let desired = watch::channel(Desired::default()).0;
        let tasks = vec![
            self.runtime.spawn(events_task(self.clone(), client.clone(), rx, desired.clone())),
            self.runtime.spawn(follow_task(self.clone(), client.clone(), desired.subscribe())),
            self.runtime.spawn(host_task(self.clone(), client.clone())),
        ];
        *self.inner.room.lock().expect("room") = Some(RoomSession { client, tasks });
        self.inner.room_version.send_modify(|v| *v += 1);
    }

    pub fn room_leave(&self) {
        let old = self.inner.room.lock().expect("room").take();
        if let Some(s) = old {
            s.client.leave();
            // The connection task ends itself after saying goodbye; the followers stop now.
            for t in s.tasks {
                t.abort();
            }
            self.inner.room_version.send_modify(|v| *v += 1);
        }
    }

    pub fn room_approve(&self, user_id: &str, yes: bool) {
        if let Some(c) = self.room_client() {
            if yes { c.approve(user_id) } else { c.reject(user_id) }
        }
    }
    pub fn room_kick(&self, user_id: &str) {
        if let Some(c) = self.room_client() {
            c.kick(user_id);
        }
    }
    pub fn room_make_host(&self, user_id: &str) {
        if let Some(c) = self.room_client() {
            c.transfer_host(user_id);
        }
    }
    /// «Синхронизировать» (a guest): the server sends the room's state again.
    pub fn room_resync(&self) {
        if let Some(c) = self.room_client() {
            c.request_sync();
        }
    }
    /// A guest suggests a track; the host approves or not.
    pub fn room_suggest(&self, track: &Track) {
        if let Some(c) = self.room_client() {
            c.suggest(&track_info(track));
        }
    }
    pub fn room_answer_suggestion(&self, id: &str, yes: bool) {
        if let Some(c) = self.room_client() {
            if yes { c.approve_suggestion(id) } else { c.reject_suggestion(id) }
        }
    }

    fn room_ended(&self) {
        let old = self.inner.room.lock().expect("room").take();
        if let Some(s) = old {
            for t in s.tasks {
                t.abort();
            }
        }
        self.inner.room_version.send_modify(|v| *v += 1);
    }
}

/// The server's words: a guest's wanted state, the host's approved suggestions, the end of the room.
async fn events_task(app: App, client: Client, mut rx: mpsc::UnboundedReceiver<Event>, desired: watch::Sender<Desired>) {
    let room = client.room();
    while let Some(e) = rx.recv().await {
        let guest = room.borrow().role == Role::Guest;
        match e {
            Event::Joined(state) => {
                let elapsed = if state.is_playing && state.last_update > 0 { (now_ms() - state.last_update).max(0) } else { 0 };
                desired.send_replace(Desired {
                    track: state.current_track,
                    playing: state.is_playing,
                    position: state.position + elapsed,
                    at: now_ms(),
                });
            }
            Event::SyncState(s) if guest => {
                let elapsed = if s.is_playing && s.last_update > 0 { (now_ms() - s.last_update).max(0) } else { 0 };
                desired.send_replace(Desired {
                    track: s.current_track.or_else(|| desired.borrow().track.clone()),
                    playing: s.is_playing,
                    position: s.position + elapsed,
                    at: now_ms(),
                });
            }
            Event::Sync(a) if guest => {
                let now = now_ms();
                let lag = if a.server_time > 0 { (now - a.server_time).clamp(0, 5_000) } else { 0 };
                desired.send_modify(|d| match a.action.as_str() {
                    "change_track" => {
                        d.track = a.track_info.clone();
                        d.playing = false;
                        d.position = 0;
                        d.at = now;
                    }
                    "play" => {
                        d.playing = true;
                        d.position = a.position + lag;
                        d.at = now;
                    }
                    "pause" => {
                        d.playing = false;
                        d.position = a.position;
                        d.at = now;
                    }
                    "seek" => {
                        d.position = a.position + if d.playing { lag } else { 0 };
                        d.at = now;
                    }
                    _ => {}
                });
            }
            Event::SuggestionApproved(t) if room.borrow().role == Role::Host => {
                let track = track_of_info(&t);
                app.remember_tracks(std::slice::from_ref(&track));
                app.enqueue_track(track, true);
            }
            Event::Ended(_) => {
                app.room_ended();
                return;
            }
            _ => {}
        }
    }
}

/// A guest's playback follows the wanted state: a new track is loaded (and the room told when it is ready), then
/// the position and play / pause are set when they are off.
async fn follow_task(app: App, client: Client, mut desired: watch::Receiver<Desired>) {
    let mut loaded: Option<String> = None;
    while desired.changed().await.is_ok() {
        let d = desired.borrow_and_update().clone();
        if client.room().borrow().role != Role::Guest {
            continue;
        }
        let Some(info) = d.track.clone() else { continue };
        let playing = || app.now_playing().borrow().playing;
        if loaded.as_deref() != Some(info.id.as_str()) {
            loaded = Some(info.id.clone());
            let track = track_of_info(&info);
            let id = track.id.clone();
            app.remember_tracks(std::slice::from_ref(&track));
            app.play_tracks(vec![track], 0);
            let started = tokio::time::timeout(LOAD_WAIT, async {
                loop {
                    let same = app.now_playing().borrow().track.as_ref().is_some_and(|t| t.id == id);
                    let c = app.clock();
                    if same && c.track.is_some() && c.playing {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            })
            .await
            .is_ok();
            client.buffer_ready(&info.id);
            // The state may have moved on while loading (the host's «play» comes right after the change).
            let d = desired.borrow().clone();
            if started && d.track.as_ref().is_some_and(|t| t.id == info.id) {
                let pos = d.position_now();
                if pos > 1_000 {
                    app.seek(pos as f64 / 1000.0);
                }
            }
            if d.playing != playing() {
                app.toggle_play();
            }
            continue;
        }
        let mine = (app.clock().position_s * 1000.0) as i64;
        let theirs = d.position_now();
        let tolerance = if d.playing && playing() { PLAYING_TOLERANCE_MS } else { PAUSED_TOLERANCE_MS };
        if (mine - theirs).abs() > tolerance {
            app.seek(theirs as f64 / 1000.0);
        }
        if d.playing != playing() {
            app.toggle_play();
        }
    }
}

/// The host's playback goes to the room: a new track (then «play» if it plays), play / pause, and seeks — a jump
/// of the position the clock cannot explain.
async fn host_task(app: App, client: Client) {
    let room = client.room();
    let mut last_track: Option<String> = None;
    let mut last_playing = false;
    let mut last_pos: Option<(i64, i64)> = None; // (position ms, at)
    loop {
        tokio::time::sleep(HOST_TICK).await;
        let (role, connected) = {
            let r = room.borrow();
            (r.role, r.connected)
        };
        if role != Role::Host || !connected {
            last_track = None;
            last_pos = None;
            continue;
        }
        let (track, playing) = {
            let n = app.now_playing();
            let n = n.borrow();
            (n.track.clone(), n.playing)
        };
        let Some(track) = track else { continue };
        let pos = (app.clock().position_s * 1000.0) as i64;
        let now = now_ms();
        let info = track_info(&track);
        if last_track.as_deref() != Some(info.id.as_str()) {
            last_track = Some(info.id.clone());
            client.playback(&PlaybackAction {
                action: "change_track".into(),
                track_id: info.id.clone(),
                track_info: Some(info.clone()),
                ..Default::default()
            });
            // The server stops the room on a track change: «play» again once it has taken the change.
            tokio::time::sleep(Duration::from_millis(150)).await;
            if playing {
                let pos = (app.clock().position_s * 1000.0) as i64;
                client.playback(&PlaybackAction { action: "play".into(), track_id: info.id.clone(), position: pos, ..Default::default() });
            }
            last_playing = playing;
            last_pos = Some((pos, now));
            continue;
        }
        if playing != last_playing {
            last_playing = playing;
            let action = if playing { "play" } else { "pause" };
            client.playback(&PlaybackAction { action: action.into(), track_id: info.id.clone(), position: pos, ..Default::default() });
            last_pos = Some((pos, now));
            continue;
        }
        if let Some((p0, t0)) = last_pos {
            let expected = if playing { p0 + (now - t0) } else { p0 };
            if (pos - expected).abs() > 2_000 {
                client.playback(&PlaybackAction { action: "seek".into(), track_id: info.id.clone(), position: pos, ..Default::default() });
            }
        }
        last_pos = Some((pos, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_round_trip_keeps_the_song_id() {
        let info = TrackInfo {
            id: "dQw4w9WgXcQ".into(),
            title: "Never".into(),
            artist: "Rick Astley".into(),
            album: "Whenever".into(),
            duration: 213_000,
            thumbnail: "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg".into(),
            suggested_by: String::new(),
        };
        let t = track_of_info(&info);
        assert_eq!(t.id, "dQw4w9WgXcQ");
        assert_eq!(t.artist_names(), "Rick Astley");
        let back = track_info(&t);
        assert_eq!((back.id.as_str(), back.duration), ("dQw4w9WgXcQ", 213_000));
    }
}
