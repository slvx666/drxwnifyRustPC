//! Chat messages, as Android's `friends/ChatHub.kt` (spec/CRITICAL.md §1.4): kind 4 direct messages with a `p`
//! tag, NIP-04 content of `{"t":"drxw-chat","v":2,"k":…,"id":…,"ts":…}`; reactions are `drxw-react`; «typing» is
//! kind 21004. Both directions are read here: what friends sent to this identity and what this identity sent to
//! them from any device (the phone keeps its own sent messages only in its local database).

use crate::nostr::{Event, KIND_DELETE, KIND_DM, KIND_TYPING, Keys};
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
                .map(|a| {
                    let mut v: Vec<String> = Vec::new();
                    for k in a.iter().filter_map(Value::as_str).filter(|k| well_formed_reaction(k)) {
                        if !v.iter().any(|x| x == k) {
                            v.push(k.to_string());
                        }
                    }
                    v.truncate(MAX_REACTIONS);
                    v
                })
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
    music_message_event(keys, peer, id, ts, text, None, reply_to)
}

/// A message with a music card (`m`: ChatMusic.toJson — k, id, t, a, i, d).
pub fn music_message_event(
    keys: &Keys,
    peer: &str,
    id: &str,
    ts: i64,
    text: &str,
    music: Option<&Value>,
    reply_to: Option<&str>,
) -> Option<Event> {
    let mut o = json!({"t": TYPE, "v": PROTOCOL, "k": "msg", "id": id, "ts": ts, "text": text.chars().take(MAX_TEXT).collect::<String>()});
    if let Some(m) = music {
        o["m"] = m.clone();
    }
    if let Some(r) = reply_to {
        o["re"] = json!(r);
    }
    let content = keys.encrypt(peer, &o.to_string()).ok()?;
    Some(keys.sign(KIND_DM, vec![vec!["p".into(), peer.to_string()]], content))
}

/// «I'm typing» to `peer` (kind 21004, not kept by relays; ChatHub.typingNow).
pub fn typing_event(keys: &Keys, peer: &str, ts: i64) -> Option<Event> {
    let o = json!({"t": TYPE, "v": PROTOCOL, "k": "typing", "id": "t", "ts": ts});
    let content = keys.encrypt(peer, &o.to_string()).ok()?;
    Some(keys.sign(KIND_TYPING, vec![vec!["p".into(), peer.to_string()]], content))
}

/// «Read up to `up_to`» to `peer`.
pub fn read_event(keys: &Keys, peer: &str, up_to: i64) -> Option<Event> {
    let o = json!({"t": TYPE, "v": PROTOCOL, "k": "read", "id": new_id(), "upTo": up_to});
    let content = keys.encrypt(peer, &o.to_string()).ok()?;
    Some(keys.sign(KIND_DM, vec![vec!["p".into(), peer.to_string()]], content))
}

fn sealed(keys: &Keys, peer: &str, o: &Value) -> Option<Event> {
    let content = keys.encrypt(peer, &o.to_string()).ok()?;
    Some(keys.sign(KIND_DM, vec![vec!["p".into(), peer.to_string()]], content))
}

/// «My message `id` now reads `text`» to `peer`.
pub fn edit_event(keys: &Keys, peer: &str, id: &str, ts: i64, text: &str) -> Option<Event> {
    let text: String = text.chars().take(MAX_TEXT).collect();
    sealed(keys, peer, &json!({"t": TYPE, "v": PROTOCOL, "k": "edit", "id": id, "ts": ts, "text": text}))
}

/// «My message `id` is gone» to `peer` (their app hides it too).
pub fn delete_event(keys: &Keys, peer: &str, id: &str, ts: i64) -> Option<Event> {
    sealed(keys, peer, &json!({"t": TYPE, "v": PROTOCOL, "k": "del", "id": id, "ts": ts}))
}

/// Asks relays to drop a stored event of mine (NIP-09, kind 5).
pub fn relay_delete_event(keys: &Keys, event_id: &str) -> Event {
    keys.sign(KIND_DELETE, vec![vec!["e".into(), event_id.to_string()]], String::new())
}

/// My whole set of reactions on message `id` (at most three keys «e:<emoji>» / «s:<sticker>»).
pub fn reaction_event(keys: &Keys, peer: &str, id: &str, ts: i64, reaction_keys: &[String]) -> Option<Event> {
    sealed(keys, peer, &json!({"t": TYPE_REACT, "v": PROTOCOL, "id": id, "ts": ts, "r": reaction_keys}))
}

/// A reaction key as the phone accepts it (ChatReactions.isWellFormed).
pub fn well_formed_reaction(key: &str) -> bool {
    (3..=48).contains(&key.len()) && !key.contains('\n') && (key.starts_with("e:") || key.starts_with("s:"))
}

/// Puts `key` on or takes it off; at most three, the oldest goes first (ChatReactions.toggle).
pub fn toggle_reaction(current: &[String], key: &str) -> Vec<String> {
    if current.iter().any(|k| k == key) {
        current.iter().filter(|k| *k != key).cloned().collect()
    } else {
        let mut v = current.to_vec();
        v.push(key.to_string());
        let skip = v.len().saturating_sub(MAX_REACTIONS);
        v.into_iter().skip(skip).collect()
    }
}

pub const MAX_REACTIONS: usize = 3;

