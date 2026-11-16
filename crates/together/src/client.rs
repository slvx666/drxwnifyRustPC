//! The connection to a Listen Together server (android ListenTogetherClient): one WebSocket of protobuf envelopes,
//! a ping every 25 s, reconnection with the session token while in a room. The room as this side knows it is a
//! watch value; what the playback has to follow comes as events.

use crate::proto::{self, PlaybackAction, RoomState, SyncState, TrackInfo, UserInfo};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

/// The server list of ListenTogetherServers (one public server).
pub const DEFAULT_SERVER: &str = "wss://metroserverx.meowery.eu/ws";
/// The room link the phone shares (ListenTogetherScreen).
pub const LINK_PREFIX: &str = "https://metrolist.meowery.eu/listen?code=";

const PING_EVERY: Duration = Duration::from_secs(25);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_RECONNECTS: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    None,
    Host,
    Guest,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub id: String,
    pub from: String,
    pub track: TrackInfo,
}

/// What this side knows of the room.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Room {
    pub connected: bool,
    pub role: Role,
    pub code: String,
    pub my_id: String,
    pub host_id: String,
    pub users: Vec<UserInfo>,
    /// Join requests waiting for the host: (user id, name).
    pub requests: Vec<(String, String)>,
    pub suggestions: Vec<Suggestion>,
    /// Who the room waits for to load the track.
    pub buffering: Vec<String>,
    /// «Ждём, когда хост впустит», an error…: a line for the screen.
    pub status: String,
    pub current_track: Option<TrackInfo>,
}

impl Room {
    pub fn in_room(&self) -> bool {
        self.role != Role::None
    }
}

/// What the playback side reacts to.
#[derive(Debug, Clone)]
pub enum Event {
    /// Joined (or rejoined) as a guest: start from this state.
    Joined(RoomState),
    Sync(PlaybackAction),
    SyncState(SyncState),
    BufferComplete(String),
    /// Out of the room (left, kicked, the room is gone): the reason, if any.
    Ended(String),
    /// A suggestion the host approved: (track) — the host plays it next.
    SuggestionApproved(TrackInfo),
}

enum Out {
    Send(Vec<u8>),
    Close,
}

/// A running connection.
#[derive(Clone)]
pub struct Client {
    out: mpsc::UnboundedSender<Out>,
    room: watch::Receiver<Room>,
}

impl Client {
    /// Connects and creates a room (as `username`) or joins `code`. Events go to `events`.
    pub fn start(url: &str, username: &str, join: Option<&str>, events: mpsc::UnboundedSender<Event>) -> Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let status = if join.is_some() { "Ждём, когда хост впустит…" } else { "Создаю комнату…" };
        let (room_tx, room_rx) = watch::channel(Room { status: status.into(), ..Default::default() });
        let first = match join {
            Some(code) => proto::envelope("join_room", &proto::write_strings(&[&normalize_code(code), username]).0, false),
            None => proto::envelope("create_room", &proto::write_strings(&[username]).0, false),
        };
        tokio::spawn(run(url.to_string(), username.to_string(), first, out_rx, room_tx, events));
        Client { out: out_tx, room: room_rx }
    }

    pub fn room(&self) -> watch::Receiver<Room> {
        self.room.clone()
    }

    fn send(&self, kind: &str, payload: &[u8]) {
        let _ = self.out.send(Out::Send(proto::envelope(kind, payload, true)));
    }

    fn send_strings(&self, kind: &str, values: &[&str]) {
        self.send(kind, &proto::write_strings(values).0);
    }

    pub fn leave(&self) {
        self.send("leave_room", &[]);
        let _ = self.out.send(Out::Close);
    }
    pub fn approve(&self, user_id: &str) {
        self.send_strings("approve_join", &[user_id]);
    }
    pub fn reject(&self, user_id: &str) {
        self.send_strings("reject_join", &[user_id, "Хост отклонил запрос"]);
    }
    pub fn kick(&self, user_id: &str) {
        self.send_strings("kick_user", &[user_id, "Хост удалил из комнаты"]);
    }
    pub fn transfer_host(&self, user_id: &str) {
        self.send_strings("transfer_host", &[user_id]);
    }
    pub fn playback(&self, action: &PlaybackAction) {
        self.send("playback_action", &action.write().0);
    }
    pub fn buffer_ready(&self, track_id: &str) {
        self.send_strings("buffer_ready", &[track_id]);
    }
    pub fn request_sync(&self) {
        self.send("request_sync", &[]);
    }
    pub fn suggest(&self, track: &TrackInfo) {
        let mut w = proto::Writer::default();
        w.message(1, &track.write());
        self.send("suggest_track", &w.0);
    }
    pub fn approve_suggestion(&self, id: &str) {
        self.send_strings("approve_suggestion", &[id]);
    }
    pub fn reject_suggestion(&self, id: &str) {
        self.send_strings("reject_suggestion", &[id, "Хост отклонил"]);
    }
}

