//! Chat messages, as Android's `friends/ChatHub.kt` (spec/CRITICAL.md §1.4): kind 4 direct messages with a `p`
//! tag, NIP-04 content of `{"t":"drxw-chat","v":2,"k":…,"id":…,"ts":…}`; reactions are `drxw-react`; «typing» is
//! kind 21004. Both directions are read here: what friends sent to this identity and what this identity sent to
//! them from any device (the phone keeps its own sent messages only in its local database).

use crate::nostr::{Event, KIND_DM, KIND_TYPING, Keys};
use serde_json::{Value, json};

pub const TYPE: &str = "drxw-chat";
pub const TYPE_REACT: &str = "drxw-react";
pub const PROTOCOL: i64 = 2;
pub const MAX_TEXT: usize = 4000;

/// One decoded chat word.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Message { id: String, ts: i64, text: String, music: Option<String>, reply_to: Option<String> },
    Edit { id: String, ts: i64, text: String },
    Delete { id: String, ts: i64 },
    Read { up_to: i64 },
    Typing,
    Reaction { id: String, ts: i64, keys: Vec<String> },
}

/// A chat word with who it is between: `peer` is the friend, `mine` = this identity wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatWord {
    pub peer: String,
    pub mine: bool,
    pub payload: Payload,
    pub event_id: String,
    pub created_at: i64,
}

/// Decodes a kind 4 / 21004 event between this identity and someone else; None for anything else.
pub fn decode(keys: &Keys, e: &Event) -> Option<ChatWord> {
    if e.kind != KIND_DM && e.kind != KIND_TYPING {
        return None;
    }
    let me = keys.public_hex();
    let p = e.tag("p")?;
    let (peer, mine) = if e.pubkey == me {
        (p.to_string(), true)
    } else if p == me {
        (e.pubkey.clone(), false)
    } else {
        return None;
    };
    if peer == me {
        return None;
    }
    let plain = keys.decrypt(&peer, &e.content).ok()?;
    let o: Value = serde_json::from_str(&plain).ok()?;
    let t = o.get("t").and_then(Value::as_str)?;
    let id = o.get("id").and_then(Value::as_str).filter(|i| !i.is_empty() && i.len() <= 64).unwrap_or("").to_string();
    // The sender's clock, never later than a minute after the relay got it.
    let ts = o
        .get("ts")
        .and_then(Value::as_i64)
        .filter(|t| *t > 0)
        .map(|t| t.min(e.created_at * 1000 + 60_000))
        .unwrap_or(e.created_at * 1000);
    let text = || o.get("text").and_then(Value::as_str).unwrap_or("").chars().take(MAX_TEXT).collect::<String>();
    let payload = if t == TYPE_REACT {
        Payload::Reaction {
            id,
            ts,
            keys: o
                .get("r")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).take(3).collect())
                .unwrap_or_default(),
        }
    } else if t == TYPE {
        match o.get("k").and_then(Value::as_str).unwrap_or("") {
            _ if e.kind == KIND_TYPING => Payload::Typing,
            "msg" => Payload::Message {
                id,
                ts,
                text: text(),
                music: o.get("m").filter(|m| m.is_object()).map(|m| m.to_string()),
                reply_to: o.get("re").and_then(Value::as_str).filter(|r| !r.is_empty()).map(str::to_string),
            },
            "edit" => Payload::Edit { id, ts, text: text() },
            "del" => Payload::Delete { id, ts },
            "read" => Payload::Read { up_to: o.get("upTo").and_then(Value::as_i64).unwrap_or(0) },
            // A newer app's message with text is still shown.
            _ if !text().is_empty() => Payload::Message { id, ts, text: text(), music: None, reply_to: None },
            _ => return None,
        }
    } else {
        return None;
    };
    if matches!(payload, Payload::Message { ref id, .. } | Payload::Edit { ref id, .. } | Payload::Delete { ref id, .. } if id.is_empty()) {
        return None;
    }
    Some(ChatWord { peer, mine, payload, event_id: e.id.clone(), created_at: e.created_at })
}

/// A new random message id (UUID v4 text, as Android's UUID.randomUUID()).
pub fn new_id() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("system randomness");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// The signed kind 4 event of a text message to `peer`.
pub fn message_event(keys: &Keys, peer: &str, id: &str, ts: i64, text: &str, reply_to: Option<&str>) -> Option<Event> {
    let mut o = json!({"t": TYPE, "v": PROTOCOL, "k": "msg", "id": id, "ts": ts, "text": text.chars().take(MAX_TEXT).collect::<String>()});
    if let Some(r) = reply_to {
        o["re"] = json!(r);
    }
    let content = keys.encrypt(peer, &o.to_string()).ok()?;
    Some(keys.sign(KIND_DM, vec![vec!["p".into(), peer.to_string()]], content))
}

/// «Read up to `up_to`» to `peer`.
pub fn read_event(keys: &Keys, peer: &str, up_to: i64) -> Option<Event> {
    let o = json!({"t": TYPE, "v": PROTOCOL, "k": "read", "id": new_id(), "upTo": up_to});
    let content = keys.encrypt(peer, &o.to_string()).ok()?;
    Some(keys.sign(KIND_DM, vec![vec!["p".into(), peer.to_string()]], content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_directions_decode() {
        let (me, friend) = (Keys::generate(), Keys::generate());
        let out = message_event(&me, &friend.public_hex(), "m1", 1000, "привет", None).unwrap();
        // My own message read back on another device of mine.
        let w = decode(&me, &out).unwrap();
        assert!(w.mine && w.peer == friend.public_hex());
        assert!(matches!(w.payload, Payload::Message { ref text, .. } if text == "привет"));
        // The friend's side.
        let w = decode(&friend, &out).unwrap();
        assert!(!w.mine && w.peer == me.public_hex());
        // A third key sees nothing.
        assert!(decode(&Keys::generate(), &out).is_none());
        let r = read_event(&friend, &me.public_hex(), 5000).unwrap();
        assert_eq!(decode(&me, &r).unwrap().payload, Payload::Read { up_to: 5000 });
        assert_eq!(new_id().len(), 36);
    }
}