/// The phone's sticker list (assets/stickers/stickers.json of the Android app: ids and codes).
const STICKERS: &str = include_str!("stickers.json");
/// A sticker inside a message: U+E000, its id, U+E001 (Stickers.token).
const STICKER_START: char = '\u{E000}';
const STICKER_END: char = '\u{E001}';

/// Mini's sticker pictures (the phone's still stickers, scripts/stickers.py): id and width/height per line.
const MINI_STICKERS: &str = include_str!("stickers_mini.txt");

/// Where sticker `id` stands among Mini's pictures, with its width/height; None for an animated or unknown one.
pub fn mini_sticker(id: &str) -> Option<(usize, f32)> {
    MINI_STICKERS.lines().enumerate().find_map(|(i, l)| {
        let (lid, ratio) = l.split_once(' ')?;
        (lid == id).then(|| (i, ratio.trim().parse().unwrap_or(1.0)))
    })
}

/// Every sticker Mini can draw, in picture order: (id, width/height).
pub fn mini_stickers() -> Vec<(String, f32)> {
    MINI_STICKERS
        .lines()
        .filter_map(|l| {
            let (id, r) = l.split_once(' ')?;
            Some((id.to_string(), r.trim().parse().unwrap_or(1.0)))
        })
        .collect()
}

/// A sticker as it travels inside a message (Stickers.token).
pub fn sticker_token(id: &str) -> String {
    format!("{STICKER_START}{id}{STICKER_END}")
}

/// The sticker ids of a message made of stickers alone (spaces between them allowed): shown big, as on the phone.
pub fn sticker_only(text: &str) -> Option<Vec<String>> {
    let mut ids = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let after = rest.strip_prefix(STICKER_START)?;
        let end = after.find(STICKER_END)?;
        ids.push(after[..end].to_string());
        rest = after[end + STICKER_END.len_utf8()..].trim_start();
    }
    (!ids.is_empty()).then_some(ids)
}

/// The text with stickers as their codes (Stickers.plain): the chat list and, until the pictures come, the bubbles.
pub fn plain_stickers(text: &str) -> String {
    if !text.contains(STICKER_START) && !text.contains(STICKER_END) {
        return text.to_string();
    }
    static CODES: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    let codes = CODES.get_or_init(|| {
        serde_json::from_str::<Value>(STICKERS)
            .ok()
            .and_then(|v| v.get("stickers").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|s| Some((s.get("id")?.as_str()?.to_string(), s.get("code")?.as_str()?.to_string())))
            .collect()
    });
    let mut out = String::new();
    let mut rest = text;
    while let Some(a) = rest.find(STICKER_START) {
        out.push_str(&rest[..a].replace(STICKER_END, ""));
        let after = &rest[a + STICKER_START.len_utf8()..];
        match after.find(STICKER_END) {
            Some(b) if (1..=40).contains(&b) && after[..b].bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_') => {
                out.push_str(codes.get(&after[..b]).map_or("[стикер]", String::as_str));
                rest = &after[b + STICKER_END.len_utf8()..];
            }
            _ => rest = after,
        }
    }
    out.push_str(&rest.replace(STICKER_END, ""));
    out
}

/// The built-in emoji reactions, in Android's order (ChatReactions.EMOJI; the list comes from scripts/emoji.py).
pub const EMOJI: &str = include_str!("emoji.txt");

/// Where the emoji of `key` («e:<emoji>») stands in [EMOJI]; None for a sticker or an emoji this version lacks.
pub fn emoji_index(key: &str) -> Option<usize> {
    let e = key.strip_prefix("e:")?;
    EMOJI.lines().position(|x| x == e)
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
        let r = reaction_event(&friend, &me.public_hex(), "m1", 6000, &["e:x1".into(), "bad".into()]).unwrap();
        assert_eq!(decode(&me, &r).unwrap().payload, Payload::Reaction { id: "m1".into(), ts: 6000, keys: vec!["e:x1".into()] });
        let e = edit_event(&me, &friend.public_hex(), "m1", 7000, "ok").unwrap();
        assert!(matches!(decode(&friend, &e).unwrap().payload, Payload::Edit { ref text, .. } if text == "ok"));
        let t = toggle_reaction(&["e:a".into(), "e:b".into(), "e:c".into()], "e:d");
        assert_eq!(t, vec!["e:b".to_string(), "e:c".into(), "e:d".into()]);
        assert_eq!(toggle_reaction(&t, "e:c"), vec!["e:b".to_string(), "e:d".into()]);
        assert_eq!(emoji_index("e:\u{2764}\u{fe0f}"), Some(0));
        assert_eq!(EMOJI.lines().count(), 40);
        assert_eq!(emoji_index("s:x"), None);
        assert_eq!(plain_stickers("ok \u{E000}aga\u{E001}!"), "ok aga!");
        assert_eq!(plain_stickers("\u{E000}nope_x\u{E001}"), "[стикер]");
        assert_eq!(sticker_only(" \u{E000}aga\u{E001} \u{E000}hmm\u{E001}"), Some(vec!["aga".to_string(), "hmm".into()]));
        assert_eq!(sticker_only("hi \u{E000}aga\u{E001}"), None);
        assert_eq!(mini_sticker("aga").map(|s| s.0), Some(0));
        let ty = typing_event(&me, &friend.public_hex(), 9000).unwrap();
        assert_eq!(decode(&friend, &ty).unwrap().payload, Payload::Typing);
    }
}
