//! Friends over Nostr, as Android's `friends/FriendsHub.kt` (spec/CRITICAL.md §1.3): the same friends list and
//! privacy (the phone's `friends` shared prefs), the same fields (`drxw:<field>` public or `drxw:<field>:<friend>`
//! encrypted to one friend, at most 40), the same JSON. A field is taken only from an event newer than the one
//! already applied. The developer's public profile is followed by everyone.

use crate::nostr::{Event, KIND_APP_DATA, KIND_METADATA, Keys};
use crate::relay::{Pool, SubEvent};
use base64::Engine;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

pub const DEVELOPER_PUBKEY: &str = "e114d6d2c422913fbb0c5b525cf927015aaecdfbea72fadd5c1dd2693f1c7f74";
pub const DEVELOPER_NAME: &str = "drxwnslvt";
pub const INVITE_BASE: &str = "https://drxwnify.pages.dev/add/";

const D_PREFIX: &str = "drxw:";
const DATA_FIELDS: &[&str] = &["now", "history", "playlists", "stats", "likes", "portrait"];
const MAX_FRIENDS_ENCRYPTED: usize = 40;
const MAX_AVATAR_CHARS: usize = 60_000;
const LIVE_SLACK_MS: i64 = 3 * 60_000;
const UNKNOWN_LENGTH_MS: i64 = 12 * 60_000;
pub const MAX_HISTORY: usize = 50;
const LIKES_PACKED_PREFIX: &str = "gz1:";

/// Who a field is shared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Audience {
    Nobody,
    Verified,
    #[default]
    Friends,
    Everyone,
}

