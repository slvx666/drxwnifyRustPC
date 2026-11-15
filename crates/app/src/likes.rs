//! Likes, saved albums and followed artists, mirrored to the linked accounts the way Android 2.3.0 does it
//! (`MusicService.setTrackLiked`, `LikeTarget`, `CrossCatalog`, `MainAccount.LikeMode`, `VkMusic.setTrackSaved`):
//!
//! - a track's heart is set in the app at once (its song row is written when it has none), then sent to the
//!   accounts the "where likes go" setting names: the main one only, the main one and the track's own catalog
//!   (default), or every linked one. VK takes likes only while "likes to VK" is on;
//! - a track from another catalog goes on the same track in each account's catalog, found by title, artist and
//!   length (±8 s), the answer remembered in `cross_catalog` (shared with the phone through the backup); what the
//!   main account's catalog lacks is kept in the app and listed under `missing:<ACCOUNT>`;
//! - an unlike clears every row of the same catalog track (its `mfb:` row and its YouTube video);
//! - a VK like is a copy in «Моя музыка»; its address is kept in `vk_saved_tracks` to remove it again;
//! - albums are saved and artists followed on the account of their own catalog; a follow also puts the artist in
//!   the app's library (`bookmarkedAt`).

use crate::songs::{catalog_id_fast, catalog_id_of, ensure_song, now_ms, song_id_of};
use crate::{App, AppError, AppResult};
use drx_catalog::model::{Source, Track};
use drx_core::shared_prefs::{SharedPrefs, SpValue};
use rusqlite::params;
use std::collections::HashSet;

/// Settings (MainAccount.LikeModeKey, VkLikesToAccountKey).
pub const LIKE_MODE_KEY: &str = "likeMode";
pub const VK_LIKES_KEY: &str = "vkLikesToAccount";

const CROSS: &str = "cross_catalog";
const VK_SAVED: &str = "vk_saved_tracks";
/// "Not in that catalog", remembered as such.
const NONE: &str = "";

/// Where a heart goes (MainAccount.LikeMode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LikeMode {
    MainOnly,
    #[default]
    MainAndSource,
    All,
}

impl LikeMode {
    pub fn key(self) -> &'static str {
        match self {
            LikeMode::MainOnly => "MAIN_ONLY",
            LikeMode::MainAndSource => "MAIN_AND_SOURCE",
            LikeMode::All => "ALL",
        }
    }
    pub fn from_key(k: &str) -> LikeMode {
        match k {
            "MAIN_ONLY" => LikeMode::MainOnly,
            "ALL" => LikeMode::All,
            _ => LikeMode::MainAndSource,
        }
    }
}

