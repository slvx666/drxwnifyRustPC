//! Chats with friends: kept in the profile's `chat.db` (the phone's own chat history is not in its backup, so
//! the PC builds its own from the relays — both what friends sent and what this identity sent from any device).
//! Rows follow Android's ChatDatabase: one per (owner, peer, id); an edit or delete that comes before its message
//! is kept so the message stays edited/deleted when it arrives. Reading the newest messages again starts ten
//! minutes before the last look; the first look goes 30 days back.

use crate::App;
use drx_social::chat::{self, ChatWord, Payload};
use drx_social::relay::SubEvent;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FIRST_LOOK_S: i64 = 30 * 86_400;
const OVERLAP_S: i64 = 600;

/// One message as the chat shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRow {
    pub id: String,
    pub mine: bool,
    pub ts: i64,
    pub text: String,
    pub edited: bool,
    /// Mine: 0 sending, 1 sent, 2 failed; theirs: 3.
    pub state: i64,
    /// Mine: the friend has read it.
    pub read: bool,
    /// A shared track, album, playlist or artist.
    pub music: Option<ChatMusic>,
}

/// A music card in a message (spec §1.4 ChatMusic): `kind` is track | album | playlist | artist.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMusic {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub by: String,
    pub cover: Option<String>,
    pub duration_ms: i64,
}

impl ChatMusic {
    fn parse(json: &str) -> Option<Self> {
        let o: serde_json::Value = serde_json::from_str(json).ok()?;
        let s = |k: &str| o.get(k).and_then(serde_json::Value::as_str).unwrap_or("").to_string();
        let id = s("id");
        if id.is_empty() {
            return None;
        }
        Some(ChatMusic {
            kind: Some(s("k")).filter(|k| !k.is_empty()).unwrap_or_else(|| "track".into()),
            id,
            title: s("t"),
            by: s("a"),
            cover: Some(s("i")).filter(|u| u.starts_with("http")),
            duration_ms: o.get("d").and_then(serde_json::Value::as_i64).unwrap_or(0),
        })
    }
}

pub(crate) struct ChatStore {
    db: Mutex<Connection>,
    /// Bumped on every change; the UI redraws when it moves.
    pub version: tokio::sync::watch::Sender<u64>,
    live: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn now_ms() -> i64 {
    crate::songs::now_ms()
}

impl ChatStore {
    fn open(path: &std::path::Path) -> rusqlite::Result<ChatStore> {
        let c = Connection::open(path)?;
        // Thousands of messages arrive at once on the first look: WAL without a sync per row keeps that quick.
        c.busy_timeout(Duration::from_secs(5))?;
        c.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS message (owner TEXT NOT NULL, peer TEXT NOT NULL, id TEXT NOT NULL,
                 mine INTEGER NOT NULL, ts INTEGER NOT NULL, text TEXT NOT NULL, music TEXT, replyTo TEXT,
                 state INTEGER NOT NULL, read INTEGER NOT NULL DEFAULT 0, edited INTEGER NOT NULL DEFAULT 0,
                 deleted INTEGER NOT NULL DEFAULT 0, eventId TEXT, PRIMARY KEY (owner, peer, id));
             CREATE INDEX IF NOT EXISTS message_ts ON message (owner, peer, ts);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);",
        )?;
        Ok(ChatStore {
            db: Mutex::new(c),
            version: tokio::sync::watch::channel(0).0,
            live: Mutex::default(),
        })
    }

    fn bump(&self) {
        self.version.send_modify(|v| *v += 1);
    }

    fn cursor(&self, me: &str) -> i64 {
        let c = self.db.lock().expect("chat db");
        c.query_row("SELECT value FROM meta WHERE key = ?1", [format!("cursor_{me}")], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    fn advance(&self, me: &str, created_at: i64) {
        let now = now_ms() / 1000;
        if created_at > now + 60 {
            return;
        }
        let c = self.db.lock().expect("chat db");
        let _ = c.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = max(value, excluded.value)",
            params![format!("cursor_{me}"), created_at],
        );
    }

    /// Applies one decoded word (ChatHub.apply). True when something changed.
    fn apply(&self, me: &str, w: &ChatWord) -> bool {
        let c = self.db.lock().expect("chat db");
        let state = if w.mine { 1 } else { 3 };
        let r = match &w.payload {
            Payload::Message { id, ts, text, music, reply_to } => c.execute(
                "INSERT OR IGNORE INTO message (owner, peer, id, mine, ts, text, music, replyTo, state, eventId)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![me, w.peer, id, w.mine, ts, text, music, reply_to, state, w.event_id],
            ),
            Payload::Edit { id, ts, text } => {
                let n = c
                    .execute(
                        "UPDATE message SET text = ?4, edited = 1 WHERE owner = ?1 AND peer = ?2 AND id = ?3 AND deleted = 0",
                        params![me, w.peer, id, text],
                    )
                    .unwrap_or(0);
                if n == 0 {
                    c.execute(
                        "INSERT OR IGNORE INTO message (owner, peer, id, mine, ts, text, state, edited) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
                        params![me, w.peer, id, w.mine, ts, text, state],
                    )
                } else {
                    Ok(n)
                }
            }
            Payload::Delete { id, ts } => {
                let n = c
                    .execute(
                        "UPDATE message SET text = '', music = NULL, deleted = 1 WHERE owner = ?1 AND peer = ?2 AND id = ?3",
                        params![me, w.peer, id],
                    )
                    .unwrap_or(0);
                if n == 0 {
                    c.execute(
                        "INSERT OR IGNORE INTO message (owner, peer, id, mine, ts, text, state, deleted) VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, 1)",
                        params![me, w.peer, id, w.mine, ts, state],
                    )
                } else {
                    Ok(n)
                }
            }
            // Their «read» marks my messages read; my own «read» (from the phone) marks theirs read.
            Payload::Read { up_to } => c.execute(
                "UPDATE message SET read = 1 WHERE owner = ?1 AND peer = ?2 AND mine = ?3 AND ts <= ?4 AND read = 0",
                params![me, w.peer, !w.mine, up_to],
            ),
            Payload::Typing | Payload::Reaction { .. } => Ok(0),
        };
        r.map(|n| n > 0).unwrap_or(false)
    }
}

