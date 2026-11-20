//! The home page (port of android `home/HomeFeed.kt`): personal rows built from what the listener loves, kept on
//! disk for 6 hours and rebuilt behind the page when older.
//!
//! "Favourite" means loved, not just played: likes, bookmarked artists and tracks played again and again count;
//! plain plays only break ties (same weights as Android). Rows, as on Android: «Похоже на <исполнитель>» (related
//! artists' tracks new to the listener), «Новое у любимых», «Альбомы, которые вы не слушали», «Глубже в любимое»
//! (album tracks of loved artists that are not their hits), «Давно не слушали» (old likes not heard for 60 days),
//! plus «Недавно слушали» from the history, and «Похожее в <жанре>» rows for the listener's main genres (artists
//! close to their favourites in that genre, kept to the genre by Last.fm). Not ported yet: Last.fm agreement for
//! related artists.

use crate::lists::{SongRow, SONG_COVER};
use crate::playback::Playable;
use crate::{App, AppError, AppResult};
use drx_catalog::model::{ArtistPage, Image, Source, Track};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(6 * 60 * 60);
const FORGOTTEN_AFTER_DAYS: i64 = 60;
const MIN_LOVE: f64 = 1.0;
/// Bumped when rows are chosen differently: an older saved feed is rebuilt at once.
const FORMAT: u32 = 2;
const FILE: &str = "home_feed.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomeRowKind {
    Tracks,
    Albums,
}

