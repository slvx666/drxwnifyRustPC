//! Friends in the app: the phone's identity (its key comes with the backup into the vault) and friends list,
//! what plays published to them (Android FriendsHub.playerChanged: debounced 1.5 s) and the history two minutes
//! after a listen; friends' tracks turned into playable ones.
//!
//! A test run (`DRX_PROFILE`) is the same identity as the owner: it publishes nothing unless DRX_SOCIAL_PUBLISH is
//! set, so checks never show friends what a test played.

use crate::songs::{catalog_id_fast, song_id_of};
use crate::App;
use drx_catalog::model::{AlbumRef, ArtistRef, Image, Track};
use drx_social::friends::{Friends, FriendsState, NowPlaying, Saved, TrackRef};
use std::time::Duration;

const NOW_DEBOUNCE: Duration = Duration::from_millis(1500);
const HISTORY_DELAY: Duration = Duration::from_secs(120);

/// For other modules (chats): publishing is allowed in this run.
pub(crate) fn may_publish_pub() -> bool {
    may_publish()
}

fn may_publish() -> bool {
    std::env::var_os("DRX_PROFILE").is_none() || std::env::var_os("DRX_SOCIAL_PUBLISH").is_some()
}

/// The ref friends get for a track.
pub fn ref_of(t: &Track) -> TrackRef {
    let song = song_id_of(&t.id);
    TrackRef {
        title: t.title.chars().take(120).collect(),
        artist: t.artist_names().chars().take(120).collect(),
        catalog_id: catalog_id_fast(&song).filter(|c| !c.starts_with("src:")),
        media_id: Some(song).filter(|s| !s.starts_with("local:")),
        cover: t
            .album
            .as_ref()
            .and_then(|a| drx_catalog::model::best_image(&a.images, 300))
            .map(|i| i.url.clone())
            .filter(|u| u.starts_with("http")),
        duration_ms: t.duration_ms as i64,
        at: 0,
    }
}

/// A friend's track as one this app can play: its catalog id (else its media id), title, artist and cover.
pub fn track_of(r: &TrackRef) -> Track {
    let id = r
        .catalog_id
        .clone()
        .or_else(|| r.media_id.clone())
        .unwrap_or_else(|| format!("friend:{}|{}", r.artist, r.title));
    Track {
        id,
        title: r.title.clone(),
        artists: r
            .artist
            .split(", ")
            .filter(|a| !a.is_empty())
            .map(|a| ArtistRef { id: String::new(), name: a.to_string() })
            .collect(),
        album: r.cover.as_ref().map(|c| AlbumRef {
            id: String::new(),
            name: String::new(),
            images: vec![Image { url: c.clone(), width: Some(300) }],
        }),
        duration_ms: r.duration_ms.max(0) as u64,
        isrc: None,
        explicit: false,
        plays: None,
    }
}

impl App {
    /// The friends side, when the profile has a friends key (from the phone's backup).
    pub fn friends(&self) -> Option<Friends> {
        self.inner
            .friends
            .get_or_init(|| {
                let vault = drx_core::secrets::Vault::load(&self.inner.profile).ok()?;
                let sk = vault.shared(drx_core::keys::sp::FRIENDS, drx_core::keys::sp::FRIENDS_SECRET_KEY)?;
                let keys = drx_social::nostr::Keys::from_hex(sk).ok()?;
                let sp = std::fs::read_to_string(self.inner.profile.shared_prefs(drx_core::keys::sp::FRIENDS))
                    .ok()
                    .and_then(|x| drx_core::shared_prefs::SharedPrefs::parse(&x).ok())
                    .unwrap_or_default();
                let set = |k: &str| match sp.values.get(k) {
                    Some(drx_core::shared_prefs::SpValue::Set(v)) => v.clone(),
                    _ => Vec::new(),
                };
                let saved = Saved {
                    friends_json: sp.get_str("friends").map(str::to_string),
                    privacy_json: sp.get_str("privacy").map(str::to_string),
                    verified: set("verifiedSet"),
                    dismissed: set("dismissed"),
                    faces_json: sp.get_str("faces").map(str::to_string),
                };
                let pool = drx_social::relay::Pool::new(self.runtime.handle());
                Some(Friends::new(keys, pool, saved))
            })
            .clone()
    }

    pub fn friends_state(&self) -> Option<tokio::sync::watch::Receiver<FriendsState>> {
        self.friends().map(|f| f.state())
    }