impl App {
    fn chat_store(&self) -> Option<Arc<ChatStore>> {
        self.inner
            .chats
            .get_or_init(|| ChatStore::open(&self.inner.profile.root().join("chat.db")).ok().map(Arc::new))
            .clone()
    }

    /// Changes of the chats (a counter): redraw when it moves.
    pub fn chats_version(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.chat_store().map(|s| s.version.subscribe())
    }

    /// Starts reading chats from the relays (both directions) while the app runs.
    pub fn start_chats_live(&self) {
        let (Some(store), Some(friends)) = (self.chat_store(), self.friends()) else { return };
        let mut live = store.live.lock().expect("chat live");
        if live.is_some() {
            return;
        }
        let keys = friends.keys();
        let me = keys.public_hex();
        let cursor = store.cursor(&me);
        let now = now_ms() / 1000;
        let since = if cursor == 0 { now - FIRST_LOOK_S } else { cursor.min(now) - OVERLAP_S };
        let pool = friends.pool();
        let store2 = store.clone();
        *live = Some(self.runtime.spawn(async move {
            let mut sub = pool.subscribe(vec![
                json!({"kinds": [4, 21004], "#p": [me], "since": since}),
                json!({"kinds": [4], "authors": [me], "since": since}),
            ]);
            let mut pending = false;
            let mut drawn = std::time::Instant::now();
            // The cursor moves only once every relay has sent what it stored: relays send newest first, so an
            // app closed during that first flow would otherwise skip the older messages next time.
            let (mut done, mut newest) = (0usize, 0i64);
            loop {
                // While messages keep coming (the first look), the chat still redraws once a second.
                if pending && drawn.elapsed() >= Duration::from_secs(1) {
                    pending = false;
                    drawn = std::time::Instant::now();
                    store2.bump();
                }
                let next = if pending {
                    match tokio::time::timeout(Duration::from_millis(300), sub.next()).await {
                        Ok(m) => m,
                        Err(_) => {
                            pending = false;
                            drawn = std::time::Instant::now();
                            store2.bump();
                            continue;
                        }
                    }
                } else {
                    sub.next().await
                };
                let e = match next {
                    None => return,
                    Some(SubEvent::Done) => {
                        done += 1;
                        if done == drx_social::relay::RELAYS.len() && newest > 0 {
                            store2.advance(&me, newest);
                        }
                        continue;
                    }
                    Some(SubEvent::Event(e)) => e,
                };
                if let Some(w) = chat::decode(&keys, &e) {
                    if store2.apply(&me, &w) {
                        pending = true;
                    }
                    if done >= drx_social::relay::RELAYS.len() {
                        store2.advance(&me, e.created_at);
                    } else {
                        newest = newest.max(e.created_at);
                    }
                }
            }
        }));
    }