/// CrossCatalog.normalize: lower case, letters folded, accents dropped, "(feat. …)" / "- Remastered" notes cut,
/// then letters and digits only. A remix stays a different track.
pub fn cross_norm(text: &str) -> String {
    const NOTES: [&str; 6] = ["feat", "ft.", "with", "remaster", "version", "edit"];
    let lower = text.to_lowercase();
    // Bracketed notes that name a credit or an edition.
    let mut s = String::with_capacity(lower.len());
    let mut rest = lower.as_str();
    while let Some(open) = rest.find(['(', '[']) {
        let close_ch = if rest[open..].starts_with('(') { ')' } else { ']' };
        let Some(len) = rest[open..].find(close_ch) else {
            break;
        };
        let inner = &rest[open + 1..open + len];
        s.push_str(&rest[..open]);
        if !NOTES.iter().any(|n| inner.contains(n)) {
            s.push_str(&rest[open..=open + len]);
        }
        s.push(' ');
        rest = &rest[open + len + 1..];
    }
    s.push_str(rest);
    // " - 2011 Remaster", " - Radio Edit".
    if let Some(dash) = s.find(" - ") {
        let tail = &s[dash..];
        if ["remaster", "version", "edit"].iter().any(|n| tail.contains(n)) {
            s.truncate(dash);
        }
    }
    drx_resolver::matcher::fold_letters(&s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// What the app knows of a track when looking for it in another catalog.
#[derive(Debug, Clone)]
struct Wanted {
    title: String,
    artists: Vec<String>,
    duration_s: i64,
    album: Option<String>,
}

/// The best of `found` for `w` (CrossCatalog.findTrack.bestOf): exact title 3, one title the start of the other 2;
/// an artist must match (one name inside the other) unless `check_artist` is off; length within 8 s.
fn best_of(found: &[Track], w: &Wanted, check_artist: bool) -> Option<String> {
    let title = cross_norm(&w.title);
    let artists: Vec<String> = w
        .artists
        .iter()
        .map(|a| cross_norm(a))
        .filter(|a| !a.is_empty())
        .collect();
    found
        .iter()
        .filter(|t| !t.id.is_empty())
        .filter_map(|t| {
            let n = cross_norm(&t.title);
            let score = if n == title {
                3
            } else if !n.is_empty() && (n.starts_with(&title) || title.starts_with(&n)) {
                2
            } else {
                0
            };
            let names: Vec<String> = t.artists.iter().map(|a| cross_norm(&a.name)).collect();
            let artist_ok = !check_artist
                || artists.is_empty()
                || names.iter().any(|n| {
                    artists
                        .iter()
                        .any(|a| a == n || a.contains(n.as_str()) || n.contains(a.as_str()))
                });
            let len_ok = w.duration_s <= 0
                || t.duration_ms == 0
                || ((t.duration_ms / 1000) as i64 - w.duration_s).abs() <= 8;
            (score > 0 && artist_ok && len_ok).then(|| (score, t.id.clone()))
        })
        .max_by_key(|(s, _)| *s)
        .map(|(_, id)| id)
}

fn err(e: impl std::fmt::Display) -> AppError {
    AppError::Other(e.to_string())
}

impl App {
    fn shared(&self, name: &str) -> SharedPrefs {
        std::fs::read_to_string(self.inner.profile.shared_prefs(name))
            .ok()
            .and_then(|x| SharedPrefs::parse(&x).ok())
            .unwrap_or_default()
    }

    fn edit_shared(&self, name: &str, f: impl FnOnce(&mut SharedPrefs)) {
        let _guard = self.inner.shared_lock.lock().expect("shared prefs lock");
        let mut p = self.shared(name);
        f(&mut p);
        let _ = drx_core::paths::write_atomic(
            &self.inner.profile.shared_prefs(name),
            p.to_xml().as_bytes(),
        );
    }

    /// Keeps tracks the UI was handed, so a heart on any of their rows knows the whole track.
    pub(crate) fn remember<'a>(&self, tracks: impl Iterator<Item = &'a Track>) {
        let mut k = self.inner.known.lock().expect("known");
        if k.len() > 20_000 {
            k.clear();
        }
        for t in tracks {
            k.entry(t.id.clone()).or_insert_with(|| t.clone());
        }
    }

    /// A track by the id a row or the player shows: one the UI was handed, else its library row.
    pub async fn known_track(&self, id: &str) -> Option<Track> {
        if let Some(t) = self.inner.known.lock().expect("known").get(id).cloned() {
            return Some(t);
        }
        if let Some(t) = self.inner.playback.now.borrow().track.clone().filter(|t| t.id == id) {
            return Some(t);
        }
        let song = song_id_of(id);
        self.store()
            .call(move |c| {
                let row = c.query_row(
                    "SELECT title, duration, albumId, albumName, thumbnailUrl, explicit, isrc FROM song WHERE id = ?1",
                    [&song],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, bool>(5)?,
                            r.get::<_, Option<String>>(6)?,
                        ))
                    },
                );
                let Ok((title, dur, album_id, album_name, thumb, explicit, isrc)) = row else {
                    return Ok(None);
                };
                let mut st = c.prepare(
                    "SELECT a.id, a.name FROM song_artist_map m JOIN artist a ON a.id = m.artistId
                     WHERE m.songId = ?1 ORDER BY m.position",
                )?;
                let artists = st
                    .query_map([&song], |r| {
                        Ok(drx_catalog::model::ArtistRef { id: r.get(0)?, name: r.get(1)? })
                    })?
                    .flatten()
                    .collect();
                Ok(Some(Track {
                    id: song.clone(),
                    title,
                    artists,
                    album: album_name.map(|name| drx_catalog::model::AlbumRef {
                        id: album_id.unwrap_or_default(),
                        name,
                        images: thumb
                            .map(|url| vec![drx_catalog::model::Image { url, width: Some(300) }])
                            .unwrap_or_default(),
                    }),
                    duration_ms: dur.max(0) as u64 * 1000,
                    isrc,
                    explicit,
                    plays: None,
                }))
            })
            .await
            .ok()
            .flatten()
    }

    /// Flips the heart of the track a row or the player shows; returns the new state.
    pub async fn toggle_like(&self, id: &str) -> AppResult<bool> {
        let t = self.known_track(id).await.ok_or_else(|| err("unknown track"))?;
        let liked = !self.is_track_liked(id).await;
        self.set_track_liked(&t, liked).await.map(|_| liked)
    }

    pub fn like_mode(&self) -> LikeMode {
        self.prefs_now()
            .get_str(LIKE_MODE_KEY)
            .map(LikeMode::from_key)
            .unwrap_or_default()
    }

    pub fn set_like_mode(&self, m: LikeMode) -> AppResult<()> {
        self.set_pref(
            LIKE_MODE_KEY,
            drx_core::prefs::PrefValue::String(m.key().to_string()),
        )
    }

    /// VK likes go to «Моя музыка» (default on).
    pub fn vk_likes_to_account(&self) -> bool {
        self.prefs_now().get_bool(VK_LIKES_KEY).unwrap_or(true)
    }

    /// The song rows that are the same catalog track as `song_id` (its `mfb:` row, older `spotify:` row and YouTube
    /// videos matched to it), `song_id` first.
    fn same_track_rows(c: &rusqlite::Connection, song_id: &str, catalog: Option<&str>) -> Vec<String> {
        let mut rows = vec![song_id.to_string()];
        if let Some(cid) = catalog {
            rows.push(format!("mfb:{cid}"));
            rows.push(format!("spotify:{cid}"));
            if let Ok(mut st) = c.prepare("SELECT youtubeId FROM spotify_match WHERE spotifyId = ?1") {
                if let Ok(it) = st.query_map([cid], |r| r.get::<_, String>(0)) {
                    rows.extend(it.flatten());
                }
            }
        }
        let mut seen = HashSet::new();
        rows.retain(|r| seen.insert(r.clone()));
        rows
    }

    /// Is the track liked in the app (any of its rows)?
    pub async fn is_track_liked(&self, track_id: &str) -> bool {
        let song = song_id_of(track_id);
        self.store()
            .call(move |c| {
                let cid = catalog_id_of(c, &song);
                let rows = Self::same_track_rows(c, &song, cid.as_deref());
                let mut st = c.prepare("SELECT liked FROM song WHERE id = ?1")?;
                Ok(rows.iter().any(|r| {
                    st.query_row([r], |x| x.get::<_, bool>(0)).unwrap_or(false)
                }))
            })
            .await
            .unwrap_or(false)
    }

    /// The liked flags of many tracks at once (rows of a page), in order.
    pub async fn liked_flags(&self, track_ids: Vec<String>) -> Vec<bool> {
        let n = track_ids.len();
        self.store()
            .call(move |c| {
                let mut st = c.prepare("SELECT liked FROM song WHERE id = ?1")?;
                Ok(track_ids
                    .iter()
                    .map(|t| {
                        let song = song_id_of(t);
                        let cid = catalog_id_of(c, &song);
                        Self::same_track_rows(c, &song, cid.as_deref())
                            .iter()
                            .any(|r| st.query_row([r], |x| x.get::<_, bool>(0)).unwrap_or(false))
                    })
                    .collect())
            })
            .await
            .unwrap_or_else(|_| vec![false; n])
    }

    /// Likes or unlikes a track everywhere it belongs (see the module notes). The app's heart changes at once; the
    /// accounts follow. Errors of the accounts are returned after the app is updated (the heart stays as set).
    pub async fn set_track_liked(&self, t: &Track, liked: bool) -> AppResult<()> {
        let t2 = t.clone();
        let (song, catalog) = self
            .store()
            .call(move |c| {
                let song = ensure_song(c, &t2)?;
                let cid = catalog_id_of(c, &song);
                let now = now_ms();
                if liked {
                    c.execute(
                        "UPDATE song SET liked = 1, likedDate = ?2, inLibrary = coalesce(inLibrary, ?2) WHERE id = ?1",
                        params![song, now],
                    )?;
                } else {
                    for r in Self::same_track_rows(c, &song, cid.as_deref()) {
                        c.execute(
                            "UPDATE song SET liked = 0, likedDate = NULL WHERE id = ?1",
                            [&r],
                        )?;
                    }
                }
                Ok((song, cid))
            })
            .await?;
        // What the accounts know it by: its catalog track (a VK track found by the source search is a VK track too).
        let Some(like_id) = catalog.or_else(|| catalog_id_fast(&song)) else {
            return Ok(());
        };
        let origin = Source::of_id(&like_id);
        if origin == Source::Vk && !like_id.starts_with("src:VK:") {
            return Ok(()); // a track from another source (SoundCloud, Soulseek…): in the app only
        }
        let main = self.main_account();
        let mut targets: Vec<Source> = match self.like_mode() {
            LikeMode::MainOnly => main.into_iter().collect(),
            LikeMode::MainAndSource => main
                .into_iter()
                .chain(self.has_account(origin).then_some(origin))
                .collect(),
            LikeMode::All => [Source::Spotify, Source::Yandex, Source::Vk]
                .into_iter()
                .filter(|s| self.has_account(*s))
                .collect(),
        };
        targets.dedup();
        targets.retain(|s| *s != Source::Vk || self.vk_likes_to_account());
        let wanted = Wanted {
            title: t.title.clone(),
            artists: t.artists.iter().map(|a| a.name.clone()).collect(),
            duration_s: (t.duration_ms / 1000) as i64,
            album: t.album.as_ref().map(|a| a.name.clone()),
        };
        let mut first_err = None;
        for target in targets {
            let is_main = Some(target) == main;
            let res = async {
                let Some(id) = self
                    .track_for(target, &like_id, &wanted, liked, &song, is_main)
                    .await
                else {
                    return Ok(());
                };
                self.write_track(target, &id, liked).await
            }
            .await;
            if let Err(e) = res {
                tracing::warn!("like of {song} to {target:?}: {e}");
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The id to like `id` with in `target` (CrossCatalog.trackFor): itself in its own catalog, else the same track
    /// there (remembered), else None. The main account's misses are listed for the library.
    async fn track_for(
        &self,
        target: Source,
        id: &str,
        w: &Wanted,
        liked: bool,
        song: &str,
        is_main: bool,
    ) -> Option<String> {
        if Source::of_id(id) == target {
            return Some(id.to_string());
        }
        let key = format!("t:{}:{id}", crate::accounts::source_name(target));
        let known = self.shared(CROSS).get_str(&key).map(str::to_string);
        let found = match known {
            Some(k) => (k != NONE).then_some(k),
            None => {
                // Not answered (no connection): neither remembered nor marked, asked again next time.
                let f = self.find_track(target, w).await.ok()?;
                let v = f.clone().unwrap_or_default();
                self.edit_shared(CROSS, |p| {
                    p.values.insert(key, SpValue::String(v));
                });
                f
            }
        };
        if is_main {
            let missing = liked && found.is_none();
            let set_key = format!("missing:{}", crate::accounts::source_name(target));
            self.edit_shared(CROSS, |p| {
                let mut set: Vec<String> = match p.values.get(&set_key) {
                    Some(SpValue::Set(v)) => v.clone(),
                    _ => Vec::new(),
                };
                for k in [format!("t:{id}"), format!("s:{song}")] {
                    set.retain(|x| x != &k);
                    if missing {
                        set.push(k);
                    }
                }
                p.values.insert(set_key, SpValue::Set(set));
            });
        }
        found
    }

    async fn search_in(&self, target: Source, q: &str) -> Result<Vec<Track>, AppError> {
        match target {
            Source::Spotify => self.spotify().ok_or_else(|| err("no Spotify"))?.search_tracks(q, 20).await.map_err(err),
            Source::Yandex => self.yandex().ok_or_else(|| err("no Yandex"))?.search(q, 20).await.map_err(err),
            Source::Vk => Ok(self
                .vk()
                .ok_or_else(|| err("no VK"))?
                .search(q, 20)
                .await
                .map_err(err)?
                .into_iter()
                .map(|a| a.track)
                .collect()),
        }
    }

    /// CrossCatalog.findTrack: title and artist searches, then the album's track list. Ok(None) = the catalog
    /// answered and hasn't it; Err = it didn't answer.
    async fn find_track(&self, target: Source, w: &Wanted) -> AppResult<Option<String>> {
        if cross_norm(&w.title).is_empty() {
            return Ok(None);
        }
        let artist = w.artists.first().cloned();
        let mut queries = vec![match &artist {
            Some(a) => format!("{} {a}", w.title),
            None => w.title.clone(),
        }];
        if artist.is_some() {
            queries.push(w.title.clone());
        }
        if let Some(a) = &artist {
            queries.push(format!("{a} {}", w.title));
        }
        queries.dedup();
        let mut answered = false;
        for q in &queries {
            let Ok(found) = self.search_in(target, q).await else {
                continue;
            };
            answered = true;
            if let Some(id) = best_of(&found, w, true) {
                return Ok(Some(id));
            }
        }
        // Some tracks never come up in a track search while their album does.
        if let Some(album) = w.album.as_deref().filter(|a| !a.is_empty()) {
            let wanted_album = cross_norm(album);
            let wanted_artists: Vec<String> = w.artists.iter().map(|a| cross_norm(a)).collect();
            let qs: Vec<String> = artist
                .iter()
                .map(|a| format!("{a} {album}"))
                .chain(std::iter::once(album.to_string()))
                .collect();
            for q in qs {
                let albums = match target {
                    Source::Spotify => match self.spotify() {
                        Some(sp) => sp.search_all(&q, 10).await.map(|r| r.albums).ok(),
                        None => None,
                    },
                    Source::Yandex => match self.yandex() {
                        Some(y) => y.search_all(&q).await.map(|r| r.albums).ok(),
                        None => None,
                    },
                    Source::Vk => None,
                };
                let Some(albums) = albums else { continue };
                answered = true;
                let Some(m) = albums.into_iter().find(|a| {
                    cross_norm(&a.name) == wanted_album
                        && (wanted_artists.is_empty()
                            || a.artists.iter().any(|x| wanted_artists.contains(&cross_norm(&x.name))))
                }) else {
                    continue;
                };
                let tracks = match target {
                    Source::Spotify => match self.spotify() {
                        Some(sp) => sp.album(&m.id).await.map(|a| a.tracks).ok(),
                        None => None,
                    },
                    Source::Yandex => match self.yandex() {
                        Some(y) => y.album(&m.id).await.map(|a| a.tracks).ok(),
                        None => None,
                    },
                    Source::Vk => None,
                };
                if let Some(id) = tracks.and_then(|t| best_of(&t, w, false)) {
                    return Ok(Some(id));
                }
                break;
            }
        }
        if answered {
            tracing::info!("\"{}\" not in {target:?}", w.title);
            Ok(None)
        } else {
            Err(err(format!("{target:?} didn't answer")))
        }
    }

    /// One like or unlike on one account.
    async fn write_track(&self, target: Source, id: &str, liked: bool) -> AppResult<()> {
        match target {
            Source::Spotify => {
                let sp = self.spotify().ok_or_else(|| err("no Spotify"))?;
                sp.set_in_library(&[format!("spotify:track:{id}")], liked)
                    .await
                    .map_err(err)
            }
            Source::Yandex => {
                let y = self.yandex().ok_or_else(|| err("no Yandex"))?;
                y.set_liked("track", id, liked).await.map_err(err)
            }
            Source::Vk => self.vk_set_saved(id, liked).await,
        }
    }

    /// VkMusic.setTrackSaved: a like adds a copy to «Моя музыка» (not for the user's own tracks), an unlike removes
    /// that copy (or the user's own track).
    async fn vk_set_saved(&self, id: &str, saved: bool) -> AppResult<()> {
        let vk = self.vk().ok_or_else(|| err("no VK"))?;
        let full = id.strip_prefix("src:VK:").unwrap_or(id).to_string();
        let me = vk.my_id().await.map_err(err)?;
        let owner: Option<i64> = full.split('_').next().and_then(|o| o.parse().ok());
        let copy = self.shared(VK_SAVED).get_str(&full).map(str::to_string);
        if saved {
            if copy.is_some() || owner == Some(me) {
                return Ok(());
            }
            let (o, c) = vk.add_track(&full).await.map_err(err)?;
            if c != 0 {
                self.edit_shared(VK_SAVED, |p| {
                    p.values.insert(full.clone(), SpValue::String(format!("{o}_{c}")));
                });
            }
        } else if let Some(copy) = copy {
            let (o, c) = copy.rsplit_once('_').unwrap_or(("", ""));
            if let (Ok(o), Ok(c)) = (o.parse(), c.parse()) {
                vk.delete_track(o, c).await.map_err(err)?;
            }
            self.edit_shared(VK_SAVED, |p| {
                p.values.remove(&full);
            });
        } else if owner == Some(me)
            && let Some(audio) = full.split('_').nth(1).and_then(|a| a.parse().ok())
        {
            vk.delete_track(me, audio).await.map_err(err)?;
        }
        Ok(())
    }

    /// Is the album saved on the account of its catalog?
    pub async fn is_album_saved(&self, album_id: &str) -> AppResult<bool> {
        match Source::of_id(album_id) {
            Source::Spotify => {
                let sp = self.spotify().ok_or_else(|| err("no Spotify"))?;
                Ok(sp.library_ids("Albums").await.map_err(err)?.contains(album_id))
            }
            Source::Yandex => {
                let y = self.yandex().ok_or_else(|| err("no Yandex"))?;
                Ok(y.liked_ids("albums").await.map_err(err)?.contains(album_id))
            }
            Source::Vk => {
                let vk = self.vk().ok_or_else(|| err("no VK"))?;
                let Some((o, p, _)) = drx_catalog::vk::parse_playlist_id(album_id) else {
                    return Ok(false);
                };
                Ok(vk
                    .my_playlists()
                    .await
                    .map_err(err)?
                    .iter()
                    .any(|x| x.owner_id == o && x.playlist_id == p))
            }
        }
    }

    /// Saves an album on (or removes it from) the account of its catalog (Catalog.saveAlbum / removeAlbum).
    pub async fn set_album_saved(&self, album_id: &str, saved: bool) -> AppResult<()> {
        match Source::of_id(album_id) {
            Source::Spotify => {
                let sp = self.spotify().ok_or_else(|| err("no Spotify"))?;
                sp.set_in_library(&[format!("spotify:album:{album_id}")], saved)
                    .await
                    .map_err(err)
            }
            Source::Yandex => {
                let y = self.yandex().ok_or_else(|| err("no Yandex"))?;
                y.set_liked("album", album_id, saved).await.map_err(err)
            }
            Source::Vk => {
                let vk = self.vk().ok_or_else(|| err("no VK"))?;
                let (o, p, key) =
                    drx_catalog::vk::parse_playlist_id(album_id).ok_or_else(|| err("not a VK album"))?;
                let mine = vk.my_playlists().await.map_err(err)?;
                let have = mine.iter().find(|x| x.owner_id == o && x.playlist_id == p);
                match (saved, have) {
                    (true, None) => vk.follow_playlist(o, p, key).await.map(|_| ()).map_err(err),
                    (false, Some(x)) => vk.delete_playlist(x.owner_id, x.playlist_id).await.map_err(err),
                    _ => Ok(()),
                }
            }
        }
    }

    /// Is the artist followed: in the app's library or on the account of its catalog?
    pub async fn is_artist_followed(&self, artist_id: &str, name: &str) -> bool {
        let (id, name2) = (artist_id.to_string(), name.to_string());
        let in_app = self
            .store()
            .call(move |c| {
                let raw = id.strip_prefix("SP_").unwrap_or(&id).to_string();
                Ok(c.query_row(
                    "SELECT count(*) FROM artist WHERE bookmarkedAt IS NOT NULL AND (id = ?1 OR spotifyId = ?2 OR name = ?3)",
                    params![id, raw, name2],
                    |r| r.get::<_, i64>(0),
                )? > 0)
            })
            .await
            .unwrap_or(false);
        if in_app {
            return true;
        }
        let raw = artist_id.strip_prefix("SP_").unwrap_or(artist_id);
        match Source::of_id(raw) {
            Source::Spotify => match self.spotify() {
                Some(sp) => sp.library_ids("Artists").await.map(|s| s.contains(raw)).unwrap_or(false),
                None => false,
            },
            Source::Yandex => match self.yandex() {
                Some(y) => y.liked_ids("artists").await.map(|s| s.contains(raw)).unwrap_or(false),
                None => false,
            },
            Source::Vk => false,
        }
    }

    /// Follows or unfollows an artist: in the app's library, and on the account of its catalog (VK keeps none).
    pub async fn set_artist_followed(
        &self,
        artist_id: &str,
        name: &str,
        photo: Option<String>,
        followed: bool,
    ) -> AppResult<()> {
        let raw = artist_id.strip_prefix("SP_").unwrap_or(artist_id).to_string();
        let catalog = matches!(Source::of_id(&raw), Source::Spotify | Source::Yandex)
            && (crate::songs::is_spotify_id(&raw) || raw.starts_with(drx_catalog::yandex::PREFIX));
        let (raw2, name2, id2) = (raw.clone(), name.to_string(), artist_id.to_string());
        self.store()
            .call(move |c| {
                let found: Option<String> = c
                    .query_row(
                        "SELECT id FROM artist WHERE id = ?1 OR (?3 AND spotifyId = ?2) LIMIT 1",
                        params![id2, raw2, catalog],
                        |r| r.get(0),
                    )
                    .ok();
                let at: Option<i64> = followed.then(now_ms);
                match found {
                    Some(row) => {
                        c.execute(
                            "UPDATE artist SET bookmarkedAt = ?2, thumbnailUrl = coalesce(thumbnailUrl, ?3) WHERE id = ?1",
                            params![row, at, photo],
                        )?;
                    }
                    None if followed => {
                        let (row, sp) = if catalog {
                            (format!("SP_{raw2}"), Some(raw2.clone()))
                        } else {
                            (id2.clone(), None)
                        };
                        c.execute(
                            "INSERT OR IGNORE INTO artist (id, name, thumbnailUrl, lastUpdateTime, bookmarkedAt, spotifyId)
                             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                            params![row, name2, photo, now_ms(), sp],
                        )?;
                    }
                    None => {}
                }
                Ok(())
            })
            .await?;
        if !catalog {
            return Ok(());
        }
        match Source::of_id(&raw) {
            Source::Spotify => match self.spotify() {
                Some(sp) => sp
                    .set_in_library(&[format!("spotify:artist:{raw}")], followed)
                    .await
                    .map_err(err),
                None => Ok(()),
            },
            Source::Yandex => match self.yandex() {
                Some(y) => y.set_liked("artist", &raw, followed).await.map_err(err),
                None => Ok(()),
            },
            Source::Vk => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drx_catalog::model::ArtistRef;

    fn t(id: &str, title: &str, artist: &str, secs: u64) -> Track {
        Track {
            id: id.into(),
            title: title.into(),
            artists: vec![ArtistRef {
                id: String::new(),
                name: artist.into(),
            }],
            album: None,
            duration_ms: secs * 1000,
            isrc: None,
            explicit: false,
            plays: None,
        }
    }

    #[test]
    fn norm_keeps_remix_drops_credits() {
        assert_eq!(cross_norm("Song (feat. Someone)"), "song");
        assert_eq!(cross_norm("Song - 2011 Remaster"), "song");
        assert_eq!(cross_norm("Sóng [Radio Edit]"), "song");
        assert_eq!(cross_norm("Song (Remix)"), "songremix");
        assert_eq!(cross_norm("Ёлка"), "елка"); // NFD splits ё as on Android
    }

    #[test]
    fn best_match_needs_title_artist_and_length() {
        let w = Wanted {
            title: "Mahjong".into(),
            artists: vec!["ASAVA".into()],
            duration_s: 200,
            album: None,
        };
        let found = vec![
            t("a", "Mahjong", "Someone Else", 200),
            t("b", "Mahjong", "ASAVA", 230),
            t("c", "Mahjong (Extended)", "Asava", 205),
            t("d", "Mahjong", "Asava", 203),
        ];
        assert_eq!(best_of(&found, &w, true).as_deref(), Some("d"));
        assert_eq!(best_of(&found[..2], &w, true), None);
    }

    #[test]
    fn like_mode_keys_round_trip() {
        for m in [LikeMode::MainOnly, LikeMode::MainAndSource, LikeMode::All] {
            assert_eq!(LikeMode::from_key(m.key()), m);
        }
        assert_eq!(LikeMode::from_key("junk"), LikeMode::MainAndSource);
    }
}