/// A code as typed or pasted: the room link's `code=`, upper case, no spaces.
pub fn normalize_code(text: &str) -> String {
    let t = text.trim();
    let code = t
        .split_once("code=")
        .or_else(|| t.split_once("room="))
        .map(|(_, c)| c.split(['&', '#']).next().unwrap_or(""))
        .unwrap_or(t);
    code.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase()
}

async fn run(
    url: String,
    me: String,
    first: Vec<u8>,
    mut out: mpsc::UnboundedReceiver<Out>,
    room: watch::Sender<Room>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut token: Option<String> = None;
    let mut attempts = 0u32;
    let mut opening = Some(first);
    loop {
        let socket = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str())).await {
            Ok(Ok((s, _))) => s,
            other => {
                let why = match other {
                    Ok(Err(e)) => e.to_string(),
                    _ => "сервер не отвечает".into(),
                };
                tracing::warn!(%why, "listen together: no connection");
                attempts += 1;
                if attempts > MAX_RECONNECTS || (token.is_none() && opening.is_some() && attempts > 2) {
                    room.send_modify(|r| *r = Room { status: format!("Нет связи с сервером: {why}"), ..Default::default() });
                    let _ = events.send(Event::Ended("Нет связи с сервером".into()));
                    return;
                }
                room.send_modify(|r| {
                    r.connected = false;
                    r.status = "Переподключаюсь…".into();
                });
                tokio::time::sleep(Duration::from_secs(u64::from(attempts.min(5)) * 2)).await;
                continue;
            }
        };
        let (mut write, mut read) = socket.split();
        room.send_modify(|r| r.connected = true);
        let hello = match (&token, opening.take()) {
            (Some(t), _) => proto::envelope("reconnect", &proto::write_strings(&[t]).0, false),
            (None, Some(f)) => f,
            (None, None) => return,
        };
        if write.send(Message::Binary(hello.into())).await.is_err() {
            continue;
        }
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        let mut closing = false;
        loop {
            tokio::select! {
                msg = out.recv() => match msg {
                    Some(Out::Send(b)) => {
                        if write.send(Message::Binary(b.into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Out::Close) | None => {
                        let _ = write.send(Message::Close(None)).await;
                        closing = true;
                        break;
                    }
                },
                _ = ping.tick() => {
                    if write.send(Message::Binary(proto::envelope("ping", &[], false).into())).await.is_err() {
                        break;
                    }
                }
                msg = read.next() => match msg {
                    Some(Ok(Message::Binary(b))) => {
                        if let Some((kind, payload)) = proto::open(&b) {
                            if handle(&kind, &payload, &me, &room, &events, &mut token) {
                                attempts = 0;
                            }
                            if !room.borrow().in_room() && token.is_none() && matches!(kind.as_str(), "kicked" | "join_rejected") {
                                closing = true;
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = write.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
            }
        }
        if closing || token.is_none() {
            let was_in = room.borrow().in_room();
            room.send_modify(|r| {
                r.connected = false;
                r.role = Role::None;
            });
            if was_in || closing {
                let _ = events.send(Event::Ended(String::new()));
            }
            return;
        }
        attempts += 1;
        if attempts > MAX_RECONNECTS {
            room.send_modify(|r| *r = Room { status: "Связь с комнатой потеряна".into(), ..Default::default() });
            let _ = events.send(Event::Ended("Связь с комнатой потеряна".into()));
            return;
        }
        room.send_modify(|r| {
            r.connected = false;
            r.status = "Переподключаюсь…".into();
        });
        tokio::time::sleep(Duration::from_secs(u64::from(attempts.min(5)))).await;
    }
}

/// Applies one server message. True when it confirms the room (created, joined, reconnected).
fn handle(
    kind: &str,
    p: &[u8],
    me: &str,
    room: &watch::Sender<Room>,
    events: &mpsc::UnboundedSender<Event>,
    token: &mut Option<String>,
) -> bool {
    let s = proto::strings(p);
    let at = |i: usize| s.get(i).cloned().unwrap_or_default();
    match kind {
        "room_created" => {
            *token = Some(at(2));
            room.send_modify(|r| {
                r.role = Role::Host;
                r.code = at(0);
                r.my_id = at(1);
                r.host_id = at(1);
                r.users = vec![UserInfo { user_id: at(1), username: me.to_string(), is_host: true, is_connected: true }];
                r.status = String::new();
            });
            return true;
        }
        "join_approved" | "reconnected" => {
            let mut state = RoomState::default();
            let mut is_host = false;
            for (f, v) in proto::Fields::new(p) {
                match (kind, f) {
                    ("join_approved", 4) | ("reconnected", 3) => state = RoomState::read(v.bytes()),
                    ("reconnected", 4) => is_host = v.bool(),
                    _ => {}
                }
            }
            if kind == "join_approved" {
                *token = Some(at(2));
            }
            room.send_modify(|r| {
                r.role = if is_host { Role::Host } else { Role::Guest };
                r.code = at(0);
                r.my_id = at(1);
                r.host_id = state.host_id.clone();
                r.users = state.users.clone();
                r.current_track = state.current_track.clone();
                r.status = String::new();
            });
            if !is_host {
                let _ = events.send(Event::Joined(state));
            }
            return true;
        }
        "join_rejected" => {
            *token = None;
            room.send_modify(|r| {
                r.role = Role::None;
                r.status = format!("Хост не впустил{}", if at(0).is_empty() { String::new() } else { format!(": {}", at(0)) });
            });
            let _ = events.send(Event::Ended(at(0)));
        }
        "join_request" => room.send_modify(|r| {
            if !r.requests.iter().any(|(id, _)| *id == at(0)) {
                r.requests.push((at(0), at(1)));
            }
        }),
        "user_joined" | "user_reconnected" => room.send_modify(|r| {
            r.requests.retain(|(id, _)| *id != at(0));
            match r.users.iter_mut().find(|u| u.user_id == at(0)) {
                Some(u) => u.is_connected = true,
                None => r.users.push(UserInfo { user_id: at(0), username: at(1), is_host: false, is_connected: true }),
            }
        }),
        "user_left" => room.send_modify(|r| {
            r.users.retain(|u| u.user_id != at(0));
            r.requests.retain(|(id, _)| *id != at(0));
        }),
        "user_disconnected" => room.send_modify(|r| {
            if let Some(u) = r.users.iter_mut().find(|u| u.user_id == at(0)) {
                u.is_connected = false;
            }
        }),
        "host_changed" => room.send_modify(|r| {
            r.host_id = at(0);
            for u in &mut r.users {
                u.is_host = u.user_id == r.host_id;
            }
            r.role = if r.host_id == r.my_id { Role::Host } else { Role::Guest };
        }),
        "kicked" => {
            *token = None;
            room.send_modify(|r| {
                *r = Room { status: format!("Тебя удалили из комнаты{}", if at(0).is_empty() { String::new() } else { format!(": {}", at(0)) }), ..Default::default() };
            });
            let _ = events.send(Event::Ended(at(0)));
        }
        "sync_playback" => {
            let a = PlaybackAction::read(p);
            if a.action == "change_track" {
                room.send_modify(|r| r.current_track = a.track_info.clone());
            }
            let _ = events.send(Event::Sync(a));
        }
        "sync_state" => {
            let _ = events.send(Event::SyncState(SyncState::read(p)));
        }
        "buffer_wait" => {
            let waiting: Vec<String> = proto::Fields::new(p).filter(|(f, _)| *f == 2).map(|(_, v)| v.str()).collect();
            room.send_modify(|r| r.buffering = waiting);
        }
        "buffer_complete" => {
            room.send_modify(|r| r.buffering.clear());
            let _ = events.send(Event::BufferComplete(at(0)));
        }
        "suggestion_received" => {
            let mut track = TrackInfo::default();
            for (f, v) in proto::Fields::new(p) {
                if f == 4 {
                    track = TrackInfo::read(v.bytes());
                }
            }
            room.send_modify(|r| {
                if r.role == Role::Host {
                    r.suggestions.push(Suggestion { id: at(0), from: at(2), track });
                }
            });
        }
        "suggestion_approved" => {
            let mut track = TrackInfo::default();
            for (f, v) in proto::Fields::new(p) {
                if f == 2 {
                    track = TrackInfo::read(v.bytes());
                }
            }
            room.send_modify(|r| r.suggestions.retain(|x| x.id != at(0)));
            let _ = events.send(Event::SuggestionApproved(track));
        }
        "suggestion_rejected" => room.send_modify(|r| r.suggestions.retain(|x| x.id != at(0))),
        "error" => {
            tracing::warn!(code = %at(0), message = %at(1), "listen together: server error");
            if at(0) == "session_not_found" || at(0) == "room_not_found" {
                *token = None;
            }
            room.send_modify(|r| r.status = if at(1).is_empty() { at(0) } else { at(1) });
        }
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_from_links() {
        assert_eq!(normalize_code(" ab12cd34 "), "AB12CD34");
        assert_eq!(normalize_code("https://metrolist.meowery.eu/listen?code=Xy7Kq2Lm&utm=1"), "XY7KQ2LM");
        assert_eq!(normalize_code("drxwnify://listen?room=abcd1234"), "ABCD1234");
    }
}
