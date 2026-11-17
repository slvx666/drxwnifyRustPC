//! Discord Rich Presence through the Discord desktop client's local IPC (`\\.\pipe\discord-ipc-N`): no account
//! token, the running Discord shows «Слушает Drxwnify» with the track. Android talks to the gateway with the user's
//! token (Kizzy); on a computer the client's own IPC is the documented way. Frames: op (u32 LE), length (u32 LE),
//! JSON. Op 0 = handshake, 1 = frame, 2 = close.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

/// The Drxwnify application on Discord (Android DiscordRPC.APPLICATION_ID).
pub const APPLICATION_ID: &str = "1411019391843172514";

/// What to show; None clears the status.
#[derive(Debug, Clone, PartialEq)]
pub struct Presence {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub cover: Option<String>,
    /// Unix ms when the track started (position taken into account) and when it will end; None while paused.
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub link: Option<String>,
    /// Activity type: 2 «Слушает» (default), 0 «Играет», 3 «Смотрит», 5 «Соревнуется» (the phone's discordActivityType).
    pub kind: u8,
    /// Show the track and artist (discordUseDetails); off — only «Drxwnify».
    pub details: bool,
    /// The «Открыть в Drxwnify» button (discordButton1Visible).
    pub button: bool,
}

/// A running presence: updates go to a background thread that keeps the pipe (and reconnects when Discord starts).
pub struct Rpc {
    tx: mpsc::Sender<Option<Presence>>,
}

impl Rpc {
    pub fn start() -> Rpc {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new().name("discord-rpc".into()).spawn(move || run(rx));
        Rpc { tx }
    }

    pub fn set(&self, p: Option<Presence>) {
        let _ = self.tx.send(p);
    }
}

fn frame(op: u32, body: &Value) -> Vec<u8> {
    let data = body.to_string().into_bytes();
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

fn read_frame(pipe: &mut std::fs::File) -> Option<(u32, Value)> {
    let mut head = [0u8; 8];
    pipe.read_exact(&mut head).ok()?;
    let op = u32::from_le_bytes(head[0..4].try_into().ok()?);
    let len = u32::from_le_bytes(head[4..8].try_into().ok()?) as usize;
    if len > 1 << 20 {
        return None;
    }
    let mut body = vec![0u8; len];
    pipe.read_exact(&mut body).ok()?;
    Some((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

/// The first Discord pipe that opens, handshaken.
fn connect() -> Option<std::fs::File> {
    for i in 0..10 {
        let path = format!(r"\\.\pipe\discord-ipc-{i}");
        let Ok(mut pipe) = std::fs::OpenOptions::new().read(true).write(true).open(&path) else { continue };
        if pipe.write_all(&frame(0, &json!({"v": 1, "client_id": APPLICATION_ID}))).is_err() {
            continue;
        }
        // READY (or an error frame when the id is refused).
        match read_frame(&mut pipe) {
            Some((1, v)) if v.get("evt").and_then(Value::as_str) == Some("READY") => return Some(pipe),
            other => tracing::debug!("discord ipc handshake: {other:?}"),
        }
    }
    None
}

fn clip(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    // Discord wants 2..=128 characters.
    format!("{t:<2}")
}

/// The SET_ACTIVITY payload (type 2 = «Слушает»).
pub fn activity(p: &Presence) -> Value {
    let mut a = if p.details {
        json!({ "type": p.kind, "details": clip(&p.title, 128), "state": clip(&p.artist, 128), "instance": false })
    } else {
        json!({ "type": p.kind, "details": "Drxwnify", "instance": false })
    };
    let mut assets = serde_json::Map::new();
    if let Some(c) = p.cover.as_ref().filter(|c| c.starts_with("https://") && p.details) {
        assets.insert("large_image".into(), json!(c));
        if !p.album.is_empty() {
            assets.insert("large_text".into(), json!(clip(&p.album, 128)));
        }
    }
    if !assets.is_empty() {
        a["assets"] = Value::Object(assets);
    }
    if let (Some(s), Some(e)) = (p.start_ms, p.end_ms) {
        a["timestamps"] = json!({"start": s, "end": e});
    }
    if let Some(l) = p.link.as_ref().filter(|l| l.starts_with("https://") && p.button) {
        a["buttons"] = json!([{"label": "Открыть в Drxwnify", "url": l}]);
    }
    a
}

fn run(rx: mpsc::Receiver<Option<Presence>>) {
    let mut pipe: Option<std::fs::File> = None;
    let mut wanted: Option<Presence> = None;
    let mut sent: Option<Option<Presence>> = None;
    let pid = std::process::id();
    let mut nonce = 0u64;
    loop {
        // The newest wish (a burst of updates collapses to the last one).
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => {
                wanted = p;
                while let Ok(p) = rx.try_recv() {
                    wanted = p;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(mut p) = pipe.take() {
                    let _ = p.write_all(&frame(2, &json!({})));
                }
                return;
            }
        }
        if sent.as_ref() == Some(&wanted) && pipe.is_some() {
            continue;
        }
        if pipe.is_none() {
            if wanted.is_none() {
                sent = Some(None);
                continue;
            }
            pipe = connect();
            if pipe.is_none() {
                continue; // Discord is not running: try again with the next update or in 15 s.
            }
        }
        nonce += 1;
        let body = json!({
            "cmd": "SET_ACTIVITY",
            "args": {"pid": pid, "activity": wanted.as_ref().map(activity)},
            "nonce": nonce.to_string(),
        });
        let ok = pipe.as_mut().is_some_and(|p| p.write_all(&frame(1, &body)).is_ok() && read_frame(p).is_some());
        if ok {
            sent = Some(wanted.clone());
        } else {
            pipe = None;
            sent = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_activity() {
        let f = frame(1, &json!({"a": 1}));
        assert_eq!(&f[0..4], &1u32.to_le_bytes());
        assert_eq!(u32::from_le_bytes(f[4..8].try_into().unwrap()) as usize, f.len() - 8);
        let a = activity(&Presence {
            title: "Never".into(),
            artist: "Rick Astley".into(),
            album: "W".into(),
            cover: Some("https://i.scdn.co/image/x".into()),
            start_ms: Some(1000),
            end_ms: Some(2000),
            link: Some("https://drxwnify.pages.dev/track/x".into()),
            kind: 2,
            details: true,
            button: true,
        });
        assert_eq!(a["type"], 2);
        assert_eq!(a["assets"]["large_image"], "https://i.scdn.co/image/x");
        assert_eq!(a["assets"]["large_text"], "W ");
        assert_eq!(a["timestamps"]["end"], 2000);
        assert_eq!(a["buttons"][0]["label"], "Открыть в Drxwnify");
    }
}