    /// Conversations, newest first: (peer, last message text, its time, unread count).
    pub fn chat_previews(&self) -> Vec<(String, String, i64, i64)> {
        let (Some(store), Some(f)) = (self.chat_store(), self.friends()) else { return Vec::new() };
        let me = f.me();
        let c = store.db.lock().expect("chat db");
        let Ok(mut st) = c.prepare(
            "SELECT m.peer, m.text, m.ts, (SELECT count(*) FROM message u WHERE u.owner = m.owner AND u.peer = m.peer
                 AND u.mine = 0 AND u.read = 0 AND u.deleted = 0)
             FROM message m WHERE m.owner = ?1 AND m.deleted = 0 AND m.ts = (SELECT max(x.ts) FROM message x
                 WHERE x.owner = m.owner AND x.peer = m.peer AND x.deleted = 0)
             GROUP BY m.peer ORDER BY m.ts DESC",
        ) else {
            return Vec::new();
        };
        st.query_map([&me], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map(|it| it.flatten().collect())
            .unwrap_or_default()
    }

    /// A conversation, oldest first (the newest `limit`).
    pub fn chat_messages(&self, peer: &str, limit: usize) -> Vec<ChatRow> {
        let (Some(store), Some(f)) = (self.chat_store(), self.friends()) else { return Vec::new() };
        let me = f.me();
        let c = store.db.lock().expect("chat db");
        let Ok(mut st) = c.prepare(
            "SELECT id, mine, ts, text, edited, state, read, music FROM message
             WHERE owner = ?1 AND peer = ?2 AND deleted = 0 ORDER BY ts DESC, id DESC LIMIT ?3",
        ) else {
            return Vec::new();
        };
        let mut rows: Vec<ChatRow> = st
            .query_map(params![me, peer, limit as i64], |r| {
                Ok(ChatRow {
                    id: r.get(0)?,
                    mine: r.get(1)?,
                    ts: r.get(2)?,
                    text: r.get(3)?,
                    edited: r.get(4)?,
                    state: r.get(5)?,
                    read: r.get(6)?,
                    music: r.get::<_, Option<String>>(7)?.as_deref().and_then(ChatMusic::parse),
                })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        rows.reverse();
        rows
    }

    /// The chat is on screen: the friend's messages are read; they hear it when the settings allow.
    pub fn chat_opened(&self, peer: &str) {
        let (Some(store), Some(f)) = (self.chat_store(), self.friends()) else { return };
        let me = f.me();
        let newest: i64 = {
            let c = store.db.lock().expect("chat db");
            let newest = c
                .query_row(
                    "SELECT coalesce(max(ts), 0) FROM message WHERE owner = ?1 AND peer = ?2 AND mine = 0 AND read = 0",
                    params![me, peer],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let _ = c.execute(
                "UPDATE message SET read = 1 WHERE owner = ?1 AND peer = ?2 AND mine = 0",
                params![me, peer],
            );
            newest
        };
        if newest == 0 {
            return;
        }
        store.bump();
        if crate::social::may_publish_pub() && f.privacy().read_receipts {
            let keys = f.keys();
            let (pool, peer) = (f.pool(), peer.to_string());
            self.runtime.spawn(async move {
                if let Some(e) = chat::read_event(&keys, &peer, newest) {
                    pool.publish(&e, Duration::from_secs(10)).await;
                }
            });
        }
    }

    /// Sends a text message: shown at once as «sending», then «sent» or «failed».
    pub fn send_chat(&self, peer: &str, text: &str) -> crate::AppResult<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let (store, f) = self
            .chat_store()
            .zip(self.friends())
            .ok_or_else(|| crate::AppError::Other("нет профиля друзей".into()))?;
        if !crate::social::may_publish_pub() {
            return Err(crate::AppError::Other("в тестовом запуске сообщения не отправляю".into()));
        }
        let keys = f.keys();
        let me = keys.public_hex();
        let id = chat::new_id();
        let ts = now_ms();
        let event = chat::message_event(&keys, peer, &id, ts, text, None)
            .ok_or_else(|| crate::AppError::Other("не удалось зашифровать".into()))?;
        {
            let c = store.db.lock().expect("chat db");
            c.execute(
                "INSERT OR IGNORE INTO message (owner, peer, id, mine, ts, text, state, read, eventId) VALUES (?1, ?2, ?3, 1, ?4, ?5, 0, 0, ?6)",
                params![me, peer, id, ts, text, event.id],
            )
            .map_err(|e| crate::AppError::Other(e.to_string()))?;
        }
        store.bump();
        let (pool, peer) = (f.pool(), peer.to_string());
        self.runtime.spawn(async move {
            let ok = pool.publish(&event, Duration::from_secs(20)).await > 0;
            {
                let c = store.db.lock().expect("chat db");
                let _ = c.execute(
                    "UPDATE message SET state = ?4 WHERE owner = ?1 AND peer = ?2 AND id = ?3",
                    params![me, peer, id, if ok { 1 } else { 2 }],
                );
            }
            store.bump();
        });
        Ok(())
    }
}
