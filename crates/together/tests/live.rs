//! Live check against the public server (not run by default): `cargo test -p drx-together -- --ignored`.

use drx_together::{Client, DEFAULT_SERVER, Event, Role};
use std::time::Duration;
use tokio::sync::mpsc;

async fn wait<F: Fn(&drx_together::Room) -> bool>(c: &Client, f: F) -> drx_together::Room {
    let mut rx = c.room();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if f(&rx.borrow()) {
                return rx.borrow().clone();
            }
            rx.changed().await.expect("room channel");
        }
    })
    .await
    .expect("timed out")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn create_join_approve_leave() {
    let (htx, _hrx) = mpsc::unbounded_channel();
    let host = Client::start(DEFAULT_SERVER, "drx-pc-test-host", None, htx);
    let r = wait(&host, |r| r.role == Role::Host && !r.code.is_empty()).await;
    println!("room {}", r.code);
    let (gtx, mut grx) = mpsc::unbounded_channel();
    let guest = Client::start(DEFAULT_SERVER, "drx-pc-test-guest", Some(&r.code), gtx);
    let r = wait(&host, |r| !r.requests.is_empty()).await;
    println!("request from {:?}", r.requests);
    host.approve(&r.requests[0].0);
    let g = wait(&guest, |r| r.role == Role::Guest).await;
    println!(
        "guest in, users {:?}",
        g.users.iter().map(|u| &u.username).collect::<Vec<_>>()
    );
    assert!(matches!(grx.recv().await, Some(Event::Joined(_))));
    host.playback(&drx_together::PlaybackAction {
        action: "change_track".into(),
        track_id: "dQw4w9WgXcQ".into(),
        track_info: Some(drx_together::TrackInfo {
            id: "dQw4w9WgXcQ".into(),
            title: "Test".into(),
            artist: "Test".into(),
            duration: 200_000,
            ..Default::default()
        }),
        ..Default::default()
    });
    let got = tokio::time::timeout(Duration::from_secs(10), grx.recv())
        .await
        .expect("no sync");
    println!("guest got {got:?}");
    guest.leave();
    host.leave();
    tokio::time::sleep(Duration::from_millis(500)).await;
}