/// An album card on the home page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HomeAlbum {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub images: Vec<Image>,
    pub date: Option<String>,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HomeRow {
    pub kind: HomeRowKind,
    pub title: String,
    /// The artist a «Похоже на» row is about (its title links there).
    pub artist_id: Option<String>,
    pub tracks: Vec<Track>,
    pub albums: Vec<HomeAlbum>,
    /// The line under a genre row's title («Рядом с <исполнитель>»).
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HomeFeed {
    /// Unix milliseconds.
    pub built_at: i64,
    pub format: u32,
    pub rows: Vec<HomeRow>,
}

impl HomeFeed {
    fn fresh(&self) -> bool {
        self.format == FORMAT && now_ms() - self.built_at < TTL.as_millis() as i64
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Small seeded random (splitmix64): the day picks the rows, a refresh picks anew.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// "artist|title" without the version part (HomeFeed.trackKey).
fn track_key(artist: &str, title: &str) -> String {
    let t = title.to_lowercase();
    let t = t.split(" (").next().unwrap_or("");
    let t = t.split(" - ").next().unwrap_or("");
    format!("{}|{}", norm(artist), norm(t))
}

fn album_key(artist: &str, album: &str) -> String {
    let a = album.to_lowercase();
    format!(
        "{}|{}",
        norm(artist),
        norm(a.split(" (").next().unwrap_or(""))
    )
}

/// The catalog id behind an app artist id (ArtistIdentity.spotifyIdOf): `SP_<id>`, `SP_ym_<n>`, a bare Spotify
/// id or a Yandex one; None for YouTube, VK and local artists (found by name instead).
pub(crate) fn catalog_id_of(id: &str) -> Option<String> {
    let id = id
        .strip_prefix("spotify:")
        .or_else(|| id.strip_prefix("SP_"))
        .unwrap_or(id);
    if id.is_empty() {
        return None;
    }
    if id.starts_with("ym_") {
        return Some(id.to_string());
    }
    (id.len() == 22 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_string())
}

#[derive(Clone)]
struct Favorite {
    name: String,
    id: Option<String>,
    score: f64,
}

struct Taste {
    favorites: Vec<Favorite>,
    /// "artist|title" of everything liked or played: not suggested as new.
    known: HashSet<String>,
    /// "artist|album" → different tracks played from it.
    album_tracks: HashMap<String, HashSet<String>>,
    /// Every artist liked, played or followed (lower case): not suggested as new.
    known_artists: HashSet<String>,
    /// Likes per artist and recent plays per artist (decaying), for the genre weights.
    likes: HashMap<String, f64>,
    plays: HashMap<String, f64>,
}

/// Favourites from the database: plays of the last 5000 events (decaying over 90 days, capped), repeats (3+ plays
/// of a track), likes (+1 each), bookmarked artists (+4) — the weights of HomeFeed.taste.
fn taste(c: &mut rusqlite::Connection) -> rusqlite::Result<Taste> {
    let mut love: HashMap<String, f64> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut known = HashSet::new();
    let mut album_tracks: HashMap<String, HashSet<String>> = HashMap::new();
    let mut likes: HashMap<String, f64> = HashMap::new();
    let mut note =
        |name: &str, id: Option<String>, names: &mut HashMap<String, String>| -> Option<String> {
            let n = name.trim();
            if n.is_empty() {
                return None;
            }
            let key = n.to_lowercase();
            names.entry(key.clone()).or_insert_with(|| n.to_string());
            if let Some(cid) = id.as_deref().and_then(catalog_id_of) {
                ids.entry(key.clone()).or_insert(cid);
            }
            Some(key)
        };
    let first_artist = "(SELECT ar.name || char(31) || ar.id FROM song_artist_map m JOIN artist ar ON ar.id = m.artistId WHERE m.songId = s.id ORDER BY m.position LIMIT 1)";
    let split = |v: Option<String>| -> Option<(String, String)> {
        let v = v?;
        let (n, i) = v.split_once('\u{1f}')?;
        Some((n.to_string(), i.to_string()))
    };

    let now = now_ms();
    let mut plays: HashMap<String, f64> = HashMap::new();
    let mut chosen: HashMap<String, (u32, String)> = HashMap::new();
    let mut st = c.prepare(&format!(
        "SELECT {first_artist}, s.title, s.albumName, e.timestamp, e.playTime, s.duration
         FROM event e JOIN song s ON s.id = e.songId ORDER BY e.timestamp DESC LIMIT 5000"
    ))?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;
    for row in rows {
        let (artist, title, album, ts, play_ms, dur_s) = row?;
        let Some((name, id)) = split(artist) else {
            continue;
        };
        let Some(key) = note(&name, Some(id), &mut names) else {
            continue;
        };
        let tk = track_key(&name, &title);
        known.insert(tk.clone());
        if let Some(a) = album.filter(|a| !a.trim().is_empty()) {
            album_tracks
                .entry(album_key(&name, &a))
                .or_default()
                .insert(tk.clone());
        }
        let age_days = (now - ts) as f64 / 86_400_000.0;
        // A play counts when it lasted 30 s or half the track (Listening.counts).
        let counts = play_ms >= 30_000 || (dur_s > 0 && play_ms * 2 >= dur_s * 1000);
        if age_days > 240.0 || !counts {
            continue;
        }
        *plays.entry(key.clone()).or_default() += (-age_days / 90.0).exp();
        let e = chosen.entry(tk).or_insert((0, key));
        e.0 += 1;
    }
    let mut repeats: HashMap<String, f64> = HashMap::new();
    for (n, key) in chosen.values() {
        if *n >= 3 {
            *repeats.entry(key.clone()).or_default() += 0.6;
        }
    }
    for (k, r) in repeats {
        *love.entry(k).or_default() += r.min(1.2);
    }
    for (k, p) in &plays {
        *love.entry(k.clone()).or_default() += (p * 0.02).min(0.5);
    }

    let mut st = c.prepare(&format!(
        "SELECT {first_artist}, s.title FROM song s WHERE s.liked = 1"
    ))?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (artist, title) = row?;
        let Some((name, id)) = split(artist) else {
            continue;
        };
        let Some(key) = note(&name, Some(id), &mut names) else {
            continue;
        };
        known.insert(track_key(&name, &title));
        *love.entry(key.clone()).or_default() += 1.0;
        *likes.entry(key).or_default() += 1.0;
    }
    let mut st = c.prepare("SELECT name, id FROM artist WHERE bookmarkedAt IS NOT NULL")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (name, id) = row?;
        let Some(key) = note(&name, Some(id), &mut names) else {
            continue;
        };
        *love.entry(key).or_default() += 4.0;
    }

    let known_artists: HashSet<String> = names.keys().cloned().collect();
    let mut fav: Vec<(String, f64)> = love.into_iter().filter(|(_, s)| *s >= MIN_LOVE).collect();
    fav.sort_by(|a, b| b.1.total_cmp(&a.1));
    let favorites = fav
        .into_iter()
        .take(40)
        .map(|(k, score)| Favorite {
            name: names.get(&k).cloned().unwrap_or(k.clone()),
            id: ids.get(&k).cloned(),
            score,
        })
        .collect();
    Ok(Taste {
        favorites,
        known,
        album_tracks,
        known_artists,
        likes,
        plays,
    })
}

/// `count` favourites drawn at random, the more loved the likelier, from the strongest `pool` with an id.
fn pick(favorites: &[Favorite], count: usize, pool: usize, rng: &mut Rng) -> Vec<Favorite> {
    let mut left: Vec<Favorite> = favorites
        .iter()
        .filter(|f| f.id.is_some())
        .take(pool)
        .cloned()
        .collect();
    let mut out = Vec::new();
    while out.len() < count && !left.is_empty() {
        let total: f64 = left.iter().map(|f| f.score).sum();
        let mut r = rng.unit() * total;
        let mut idx = left.len() - 1;
        for (i, f) in left.iter().enumerate() {
            r -= f.score;
            if r <= 0.0 {
                idx = i;
                break;
            }
        }
        out.push(left.remove(idx));
    }
    out
}

fn interleave<T>(lists: Vec<Vec<T>>) -> Vec<T> {
    let mut queues: Vec<std::collections::VecDeque<T>> =
        lists.into_iter().map(Into::into).collect();
    let mut out = Vec::new();
    while queues.iter().any(|q| !q.is_empty()) {
        for q in &mut queues {
            if let Some(x) = q.pop_front() {
                out.push(x);
            }
        }
    }
    out
}

impl App {
    fn home_file(&self) -> std::path::PathBuf {
        self.profile().root().join(FILE)
    }

    /// The last built home page, from disk (instant; may be stale).
    pub fn home_cached(&self) -> Option<HomeFeed> {
        let bytes = std::fs::read(self.home_file()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// The home page: the saved one while it is fresh, otherwise built anew (`force` = a refresh, new picks).
    pub async fn home(&self, force: bool) -> AppResult<HomeFeed> {
        if !force && let Some(f) = self.home_cached().filter(HomeFeed::fresh) {
            return Ok(f);
        }
        let _guard = self.inner.home_lock.lock().await;
        if !force && let Some(f) = self.home_cached().filter(HomeFeed::fresh) {
            return Ok(f);
        }
        let day = (now_ms() / 86_400_000) as u64;
        let seed = if force { now_ms() as u64 } else { day };
        let feed = self.build_home(seed).await?;
        if let Ok(bytes) = serde_json::to_vec(&feed) {
            let _ = std::fs::write(self.home_file(), bytes);
        }
        Ok(feed)
    }

    /// Plays tracks that are not a list of the library (a home row) from `index`.
    pub fn play_tracks(&self, tracks: Vec<Track>, index: usize) {
        let items: Vec<Playable> = tracks
            .into_iter()
            .map(|track| Playable { track, url: None })
            .collect();
        self.remember(items.iter().map(|p| &p.track));
        self.inner.playback.play_list(items, index);
    }

    /// The catalog heading (photo, releases) for a library artist page whose artist is also in a catalog
    /// (`SP_<id>`, `SP_ym_<n>`, or found on Spotify by its exact name); the page keeps the library's songs.
    pub async fn library_artist_header(&self, id: &str, name: &str) -> Option<crate::ListHeader> {
        let cid = match catalog_id_of(id) {
            Some(c) => c,
            None => {
                self.spotify_or_public()
                    .search_artists(name, 3)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|a| a.name.eq_ignore_ascii_case(name.trim()))?
                    .id
            }
        };
        let page = self.catalog_artist(&cid).await?;
        let mut h = crate::accounts::artist_header(&page);
        h.subtitle = if h.subtitle.is_empty() {
            "В медиатеке".into()
        } else {
            format!("{} · в медиатеке", h.subtitle)
        };
        Some(h)
    }

    /// All library songs of `artist_title` grouped by album and sorted by year descending.
    pub async fn artist_library_albums(
        &self,
        artist_title: &str,
    ) -> AppResult<Vec<(String, String, i64, Option<String>, SongRow)>> {
        let artist_title_q = artist_title.to_string();
        let rows = self.store()
            .call(move |c| {
                let sql = format!(
                    "SELECT coalesce(al.id, s.albumId, s.albumName, 'single'),
                            coalesce(al.title, s.albumName, 'Синглы'),
                            coalesce(al.year, s.year, 0) AS yr,
                            coalesce(al.thumbnailUrl, s.thumbnailUrl),
                            s.id, s.title, (SELECT group_concat(a.name, ', ') FROM (SELECT ar.name FROM song_artist_map m JOIN artist ar ON ar.id = m.artistId WHERE m.songId = s.id ORDER BY m.position) a),
                            s.duration, {SONG_COVER}, s.liked, (SELECT m.artistId FROM song_artist_map m WHERE m.songId = s.id ORDER BY m.position LIMIT 1), s.albumId
                     FROM song_artist_map sam
                     JOIN song s ON s.id = sam.songId
                     LEFT JOIN album al ON al.id = s.albumId
                     WHERE sam.artistId = (SELECT id FROM artist WHERE name = ?1 LIMIT 1)
                        OR s.artistsText LIKE ?2
                     ORDER BY yr DESC, coalesce(al.title, s.albumName), s.id"
                );
                let pat = format!("%{artist_title_q}%");
                let mut st = c.prepare(&sql)?;
                let rows = st
                    .query_map([&artist_title_q, &pat], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            SongRow {
                                id: r.get(4)?,
                                title: r.get(5)?,
                                artists: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                                album: Some(r.get(1)?),
                                duration_s: r.get(7)?,
                                thumbnail: r.get(8)?,
                                liked: r.get(9)?,
                                artist_id: r.get(10)?,
                                album_id: r.get(11)?,
                                plays: None,
                            },
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;
        Ok(rows)
    }

    /// An artist page from its catalog (Spotify without a session works for public data).
    async fn catalog_artist(&self, id: &str) -> Option<ArtistPage> {
        match Source::of_id(id) {
            Source::Yandex => self.yandex()?.artist(id).await.ok(),
            Source::Vk => self.vk()?.artist(id).await.ok().map(|(p, _)| p),
            Source::Spotify => self.spotify_or_public().artist(id).await.ok(),
        }
    }

    pub(crate) fn spotify_or_public(&self) -> Arc<drx_catalog::spotify::Spotify> {
        self.spotify().unwrap_or_else(|| {
            Arc::new(drx_catalog::spotify::Spotify::new(
                self.inner.http.clone(),
                None,
                None,
            ))
        })
    }

    async fn build_home(&self, seed: u64) -> AppResult<HomeFeed> {
        let mut taste = self
            .store()
            .call(|c| Ok(taste(c)?))
            .await
            .map_err(|e| AppError::Other(e.to_string()))?;
        let mut rows: Vec<HomeRow> = Vec::new();

        // Recent: the history, one entry per song.
        let h = self.open_list(crate::ListSpec::RecentSongs);
        let recent: Vec<Track> = self
            .list_rows(h, 0..20)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| crate::playable_of(r).track)
            .collect();
        self.close_list(h);
        if !recent.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::Tracks,
                title: "Недавно слушали".into(),
                artist_id: None,
                tracks: recent,
                albums: Vec::new(),
                label: None,
            });
        }

        // Favourites without a catalog id (YouTube, VK, local): found by name, the strongest 20 only.
        let sp = self.spotify_or_public();
        for f in taste.favorites.iter_mut().take(20) {
            if f.id.is_none()
                && let Ok(found) = sp.search_artists(&f.name, 3).await
            {
                f.id = found
                    .into_iter()
                    .find(|a| a.name.eq_ignore_ascii_case(&f.name))
                    .map(|a| a.id);
            }
        }

        // One artist page per seed (popular tracks, releases, related): fetched once, a few at a time.
        let mut rng = Rng(seed ^ 0x5eed);
        let similar_seeds = pick(&taste.favorites, 3, 14, &mut rng);
        let unheard_seeds = pick(&taste.favorites, 10, 20, &mut rng);
        let deep_seeds = pick(&taste.favorites, 5, 15, &mut rng);
        let new_seeds: Vec<Favorite> = taste
            .favorites
            .iter()
            .filter(|f| f.id.is_some())
            .take(12)
            .cloned()
            .collect();
        let mut want: Vec<String> = Vec::new();
        for f in similar_seeds
            .iter()
            .chain(&unheard_seeds)
            .chain(&deep_seeds)
            .chain(&new_seeds)
        {
            if let Some(id) = &f.id
                && !want.contains(id)
            {
                want.push(id.clone());
            }
        }
        let pages = self.artist_pages(&want).await;

        // «Похоже на»: related artists' popular tracks that are new to the listener, two per artist.
        for (i, seed) in similar_seeds.iter().enumerate() {
            let Some(page) = seed.id.as_ref().and_then(|id| pages.get(id)) else {
                continue;
            };
            let mut related: Vec<String> = page
                .related
                .iter()
                .filter(|a| a.name.to_lowercase() != seed.name.to_lowercase())
                .take(10)
                .map(|a| a.id.clone())
                .collect();
            let mut r = Rng(seed_mix(seed_u(&seed.name), i as u64, rng.next()));
            r.shuffle(&mut related);
            related.truncate(7);
            let rel_pages = self.artist_pages(&related).await;
            let lists: Vec<Vec<Track>> = related
                .iter()
                .filter_map(|id| rel_pages.get(id))
                .map(|p| {
                    let mut t: Vec<Track> = p
                        .top_tracks
                        .iter()
                        .filter(|t| {
                            let a = t.artists.first().map(|a| a.name.as_str()).unwrap_or("");
                            !taste.known.contains(&track_key(a, &t.title))
                        })
                        .take(5)
                        .cloned()
                        .collect();
                    r.shuffle(&mut t);
                    t.truncate(2);
                    t
                })
                .collect();
            let tracks: Vec<Track> = interleave(lists).into_iter().take(14).collect();
            if tracks.len() >= 4 {
                rows.push(HomeRow {
                    kind: HomeRowKind::Tracks,
                    title: format!("Похоже на {}", page.name),
                    artist_id: seed.id.clone(),
                    tracks,
                    albums: Vec::new(),
                    label: None,
                });
            }
        }

        // «Новое у любимых»: releases of the strongest favourites from the last 120 days, newest first.
        let cutoff = date_days_ago(120);
        let mut fresh: Vec<HomeAlbum> = new_seeds
            .iter()
            .filter_map(|f| pages.get(f.id.as_ref()?))
            .flat_map(|p| {
                p.releases
                    .iter()
                    .filter(|r| {
                        r.date
                            .as_deref()
                            .is_some_and(|d| d.len() >= 10 && d >= cutoff.as_str())
                    })
                    .map(|r| home_album(r, &p.name))
                    .collect::<Vec<_>>()
            })
            .collect();
        fresh.sort_by(|a, b| b.date.cmp(&a.date));
        dedup_albums(&mut fresh);
        fresh.truncate(16);
        if !fresh.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::Albums,
                title: "Новое у любимых".into(),
                artist_id: None,
                tracks: Vec::new(),
                albums: fresh,
                label: None,
            });
        }

        // «Альбомы, которые вы не слушали»: albums/EPs of loved artists never played or left after 1-2 tracks.
        let lists: Vec<Vec<HomeAlbum>> = unheard_seeds
            .iter()
            .filter_map(|f| pages.get(f.id.as_ref()?).map(|p| (f, p)))
            .map(|(f, p)| {
                let mut a: Vec<HomeAlbum> = p
                    .releases
                    .iter()
                    .filter(|r| matches!(r.kind.as_str(), "album" | "ep"))
                    .filter(|r| {
                        taste
                            .album_tracks
                            .get(&album_key(&f.name, &r.name))
                            .map_or(0, HashSet::len)
                            < 3
                    })
                    .map(|r| home_album(r, &p.name))
                    .collect();
                rng.shuffle(&mut a);
                a.truncate(2);
                a
            })
            .collect();
        let mut unheard = interleave(lists);
        dedup_albums(&mut unheard);
        unheard.truncate(16);
        if !unheard.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::Albums,
                title: "Альбомы, которые вы не слушали".into(),
                artist_id: None,
                tracks: Vec::new(),
                albums: unheard,
                label: None,
            });
        }

        // «Глубже в любимое»: tracks from albums of loved artists that are not their hits and not heard yet.
        let mut lists: Vec<Vec<Track>> = Vec::new();
        for f in &deep_seeds {
            let Some(p) = f.id.as_ref().and_then(|id| pages.get(id)) else {
                continue;
            };
            let hits: HashSet<&str> = p.top_tracks.iter().map(|t| t.id.as_str()).collect();
            let mut albums: Vec<&drx_catalog::model::Release> =
                p.releases.iter().filter(|r| r.kind == "album").collect();
            rng.shuffle(&mut albums);
            for rel in albums.into_iter().take(2) {
                let Some(album) = self.catalog_album(&rel.id).await else {
                    continue;
                };
                let mut t: Vec<Track> = album
                    .tracks
                    .into_iter()
                    .filter(|t| !t.id.is_empty() && !hits.contains(t.id.as_str()))
                    .filter(|t| {
                        let a = t
                            .artists
                            .first()
                            .map(|a| a.name.as_str())
                            .unwrap_or(&f.name);
                        !taste.known.contains(&track_key(a, &t.title))
                    })
                    .collect();
                rng.shuffle(&mut t);
                t.truncate(2);
                lists.push(t);
            }
        }
        let deep: Vec<Track> = interleave(lists).into_iter().take(14).collect();
        if !deep.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::Tracks,
                title: "Глубже в любимое".into(),
                artist_id: None,
                tracks: deep,
                albums: Vec::new(),
                label: None,
            });
        }

        // «Похожее в <жанре>»: the listener's main genres (HomeFeed.genreStats / genreRows).
        rows.extend(self.genre_rows(&taste, &mut rng).await);

        // «Давно не слушали»: likes older than 60 days not heard for 60 days, one per artist.
        let cut = now_ms() - FORGOTTEN_AFTER_DAYS * 86_400_000;
        let forgotten_rows = self
            .store()
            .call(move |c| crate::lists::forgotten(c, cut))
            .await
            .unwrap_or_default();
        let mut forgotten: Vec<Track> = forgotten_rows
            .into_iter()
            .map(|r| crate::playable_of(r).track)
            .collect();
        rng.shuffle(&mut forgotten);
        let mut seen = HashSet::new();
        forgotten.retain(|t| {
            seen.insert(
                t.artists
                    .first()
                    .map(|a| a.name.to_lowercase())
                    .unwrap_or_default(),
            )
        });
        forgotten.truncate(16);
        if !forgotten.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::Tracks,
                title: "Давно не слушали".into(),
                artist_id: None,
                tracks: forgotten,
                albums: Vec::new(),
                label: None,
            });
        }

        // Android's order (HomeScreen sections): recent, new releases, unheard albums, forgotten, deep cuts,
        // then «Похоже на» rows at the bottom.
        let rank = |r: &HomeRow| match r.title.as_str() {
            "Недавно слушали" => 0,
            "Новое у любимых" => 1,
            "Альбомы, которые вы не слушали" => 2,
            t if t.starts_with("Похожее в ") => 3,
            "Давно не слушали" => 4,
            "Глубже в любимое" => 5,
            _ => 6,
        };
        rows.sort_by_key(rank);
        Ok(HomeFeed {
            built_at: now_ms(),
            format: FORMAT,
            rows,
        })
    }

    /// HomeFeed.genreStats: each artist's two main genres weighted by how loved and how played the artist is; rows
    /// go to the two strongest genres with at least two loved artists, plus the one liked most for how little it is
    /// played.
    async fn genre_stats(&self, taste: &Taste) -> (Vec<String>, HashMap<String, Vec<String>>) {
        const UMBRELLA: &[&str] = &[
            "metal", "rock", "pop", "hip-hop", "electronic", "alternative rock", "alternative", "indie",
            "punk", "dance", "experimental", "electronic dance", "hard rock", "heavy metal", "soundtrack",
            "classical", "jazz", "folk", "r&b", "soul", "rap",
        ];
        let mut artists: Vec<String> = taste.favorites.iter().take(40).map(|f| f.name.clone()).collect();
        let mut by_plays: Vec<(&String, &f64)> = taste.plays.iter().collect();
        by_plays.sort_by(|a, b| b.1.total_cmp(a.1));
        for (k, _) in by_plays.into_iter().take(30) {
            if !artists.iter().any(|a| a.to_lowercase() == *k) {
                artists.push(k.clone());
            }
        }
        self.ensure_genres(&artists).await;
        let fav_score: HashMap<String, f64> =
            taste.favorites.iter().map(|f| (f.name.to_lowercase(), f.score)).collect();
        let (mut love, mut like, mut play) =
            (HashMap::<String, f64>::new(), HashMap::<String, f64>::new(), HashMap::<String, f64>::new());
        let mut artists_of: HashMap<String, Vec<String>> = HashMap::new();
        for name in &artists {
            let key = name.to_lowercase();
            let genres: Vec<String> = self
                .cached_genres(name)
                .into_iter()
                .filter(|g| !UMBRELLA.contains(&g.to_lowercase().as_str()))
                .fold(Vec::new(), |mut v, g| {
                    if !v.contains(&g) {
                        v.push(g);
                    }
                    v
                })
                .into_iter()
                .take(2)
                .collect();
            for (i, g) in genres.iter().enumerate() {
                let part = if i == 0 { 1.0 } else { 0.5 };
                *love.entry(g.clone()).or_default() += fav_score.get(&key).copied().unwrap_or(0.0) * part;
                *like.entry(g.clone()).or_default() += taste.likes.get(&key).copied().unwrap_or(0.0) * part;
                *play.entry(g.clone()).or_default() += taste.plays.get(&key).copied().unwrap_or(0.0) * part;
                artists_of.entry(g.clone()).or_default().push(name.clone());
            }
        }
        let total_love = love.values().sum::<f64>().max(1.0);
        let total_plays = play.values().sum::<f64>().max(1.0);
        let total_likes = like.values().sum::<f64>().max(1.0);
        let mut top: Vec<(String, f64)> = love
            .keys()
            .map(|g| (g.clone(), love[g] / total_love + play.get(g).copied().unwrap_or(0.0) / total_plays))
            .collect();
        top.sort_by(|a, b| b.1.total_cmp(&a.1));
        let worthy: Vec<String> = top
            .iter()
            .map(|(g, _)| g.clone())
            .filter(|g| {
                artists_of
                    .get(g)
                    .map_or(0, |v| v.iter().filter(|a| fav_score.contains_key(&a.to_lowercase())).count())
                    >= 2
            })
            .collect();
        let mut rows: Vec<String> = worthy.iter().take(2).cloned().collect();
        let loved_more = like
            .iter()
            .filter(|(g, l)| !rows.contains(g) && worthy.contains(g) && **l >= 3.0)
            .max_by(|a, b| {
                let score = |(g, l): (&String, &f64)| {
                    (l / total_likes) / (play.get(g).copied().unwrap_or(0.0) / total_plays + 0.02)
                };
                score(*a).total_cmp(&score(*b))
            })
            .map(|(g, _)| g.clone());
        rows.extend(loved_more);
        (rows, artists_of)
    }

    /// HomeFeed.genreRows: for each row genre, artists the catalog lists as close to up to four of the listener's
    /// favourites in it, new to the listener, in-genre ones first (Last.fm's top artists of the genre or their own
    /// tags); two popular tracks of seven of them.
    async fn genre_rows(&self, taste: &Taste, rng: &mut Rng) -> Vec<HomeRow> {
        let (genres, artists_of) = self.genre_stats(taste).await;
        let by_name: HashMap<String, &Favorite> =
            taste.favorites.iter().map(|f| (f.name.to_lowercase(), f)).collect();
        let mut out = Vec::new();
        for (index, genre) in genres.iter().enumerate() {
            let mut r = Rng(seed_mix(seed_u(genre), index as u64, rng.next()));
            let of_genre = self.genre_top_artists(genre).await;
            let fam = crate::genres::family(genre).to_lowercase();
            let in_genre = |app: &App, name: &str| {
                of_genre.contains(&crate::genres::norm(name))
                    || app.cached_genres(name).iter().any(|g| {
                        g.eq_ignore_ascii_case(genre) || crate::genres::family(g).to_lowercase() == fam
                    })
            };
            let mine: Vec<Favorite> = artists_of
                .get(genre)
                .map(|v| {
                    v.iter()
                        .filter_map(|a| by_name.get(&a.to_lowercase()).map(|f| (*f).clone()))
                        .filter(|f| f.id.is_some())
                        .collect()
                })
                .unwrap_or_default();
            let seeds = pick(&mine, 4, 8, &mut r);
            let seed_ids: Vec<String> = seeds.iter().filter_map(|f| f.id.clone()).collect();
            let pages = self.artist_pages(&seed_ids).await;
            let related: Vec<(String, String)> = seed_ids
                .iter()
                .filter_map(|id| pages.get(id))
                .flat_map(|p| p.related.iter().map(|a| (a.id.clone(), a.name.clone())))
                .filter(|(id, name)| !id.is_empty() && !taste.known_artists.contains(&name.to_lowercase()))
                .collect();
            let mut counted: HashMap<String, usize> = HashMap::new();
            for (id, _) in &related {
                *counted.entry(id.clone()).or_default() += 1;
            }
            let mut seen = HashSet::new();
            let mut candidates: Vec<(String, String, bool)> = related
                .into_iter()
                .filter(|(id, _)| seen.insert(id.clone()))
                .map(|(id, name)| {
                    let g = in_genre(self, &name);
                    (id, name, g)
                })
                .collect();
            candidates.sort_by_key(|(id, _, g)| std::cmp::Reverse(counted.get(id).copied().unwrap_or(0) + if *g { 2 } else { 0 }));
            let (inside, outside): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|(_, _, g)| *g);
            let mut chosen: Vec<(String, String, bool)> = inside.into_iter().chain(outside).take(12).collect();
            r.shuffle(&mut chosen);
            chosen.truncate(7);
            let ids: Vec<String> = chosen.iter().map(|(id, _, _)| id.clone()).collect();
            let tops = self.artist_pages(&ids).await;
            let lists: Vec<Vec<Track>> = ids
                .iter()
                .filter_map(|id| tops.get(id))
                .map(|p| {
                    let mut t: Vec<Track> = p
                        .top_tracks
                        .iter()
                        .filter(|t| {
                            let a = t.artists.first().map(|a| a.name.as_str()).unwrap_or("");
                            !t.id.is_empty() && !taste.known.contains(&track_key(a, &t.title))
                        })
                        .take(5)
                        .cloned()
                        .collect();
                    r.shuffle(&mut t);
                    t.truncate(2);
                    t
                })
                .collect();
            let tracks: Vec<Track> = interleave(lists).into_iter().take(14).collect();
            tracing::info!("home: more {genre} from {:?}: {} tracks", seeds.iter().map(|f| &f.name).collect::<Vec<_>>(), tracks.len());
            if tracks.len() >= 4 {
                out.push(HomeRow {
                    kind: HomeRowKind::Tracks,
                    title: format!("Похожее в {genre}"),
                    artist_id: None,
                    tracks,
                    albums: Vec::new(),
                    label: seeds.first().map(|f| format!("Рядом с {}", f.name)),
                });
            }
        }
        out
    }

    /// Artist pages for ids, four requests at a time; failures are skipped.
    async fn artist_pages(&self, ids: &[String]) -> HashMap<String, ArtistPage> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(4) {
            let mut tasks = Vec::new();
            for id in chunk {
                let (app, id) = (self.clone(), id.clone());
                tasks.push(tokio::spawn(async move {
                    let p = app.catalog_artist(&id).await;
                    (id, p)
                }));
            }
            for t in tasks {
                if let Ok((id, Some(p))) = t.await {
                    out.insert(id, p);
                }
            }
        }
        out
    }

    pub async fn catalog_album(&self, id: &str) -> Option<drx_catalog::model::Album> {
        match Source::of_id(id) {
            Source::Yandex => self.yandex()?.album(id).await.ok(),
            Source::Spotify => self.spotify_or_public().album(id).await.ok(),
            Source::Vk => None,
        }
    }
}