    /// Opens the live view of friends (their fields, requests).
    pub fn start_friends_live(&self) {
        if let Some(f) = self.friends() {
            self.runtime.spawn(async move { f.start_live() });
        }
    }

    /// What plays → friends, debounced (track change, play/pause, seek all send a new word).
    pub(crate) fn start_social_publishing(&self) {
        if !may_publish() {
            return;
        }
        let app = self.clone();
        self.runtime.spawn(async move {
            let Some(friends) = app.friends() else { return };
            let mut rx = app.inner.playback.now.subscribe();
            let mut last: Option<(String, bool)> = None;
            loop {
                if rx.changed().await.is_err() {
                    return;
                }
                // Debounce: wait until nothing changed for 1.5 s.
                loop {
                    match tokio::time::timeout(NOW_DEBOUNCE, rx.changed()).await {
                        Ok(Ok(())) => continue,
                        Ok(Err(_)) => return,
                        Err(_) => break,
                    }
                }
                let now = rx.borrow().clone();
                let Some(t) = now.track else { continue };
                let key = (t.id.clone(), now.playing);
                if last.as_ref() == Some(&key) {
                    continue;
                }
                last = Some(key);
                let clock = app.inner.playback.clock();
                friends
                    .publish_now(NowPlaying {
                        track: ref_of(&t),
                        position_ms: (clock.position_s * 1000.0) as i64,
                        playing: now.playing,
                        at: crate::songs::now_ms(),
                        listen_along: true,
                    })
                    .await;
            }
        });
    }

    /// A listen was saved: the history goes to friends two minutes later (one send for a run of listens).
    pub(crate) fn history_changed(&self) {
        if !may_publish() {
            return;
        }
        let Some(friends) = self.friends() else { return };
        let mut pending = self.inner.history_publish.lock().expect("history publish");
        if let Some(t) = pending.take() {
            t.abort();
        }
        let app = self.clone();
        *pending = Some(self.runtime.spawn(async move {
            tokio::time::sleep(HISTORY_DELAY).await;
            let rows = app.history(0, 200).await.unwrap_or_default();
            let mut seen = std::collections::HashSet::new();
            let refs: Vec<TrackRef> = rows
                .into_iter()
                .filter(|e| seen.insert(e.song.id.clone()))
                .take(drx_social::friends::MAX_HISTORY)
                .map(|e| {
                    let mut r = ref_of(&crate::playable_of(e.song).track);
                    r.at = e.at_ms;
                    r
                })
                .collect();
            friends.publish_history(refs).await;
        }));
    }
}

impl App {
    fn save_friends(&self, f: &Friends) {
        let (list, dismissed) = (f.friends_json(), f.dismissed());
        self.edit_shared_prefs(drx_core::keys::sp::FRIENDS, move |p| {
            p.values.insert("friends".into(), drx_core::shared_prefs::SpValue::String(list));
            p.values.insert("dismissed".into(), drx_core::shared_prefs::SpValue::Set(dismissed));
        });
    }

    /// Adds a friend by code (npub / hex), invite link or accepted request. Err = not a profile code.
    pub async fn add_friend(&self, code: &str, name: &str) -> crate::AppResult<()> {
        let f = self.friends().ok_or_else(|| crate::AppError::Other("нет профиля друзей".into()))?;
        let text = code.trim();
        let key = text.rsplit('/').next().unwrap_or(text);
        let key = key.split('?').next().unwrap_or(key);
        let pubkey = drx_social::nostr::pubkey_from_text(key)
            .ok_or_else(|| crate::AppError::Other("это не код профиля".into()))?;
        if !may_publish() {
            return Err(crate::AppError::Other("в тестовом запуске друзей не добавляю".into()));
        }
        f.add_friend(&pubkey, name, drx_core::APP_VERSION).await;
        self.save_friends(&f);
        Ok(())
    }

    /// Removes a friend (their data cleared on the relays too). A test run never does it.
    pub async fn remove_friend(&self, pubkey: &str) -> crate::AppResult<()> {
        let f = self.friends().ok_or_else(|| crate::AppError::Other("нет профиля друзей".into()))?;
        if !may_publish() {
            return Err(crate::AppError::Other("в тестовом запуске друзей не удаляю".into()));
        }
        if self.listening_along().as_deref() == Some(pubkey) {
            self.stop_listen_along();
        }
        f.remove_friend(pubkey).await;
        self.save_friends(&f);
        Ok(())
    }

    pub fn dismiss_request(&self, pubkey: &str) {
        if let Some(f) = self.friends() {
            f.dismiss(pubkey);
            self.save_friends(&f);
        }
    }
}
