//! «Статус в Discord» (Android Discord Rich Presence, here over the desktop client's IPC): what plays shows in the
//! listener's Discord profile while the switch is on; paused or stopped, the status goes.

use drx_app::App;
use drx_integrations::discord::{Presence, Rpc};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const KEY: &str = "pcDiscordRpc";
/// The phone's «Расширенный режим» and its options (MusicService.updateDiscordRPC reads them only in that mode).
pub const ADVANCED_KEY: &str = "discordAdvancedMode";
pub const TYPE_KEY: &str = "discordActivityType";
pub const BUTTON_KEY: &str = "discordButton1Visible";
/// discordActivityType values and the Discord activity type of each.
pub const TYPES: [(&str, &str, u8); 4] = [("listening", "Слушает", 2), ("playing", "Играет", 0), ("watching", "Смотрит", 3), ("competing", "Соревнуется", 5)];


static RPC: OnceLock<Mutex<Option<Rpc>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Rpc>> {
    RPC.get_or_init(|| Mutex::new(None))
}

/// The switch: on starts talking to Discord, off clears the status and stops.
pub fn set_enabled(app: &App, on: bool) {
    let mut s = slot().lock().expect("discord");
    match (on, s.is_some()) {
        (true, false) => {
            let rpc = Rpc::start();
            rpc.set(presence(app));
            *s = Some(rpc);
        }
        (false, true) => {
            if let Some(r) = s.take() {
                r.set(None);
            }
        }
        _ => {}
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// What plays now as a presence; None when nothing plays.
fn presence(app: &App) -> Option<Presence> {
    let (track, playing) = {
        let n = app.now_playing();
        let n = n.borrow();
        (n.track.clone(), n.playing)
    };
    let t = track.filter(|_| playing)?;
    let advanced = app.pref_bool(ADVANCED_KEY).unwrap_or(false);
    let c = app.clock();
    let now = now_ms();
    let start = now - (c.position_s * 1000.0) as i64;
    let dur = c.duration_s.map(|d| (d * 1000.0) as i64).filter(|d| *d > 0).unwrap_or(t.duration_ms as i64);
    let link = drx_app::share_text("track", &t.id, &t.title, &t.artist_names()).lines().last().map(str::to_string);
    Some(Presence {
        title: t.title.clone(),
        artist: t.artist_names(),
        album: t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
        cover: t.album.as_ref().and_then(|a| drx_catalog::model::best_image(&a.images, 300)).map(|i| i.url.clone()),
        start_ms: Some(start),
        end_ms: (dur > 0).then_some(start + dur),
        link: link.filter(|_| !t.id.starts_with("local:")),
        kind: if advanced {
            let v = app.pref_string(TYPE_KEY).unwrap_or_default().to_lowercase();
            TYPES.iter().find(|x| x.0 == v).map_or(2, |x| x.2)
        } else {
            2
        },
        details: true,
        button: !advanced || app.pref_bool(BUTTON_KEY).unwrap_or(true),
    })
}

/// Starts with the setting and follows the playback: a new track, play / pause, and seeks (a start time off by
/// more than 3 s).
pub fn start(app: &App) {
    if app.pref_bool(KEY).unwrap_or(false) {
        set_enabled(app, true);
    }
    let app2 = app.clone();
    app.runtime().spawn(async move {
        let mut rx = app2.now_playing();
        let mut last: Option<Presence> = None;
        loop {
            let p = presence(&app2);
            let changed = match (&last, &p) {
                (Some(a), Some(b)) => {
                    a.title != b.title
                        || a.artist != b.artist
                        || (a.start_ms.unwrap_or(0) - b.start_ms.unwrap_or(0)).abs() > 3_000
                }
                (None, None) => false,
                _ => true,
            };
            if changed {
                if let Some(r) = slot().lock().expect("discord").as_ref() {
                    r.set(p.clone());
                }
                last = p;
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), rx.changed()).await;
        }
    });
}