fn seed_u(s: &str) -> u64 {
    s.bytes().fold(1469598103934665603u64, |h, b| {
        (h ^ b as u64).wrapping_mul(1099511628211)
    })
}

fn seed_mix(a: u64, b: u64, c: u64) -> u64 {
    a.wrapping_mul(31)
        .wrapping_add(b)
        .wrapping_mul(31)
        .wrapping_add(c)
}

fn home_album(r: &drx_catalog::model::Release, artist: &str) -> HomeAlbum {
    HomeAlbum {
        id: r.id.clone(),
        name: r.name.clone(),
        artist: artist.to_string(),
        images: r.images.clone(),
        date: r.date.clone(),
        kind: r.kind.clone(),
    }
}

fn dedup_albums(v: &mut Vec<HomeAlbum>) {
    let mut seen = HashSet::new();
    v.retain(|a| seen.insert(a.id.clone()));
}

/// "YYYY-MM-DD" of `days` ago (civil date from Unix days, no date library).
fn date_days_ago(days: i64) -> String {
    let z = now_ms() / 86_400_000 - days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_drop_versions() {
        assert_eq!(
            track_key("The Weeknd", "Blinding Lights (Remix)"),
            "theweeknd|blindinglights"
        );
        assert_eq!(track_key("A", "Song - Live"), "a|song");
    }

    #[test]
    fn catalog_ids() {
        assert_eq!(
            catalog_id_of("SP_0OdUWJ0sBjDrqHygGUXeCF").as_deref(),
            Some("0OdUWJ0sBjDrqHygGUXeCF")
        );
        assert_eq!(catalog_id_of("SP_ym_1057").as_deref(), Some("ym_1057"));
        assert_eq!(catalog_id_of("UCQ2abcdefghijklmnopqrst"), None);
        assert_eq!(catalog_id_of("vkar_123"), None);
    }

    #[test]
    fn weighted_pick_is_deterministic() {
        let f = |n: &str, s: f64| Favorite {
            name: n.into(),
            id: Some(n.into()),
            score: s,
        };
        let favs = vec![f("a", 10.0), f("b", 5.0), f("c", 1.0)];
        let x = pick(&favs, 2, 3, &mut Rng(7));
        let y = pick(&favs, 2, 3, &mut Rng(7));
        assert_eq!(x.len(), 2);
        assert_eq!(
            x.iter().map(|f| &f.name).collect::<Vec<_>>(),
            y.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn civil_date_shape() {
        let d = date_days_ago(0);
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
    }
}
