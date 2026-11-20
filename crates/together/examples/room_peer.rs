//! Developer check of rooms with a real app on the other side.
//! `cargo run -p drx-together --example room_peer -- join <code> [seconds]` — a guest that prints what the host sends;
//! `cargo run -p drx-together --example room_peer -- host <track id> <title> <artist> [seconds]` — a host that lets
//! everyone in and plays that track.

use drx_together::{Client, DEFAULT_SERVER, PlaybackAction, Role, TrackInfo};
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (tx, mut rx) = mpsc::unbounded_channel();
    match args.first().map(String::as_str) {
        Some("join") => {
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
            let c = Client::start(DEFAULT_SERVER, "room-peer", Some(&args[1]), tx);
            let mut room = c.room();
            let end = tokio::time::sleep(Duration::from_secs(secs));
            tokio::pin!(end);
            loop {
                tokio::select! {
                    _ = &mut end => break,
                    e = rx.recv() => match e { Some(e) => println!("event {e:?}"), None => break },
                    r = room.changed() => { if r.is_err() { break } let r = room.borrow().clone(); println!("room role={:?} code={} users={:?} status={}", r.role, r.code, r.users.iter().map(|u| &u.username).collect::<Vec<_>>(), r.status); }
                }
            }
            c.leave();
        }
        Some("host") => {
            let secs: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(90);
            let c = Client::start(DEFAULT_SERVER, "room-peer-host", None, tx);
            let mut room = c.room();
            let info = TrackInfo {
                id: args[1].clone(),
                title: args[2].clone(),
                artist: args[3].clone(),
                duration: 213_000,
                ..Default::default()
            };
            let end = tokio::time::sleep(Duration::from_secs(secs));
            tokio::pin!(end);
            let mut sent = false;
            loop {
                tokio::select! {
                    _ = &mut end => break,
                    e = rx.recv() => match e { Some(e) => println!("event {e:?}"), None => break },
                    r = room.changed() => {
                        if r.is_err() { break }
                        let r = room.borrow().clone();
                        println!("room role={:?} code={} users={:?} requests={:?}", r.role, r.code, r.users.iter().map(|u| &u.username).collect::<Vec<_>>(), r.requests);
                        if r.role == Role::Host {
                            for (id, _) in &r.requests { c.approve(id); }
                            if r.users.len() > 1 && !sent {
                                sent = true;
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                c.playback(&PlaybackAction { action: "change_track".into(), track_id: info.id.clone(), track_info: Some(info.clone()), ..Default::default() });
                                tokio::time::sleep(Duration::from_millis(150)).await;
                                c.playback(&PlaybackAction { action: "play".into(), track_id: info.id.clone(), position: 0, ..Default::default() });
                                println!("sent change_track + play");
                            }
                        }
                    }
                }
            }
            c.leave();
        }
        _ => eprintln!("usage: room_peer join <code> | host <id> <title> <artist>"),
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
}