impl Audience {
    fn from_name(s: &str) -> Audience {
        match s {
            "NOBODY" => Audience::Nobody,
            "VERIFIED" => Audience::Verified,
            "EVERYONE" => Audience::Everyone,
            _ => Audience::Friends,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Privacy {
    pub now: Audience,
    pub history: Audience,
    pub playlists: Audience,
    pub stats: Audience,
    pub discoverable: bool,
    pub listen_along: bool,
    pub read_receipts: bool,
}

impl Default for Privacy {
    fn default() -> Self {
        Privacy {
            now: Audience::Friends,
            history: Audience::Friends,
            playlists: Audience::Friends,
            stats: Audience::Friends,
            discoverable: false,
            listen_along: true,
            read_receipts: true,
        }
    }
}

impl Privacy {
    /// The phone's JSON (`{"now","history","playlists","stats","discoverable","along","read"}`).
    pub fn from_json(s: &str) -> Privacy {
        let Ok(o) = serde_json::from_str::<Value>(s) else {
            return Privacy::default();
        };
        let aud = |k: &str| o.get(k).and_then(Value::as_str).map(Audience::from_name).unwrap_or_default();
        Privacy {
            now: aud("now"),
            history: aud("history"),
            playlists: aud("playlists"),
            stats: aud("stats"),
            discoverable: o.get("discoverable").and_then(Value::as_bool).unwrap_or(false),
            listen_along: o.get("along").and_then(Value::as_bool).unwrap_or(true),
            read_receipts: o.get("read").and_then(Value::as_bool).unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Friend {
    pub pubkey: String,
    pub name: String,
    pub added_at: i64,
}

/// A track as friends exchange it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackRef {
    pub title: String,
    pub artist: String,
    pub catalog_id: Option<String>,
    pub media_id: Option<String>,
    pub cover: Option<String>,
    pub duration_ms: i64,
    pub at: i64,
}

impl TrackRef {
    pub fn to_json(&self) -> Value {
        let mut o = json!({"t": self.title, "a": self.artist});
        if let Some(c) = &self.catalog_id {
            o["c"] = json!(c);
        }
        if let Some(m) = &self.media_id {
            o["m"] = json!(m);
        }
        if let Some(i) = &self.cover {
            o["i"] = json!(i);
        }
        if self.duration_ms > 0 {
            o["d"] = json!(self.duration_ms);
        }
        if self.at > 0 {
            o["at"] = json!(self.at);
        }
        o
    }

    pub fn from_json(o: &Value) -> TrackRef {
        let s = |k: &str| o.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string);
        TrackRef {
            title: s("t").unwrap_or_default(),
            artist: s("a").unwrap_or_default(),
            catalog_id: s("c"),
            media_id: s("m"),
            cover: s("i"),
            duration_ms: o.get("d").and_then(Value::as_i64).unwrap_or(0),
            at: o.get("at").and_then(Value::as_i64).unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub track: TrackRef,
    pub position_ms: i64,
    pub playing: bool,
    pub at: i64,
    pub listen_along: bool,
}

impl NowPlaying {
    pub fn to_json(&self) -> Value {
        json!({"track": self.track.to_json(), "pos": self.position_ms, "playing": self.playing, "at": self.at, "along": self.listen_along})
    }

    pub fn from_json(o: &Value) -> Option<NowPlaying> {
        Some(NowPlaying {
            track: TrackRef::from_json(o.get("track")?),
            position_ms: o.get("pos").and_then(Value::as_i64).unwrap_or(0),
            playing: o.get("playing").and_then(Value::as_bool).unwrap_or(false),
            at: o.get("at").and_then(Value::as_i64).unwrap_or(0),
            listen_along: o.get("along").and_then(Value::as_bool).unwrap_or(true),
        })
    }

    pub fn position_now(&self, now_ms: i64) -> i64 {
        if self.playing {
            self.position_ms + (now_ms - self.at).max(0)
        } else {
            self.position_ms
        }
    }

    /// Playing right now as far as one can tell (Android NowPlaying.isLive): the last word was "playing" and the
    /// track would not have ended yet (a few minutes' slack for clocks and slow networks).
    pub fn is_live(&self, now_ms: i64) -> bool {
        if !self.playing {
            return false;
        }
        let left = if self.track.duration_ms > 0 {
            (self.track.duration_ms - self.position_ms).max(0)
        } else {
            UNKNOWN_LENGTH_MS
        };
        now_ms <= self.at + left + LIVE_SLACK_MS
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SharedPlaylist {
    pub name: String,
    pub total: i64,
    pub tracks: Vec<TrackRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SharedLikes {
    pub total: i64,
    pub source: Option<String>,
    pub tracks: Vec<TrackRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub week_minutes: i64,
    pub week_plays: i64,
    pub liked: i64,
    pub top: Vec<(String, i64)>,
}

/// What is known about one friend (or the developer, or oneself).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FriendData {
    pub name: Option<String>,
    /// A small JPEG, base64.
    pub avatar: Option<String>,
    pub now: Option<NowPlaying>,
    pub history: Option<Vec<TrackRef>>,
    pub playlists: Option<Vec<SharedPlaylist>>,
    pub stats: Option<Stats>,
    pub likes: Option<SharedLikes>,
    /// The musical portrait (built on the phone once a day).
    pub portrait: Option<crate::portrait::ProfilePortrait>,
    /// Newest applied event per field (created_at, s).
    pub stamps: HashMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FriendRequest {
    pub pubkey: String,
    pub name: String,
    pub at: i64,
}

/// Everything the UI shows, sent on every change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FriendsState {
    pub me: Option<String>,
    pub friends: Vec<Friend>,
    pub data: HashMap<String, FriendData>,
    pub requests: Vec<FriendRequest>,
}

fn refs(a: Option<&Value>) -> Vec<TrackRef> {
    a.and_then(Value::as_array)
        .map(|a| a.iter().map(TrackRef::from_json).collect())
        .unwrap_or_default()
}

/// Android packLikes / unpackLikes: `gz1:` + base64(gzip(JSON)).
pub fn unpack_likes(content: &str) -> Option<SharedLikes> {
    let text = match content.strip_prefix(LIKES_PACKED_PREFIX) {
        Some(b64) => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
            let mut s = String::new();
            flate2::read::GzDecoder::new(bytes.as_slice()).read_to_string(&mut s).ok()?;
            s
        }
        None => content.to_string(),
    };
    let o: Value = serde_json::from_str(&text).ok()?;
    let prefixes: Vec<String> = o
        .get("p")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let tracks: Vec<TrackRef> = o
        .get("t")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let r = r.as_array()?;
                    let s = |i: usize| r.get(i).and_then(Value::as_str).unwrap_or("").to_string();
                    let ne = |v: String| (!v.is_empty()).then_some(v);
                    let index = r.get(4).and_then(Value::as_i64).unwrap_or(-1);
                    Some(TrackRef {
                        title: s(0),
                        artist: s(1),
                        catalog_id: ne(s(2)),
                        media_id: ne(s(6)),
                        cover: usize::try_from(index).ok().and_then(|i| prefixes.get(i)).map(|p| format!("{p}{}", s(5))),
                        duration_ms: r.get(3).and_then(Value::as_i64).unwrap_or(0) * 1000,
                        at: 0,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(SharedLikes {
        total: o.get("total").and_then(Value::as_i64).unwrap_or(tracks.len() as i64),
        source: o.get("src").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
        tracks,
    })
}

/// Applies one field's content to a friend's data (FriendsHub.onEvent).
pub fn apply_field(data: &mut FriendData, field: &str, content: &str) {
    let json: Option<Value> = (!content.trim().is_empty()).then(|| serde_json::from_str(content).ok()).flatten();
    match field {
        "profile" => {
            if let Some(o) = &json {
                if let Some(n) = o.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) {
                    data.name = Some(n.to_string());
                }
                if let Some(p) = o.get("pic").and_then(Value::as_str).filter(|p| !p.is_empty() && p.len() <= MAX_AVATAR_CHARS) {
                    data.avatar = Some(p.to_string());
                }
            }
        }
        "now" => data.now = json.as_ref().and_then(NowPlaying::from_json),
        "history" => data.history = json.as_ref().map(|o| refs(o.get("tracks"))),
        "playlists" => {
            data.playlists = json.as_ref().map(|o| {
                o.get("playlists")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|p| SharedPlaylist {
                                name: p.get("n").and_then(Value::as_str).unwrap_or("").to_string(),
                                total: p.get("total").and_then(Value::as_i64).unwrap_or(0),
                                tracks: refs(p.get("tracks")),
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
        }
        "stats" => {
            data.stats = json.as_ref().map(|o| Stats {
                week_minutes: o.get("min").and_then(Value::as_i64).unwrap_or(0),
                week_plays: o.get("plays").and_then(Value::as_i64).unwrap_or(0),
                liked: o.get("liked").and_then(Value::as_i64).unwrap_or(0),
                top: o
                    .get("top")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|t| {
                                (
                                    t.get("n").and_then(Value::as_str).unwrap_or("").to_string(),
                                    t.get("c").and_then(Value::as_i64).unwrap_or(0),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        }
        "likes" => data.likes = (!content.trim().is_empty()).then(|| unpack_likes(content)).flatten(),
        "portrait" => data.portrait = (!content.trim().is_empty()).then(|| crate::portrait::unpack(content)).flatten(),
        _ => {}
    }
}

struct Inner {
    keys: Keys,
    pool: Pool,
    privacy: Mutex<Privacy>,
    verified: Mutex<Vec<String>>,
    dismissed: Mutex<Vec<String>>,
    state: watch::Sender<FriendsState>,
    live: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// The friends side of one identity.
#[derive(Clone)]
pub struct Friends {
    inner: Arc<Inner>,
}

/// What the phone keeps in `shared_prefs/friends.xml` (read by the caller).
#[derive(Debug, Clone, Default)]
pub struct Saved {
    pub friends_json: Option<String>,
    pub privacy_json: Option<String>,
    pub verified: Vec<String>,
    pub dismissed: Vec<String>,
    pub faces_json: Option<String>,
}

pub fn parse_friends(json: &str) -> Vec<Friend> {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|o| {
            Some(Friend {
                pubkey: o.get("p")?.as_str()?.to_string(),
                name: o.get("n").and_then(Value::as_str).unwrap_or("").to_string(),
                added_at: o.get("t").and_then(Value::as_i64).unwrap_or(0),
            })
        })
        .collect()
}

impl Friends {
    pub fn new(keys: Keys, pool: Pool, saved: Saved) -> Friends {
        pool.set_signer(Some(keys.clone()));
        let friends = saved.friends_json.as_deref().map(parse_friends).unwrap_or_default();
        let state = FriendsState {
            me: Some(keys.public_hex()),
            friends,
            ..Default::default()
        };
        Friends {
            inner: Arc::new(Inner {
                privacy: Mutex::new(saved.privacy_json.as_deref().map(Privacy::from_json).unwrap_or_default()),
                verified: Mutex::new(saved.verified),
                dismissed: Mutex::new(saved.dismissed),
                state: watch::channel(state).0,
                keys,
                pool,
                live: Mutex::default(),
            }),
        }
    }

    pub fn keys(&self) -> Keys {
        self.inner.keys.clone()
    }

    pub fn pool(&self) -> Pool {
        self.inner.pool.clone()
    }

    pub fn me(&self) -> String {
        self.inner.keys.public_hex()
    }

    pub fn invite_link(&self, name: &str) -> String {
        let n: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_string() } else { format!("%{:02X}", c as u32 & 0xFF) })
            .collect();
        let mut link = format!("{INVITE_BASE}{}", self.inner.keys.npub());
        if !name.is_empty() && name.is_ascii() {
            link.push_str(&format!("?n={n}"));
        }
        link
    }

    pub fn state(&self) -> watch::Receiver<FriendsState> {
        self.inner.state.subscribe()
    }

    pub fn privacy(&self) -> Privacy {
        self.inner.privacy.lock().expect("privacy").clone()
    }

    /// Starts following friends (their fields) and requests addressed to this identity.
    pub fn start_live(&self) {
        let mut live = self.inner.live.lock().expect("live");
        if !live.is_empty() {
            return;
        }
        let me = self.me();
        let st = self.inner.state.borrow().clone();
        let mut authors: Vec<String> = st.friends.iter().map(|f| f.pubkey.clone()).collect();
        authors.push(DEVELOPER_PUBKEY.into());
        authors.push(me.clone());
        authors.dedup();
        let mut d = vec![format!("{D_PREFIX}profile")];
        for f in DATA_FIELDS {
            d.push(format!("{D_PREFIX}{f}"));
            d.push(format!("{D_PREFIX}{f}:{me}"));
        }
        let filter = json!({"kinds": [KIND_APP_DATA], "authors": authors, "#d": d});
        let since = now_s() - 60 * 24 * 3600;
        let to_me = json!({"kinds": [KIND_APP_DATA], "#p": [me], "since": since});
        for f in [filter, to_me] {
            let mut sub = self.inner.pool.subscribe(vec![f]);
            let this = self.clone();
            live.push(tokio::spawn(async move {
                while let Some(m) = sub.next().await {
                    if let SubEvent::Event(e) = m {
                        this.on_event(&e);
                    }
                }
            }));
        }
    }

    pub fn stop_live(&self) {
        for t in self.inner.live.lock().expect("live").drain(..) {
            t.abort();
        }
    }

    fn on_event(&self, e: &Event) {
        let me = self.me();
        let Some(d) = e.tag("d").and_then(|d| d.strip_prefix(D_PREFIX)) else { return };
        let mut parts = d.splitn(2, ':');
        let field = parts.next().unwrap_or("").to_string();
        let personal = parts.next();
        if field == "verified" {
            return;
        }
        if personal.is_some_and(|p| p != me) {
            return;
        }
        let content = if personal.is_some() && !e.content.is_empty() {
            match self.inner.keys.decrypt(&e.pubkey, &e.content) {
                Ok(c) => c,
                Err(_) => return,
            }
        } else {
            e.content.clone()
        };
        let author = e.pubkey.clone();
        if field == "hello" {
            let is_friend = self.inner.state.borrow().friends.iter().any(|f| f.pubkey == author);
            let dismissed = self.inner.dismissed.lock().expect("dismissed").contains(&author);
            if content.is_empty() || is_friend || dismissed || author == me {
                return;
            }
            let name = serde_json::from_str::<Value>(&content)
                .ok()
                .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            self.inner.state.send_if_modified(|s| {
                if s.requests.iter().any(|r| r.pubkey == author) {
                    return false;
                }
                s.requests.push(FriendRequest { pubkey: author.clone(), name, at: e.created_at * 1000 });
                true
            });
            return;
        }
        self.inner.state.send_if_modified(|s| {
            let known = s.friends.iter().any(|f| f.pubkey == author) || author == DEVELOPER_PUBKEY || author == me;
            if !known {
                return false;
            }
            let data = s.data.entry(author.clone()).or_default();
            if data.stamps.get(&field).copied().unwrap_or(0) >= e.created_at {
                return false;
            }
            data.stamps.insert(field.clone(), e.created_at);
            apply_field(data, &field, &content);
            if field == "profile"
                && let Some(n) = data.name.clone()
                && let Some(f) = s.friends.iter_mut().find(|f| f.pubkey == author)
            {
                f.name = n;
            }
            true
        });
    }

    /// Publishes a field to whoever `audience` allows (FriendsHub.publishField).
    async fn publish_field(&self, field: &str, content: &str, audience: Audience) {
        let keys = &self.inner.keys;
        let targets: Vec<String> = match audience {
            Audience::Nobody => return,
            Audience::Everyone => {
                let e = keys.sign(
                    KIND_APP_DATA,
                    vec![vec!["d".into(), format!("{D_PREFIX}{field}")], vec!["t".into(), "drxwnify".into()]],
                    content.to_string(),
                );
                self.inner.pool.publish(&e, Duration::from_secs(10)).await;
                return;
            }
            Audience::Friends => self.inner.state.borrow().friends.iter().map(|f| f.pubkey.clone()).collect(),
            Audience::Verified => self.inner.verified.lock().expect("verified").clone(),
        };
        for friend in targets.into_iter().take(MAX_FRIENDS_ENCRYPTED) {
            let enc = if content.is_empty() {
                String::new()
            } else {
                match keys.encrypt(&friend, content) {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            };
            let e = keys.sign(
                KIND_APP_DATA,
                vec![vec!["d".into(), format!("{D_PREFIX}{field}:{friend}")], vec!["p".into(), friend.clone()]],
                enc,
            );
            let pool = self.inner.pool.clone();
            tokio::spawn(async move {
                pool.publish(&e, Duration::from_secs(10)).await;
            });
        }
    }

    /// The friends list as the phone keeps it (`friends` in friends.xml).
    pub fn friends_json(&self) -> String {
        let st = self.inner.state.borrow();
        Value::Array(
            st.friends
                .iter()
                .map(|f| json!({"p": f.pubkey, "n": f.name, "t": f.added_at}))
                .collect(),
        )
        .to_string()
    }

    /// Requests hidden on purpose (`dismissed` in friends.xml).
    pub fn dismissed(&self) -> Vec<String> {
        self.inner.dismissed.lock().expect("dismissed").clone()
    }

    /// Adds a friend (a code, an invite or an accepted request) and says hello to them (FriendsHub.addFriend):
    /// they see this profile as a request, or as a friend if they added it already.
    pub async fn add_friend(&self, pubkey: &str, name: &str, version: &str) -> bool {
        let pubkey = pubkey.to_lowercase();
        if pubkey == self.me() || pubkey.len() != 64 {
            return false;
        }
        let added = self.inner.state.send_if_modified(|s| {
            s.requests.retain(|r| r.pubkey != pubkey);
            if s.friends.iter().any(|f| f.pubkey == pubkey) {
                return true;
            }
            s.friends.push(Friend {
                pubkey: pubkey.clone(),
                name: name.chars().take(40).collect(),
                added_at: now_s() * 1000,
            });
            true
        });
        let my_name = self
            .inner
            .state
            .borrow()
            .data
            .get(&self.me())
            .and_then(|d| d.name.clone())
            .unwrap_or_default();
        let hello = json!({"name": my_name, "chat": 2, "ver": version.chars().take(16).collect::<String>()}).to_string();
        if let Ok(enc) = self.inner.keys.encrypt(&pubkey, &hello) {
            let e = self.inner.keys.sign(
                KIND_APP_DATA,
                vec![vec!["d".into(), format!("{D_PREFIX}hello:{pubkey}")], vec!["p".into(), pubkey.clone()]],
                enc,
            );
            self.inner.pool.publish(&e, Duration::from_secs(10)).await;
        }
        // Follow the new friend too.
        if !self.inner.live.lock().expect("live").is_empty() {
            self.stop_live();
            self.start_live();
        }
        added
    }

    /// Removes a friend (FriendsHub.removeFriend): off the list, every personal field sent to them (and the hello)
    /// replaced by an empty one so their app forgets this profile's data, and the live view stops following them.
    pub async fn remove_friend(&self, pubkey: &str) -> bool {
        let removed = self.inner.state.send_if_modified(|s| {
            let n = s.friends.len();
            s.friends.retain(|f| f.pubkey != pubkey);
            if n == s.friends.len() {
                return false;
            }
            s.data.remove(pubkey);
            true
        });
        if !removed {
            return false;
        }
        self.inner.verified.lock().expect("verified").retain(|v| v != pubkey);
        for field in DATA_FIELDS.iter().chain(std::iter::once(&"hello")) {
            let e = self.inner.keys.sign(
                KIND_APP_DATA,
                vec![vec!["d".into(), format!("{D_PREFIX}{field}:{pubkey}")], vec!["p".into(), pubkey.to_string()]],
                String::new(),
            );
            let pool = self.inner.pool.clone();
            tokio::spawn(async move {
                pool.publish(&e, Duration::from_secs(10)).await;
            });
        }
        if !self.inner.live.lock().expect("live").is_empty() {
            self.stop_live();
            self.start_live();
        }
        true
    }

    /// Hides a request; it does not come back (FriendsHub.dismissRequest).
    pub fn dismiss(&self, pubkey: &str) {
        {
            let mut d = self.inner.dismissed.lock().expect("dismissed");
            if !d.iter().any(|x| x == pubkey) {
                d.push(pubkey.to_string());
            }
        }
        self.inner.state.send_if_modified(|s| {
            let n = s.requests.len();
            s.requests.retain(|r| r.pubkey != pubkey);
            n != s.requests.len()
        });
    }

    /// What plays now (already debounced by the caller), to whoever may see it.
    pub async fn publish_now(&self, mut now: NowPlaying) {
        let privacy = self.privacy();
        now.listen_along = privacy.listen_along;
        self.publish_field("now", &now.to_json().to_string(), privacy.now).await;
    }

    /// The listening history (newest first, at most 50).
    pub async fn publish_history(&self, tracks: Vec<TrackRef>) {
        let audience = self.privacy().history;
        let list: Vec<Value> = tracks.iter().take(MAX_HISTORY).map(TrackRef::to_json).collect();
        self.publish_field("history", &json!({"tracks": list}).to_string(), audience).await;
    }

    /// Kind 0 metadata only when the profile is findable (FriendsHub.publishProfile does both).
    pub async fn publish_metadata(&self, name: &str) {
        let content = if self.privacy().discoverable {
            json!({"name": name, "about": "Drxwnify"}).to_string()
        } else {
            "{}".into()
        };
        let e = self.inner.keys.sign(KIND_METADATA, Vec::new(), content);
        self.inner.pool.publish(&e, Duration::from_secs(10)).await;
    }
}

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_and_friends_parse() {
        let p = Privacy::from_json(r#"{"now":"EVERYONE","history":"NOBODY","playlists":"VERIFIED","stats":"FRIENDS","discoverable":true,"along":false,"read":true}"#);
        assert_eq!((p.now, p.history, p.playlists, p.stats), (Audience::Everyone, Audience::Nobody, Audience::Verified, Audience::Friends));
        assert!(p.discoverable && !p.listen_along);
        let f = parse_friends(r#"[{"p":"ab","n":"Name","t":5}]"#);
        assert_eq!(f, vec![Friend { pubkey: "ab".into(), name: "Name".into(), added_at: 5 }]);
    }

    #[test]
    fn now_round_trip_and_liveness() {
        let n = NowPlaying {
            track: TrackRef { title: "T".into(), artist: "A".into(), catalog_id: Some("c".into()), duration_ms: 200_000, ..Default::default() },
            position_ms: 10_000,
            playing: true,
            at: 1_000_000,
            listen_along: true,
        };
        let back = NowPlaying::from_json(&n.to_json()).unwrap();
        assert_eq!(back, n);
        assert!(n.is_live(1_000_000 + 190_000));
        assert!(!n.is_live(1_000_000 + 190_000 + LIVE_SLACK_MS + 1));
        assert_eq!(n.position_now(1_005_000), 15_000);
    }

    #[test]
    fn likes_unpack_from_gzip() {
        let json = r#"{"v":1,"total":2,"src":"spotify","p":["https://i.scdn.co/image/"],"t":[["T","A","id1",200,0,"abc",""]]}"#;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, json.as_bytes()).unwrap();
        let packed = format!("gz1:{}", base64::engine::general_purpose::STANDARD.encode(gz.finish().unwrap()));
        let l = unpack_likes(&packed).unwrap();
        assert_eq!(l.total, 2);
        assert_eq!(l.tracks[0].cover.as_deref(), Some("https://i.scdn.co/image/abc"));
        assert_eq!(l.tracks[0].duration_ms, 200_000);
        assert_eq!(l.tracks[0].media_id, None);
    }
}
