//! Live lists behind opaque handles. The UI asks for a row range and is told when rows change, so Mini can use a
//! virtual list model and Full a virtual scroller over the same query. Paging stays in the core.

use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListHandle(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListSpec {
    /// Liked songs, newest like first (Android library «Liked»).
    LikedSongs,
    /// Songs in the library, by title.
    LibrarySongs,
    /// Songs of a local playlist, in playlist order.
    Playlist {
        id: String,
    },
    /// Listening history, newest first.
    History,
    /// Songs heard lately, each once, the latest first (home «Недавно слушали»).
    RecentSongs,
    /// A saved album, in track order (`song_album_map.index`, as Android's album page).
    Album {
        id: String,
    },
    /// Songs of an artist that are in the database, most played first.
    Artist {
        id: String,
    },
    /// VK «Моя музыка», loaded from VK (not stored in the database).
    VkMyMusic,
    /// A VK album or playlist (`vk_<owner>_<id>[_<key>]`), loaded from VK.
    VkPlaylist {
        id: String,
    },
    /// Spotify liked songs, loaded from Spotify.
    SpotifyLiked,
    SpotifyPlaylist {
        id: String,
    },
    /// Yandex liked tracks, loaded from Yandex.
    YandexLiked,
    YandexPlaylist {
        id: String,
    },
    /// Track search in the main account's catalog.
    Search {
        query: String,
    },
    /// An album from a catalog (`ym_…` Yandex, `vk_…` VK, otherwise Spotify), loaded over the network.
    CatalogAlbum {
        id: String,
    },
    /// An artist's popular tracks from a catalog (`ym_…`, `vkar_…`, otherwise Spotify).
    CatalogArtist {
        id: String,
    },
    /// Tracks downloaded for offline listening, the newest first.
    Downloads,
    /// «Мой топ» (Android TopPlaylistScreen): the most listened songs (by time) of a period, `size` of them.
    Top {
        period: TopPeriod,
        size: usize,
    },
    /// A YouTube Music playlist, album or podcast (`VL…`/`PL…`, `MPREb…`, `MPSP…`), loaded over the network.
    YtList {
        id: String,
    },
}

/// MyTopFilter: the period «Мой топ» counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TopPeriod {
    #[default]
    AllTime,
    Day,
    Week,
    Month,
    Year,
}

impl TopPeriod {
    pub const ALL: [TopPeriod; 5] = [
        TopPeriod::Day,
        TopPeriod::Week,
        TopPeriod::Month,
        TopPeriod::Year,
        TopPeriod::AllTime,
    ];

    /// The chip's words (the phone's past_24_hours / past_week / …).
    pub fn label(self) -> &'static str {
        match self {
            TopPeriod::Day => "24 часа",
            TopPeriod::Week => "Неделя",
            TopPeriod::Month => "Месяц",
            TopPeriod::Year => "Год",
            TopPeriod::AllTime => "Всё время",
        }
    }

    /// The phone's enum name (kept in the setting).
    pub fn name(self) -> &'static str {
        match self {
            TopPeriod::Day => "DAY",
            TopPeriod::Week => "WEEK",
            TopPeriod::Month => "MONTH",
            TopPeriod::Year => "YEAR",
            TopPeriod::AllTime => "ALL_TIME",
        }
    }

    pub fn from_name(s: &str) -> TopPeriod {
        TopPeriod::ALL
            .into_iter()
            .find(|p| p.name() == s)
            .unwrap_or_default()
    }

    /// Where the period starts (Unix ms); 0 = always.
    #[allow(clippy::wrong_self_convention)]
    fn from_ms(self) -> i64 {
        let day = 86_400_000i64;
        let now = crate::songs::now_ms();
        match self {
            TopPeriod::Day => now - day,
            TopPeriod::Week => now - 7 * day,
            TopPeriod::Month => now - 30 * day,
            TopPeriod::Year => now - 365 * day,
            TopPeriod::AllTime => 0,
        }
    }
}

impl ListSpec {
    /// Lists that live in memory, filled from a catalog over the network.
    pub fn is_remote(&self) -> bool {
        !matches!(
            self,
            ListSpec::LikedSongs
                | ListSpec::LibrarySongs
                | ListSpec::Playlist { .. }
                | ListSpec::History
                | ListSpec::RecentSongs
                | ListSpec::Album { .. }
                | ListSpec::Artist { .. }
                | ListSpec::Top { .. }
        )
    }

    /// Library panel id of a remote list: `sp:liked`, `sp:pl:<id>`, `ym:…`, `vk:…`, `al:…`, `ar:…`.
    pub fn entry_id(&self) -> Option<String> {
        Some(match self {
            ListSpec::SpotifyLiked => "sp:liked".into(),
            ListSpec::SpotifyPlaylist { id } => format!("sp:pl:{id}"),
            ListSpec::YandexLiked => "ym:liked".into(),
            ListSpec::YandexPlaylist { id } => format!("ym:pl:{id}"),
            ListSpec::VkMyMusic => "vk:liked".into(),
            ListSpec::VkPlaylist { id } => format!("vk:pl:{id}"),
            ListSpec::CatalogAlbum { id } => format!("al:{id}"),
            ListSpec::CatalogArtist { id } => format!("ar:{id}"),
            _ => return None,
        })
    }

    pub fn from_entry_id(s: &str) -> Option<ListSpec> {
        if let Some(id) = s.strip_prefix("al:") {
            return Some(ListSpec::CatalogAlbum { id: id.to_string() });
        }
        if let Some(id) = s.strip_prefix("ar:") {
            return Some(ListSpec::CatalogArtist { id: id.to_string() });
        }
        let (src, rest) = s.split_once(':')?;
        let pl = rest.strip_prefix("pl:").map(str::to_string);
        Some(match (src, rest, pl) {
            ("sp", "liked", _) => ListSpec::SpotifyLiked,
            ("ym", "liked", _) => ListSpec::YandexLiked,
            ("vk", "liked", _) => ListSpec::VkMyMusic,
            ("sp", _, Some(id)) => ListSpec::SpotifyPlaylist { id },
            ("ym", _, Some(id)) => ListSpec::YandexPlaylist { id },
            ("vk", _, Some(id)) => ListSpec::VkPlaylist { id },
            _ => return None,
        })
    }
}

impl ListSpec {
    /// Tables whose changes affect this list.
    fn tables(&self) -> &'static [&'static str] {
        match self {
            ListSpec::LikedSongs | ListSpec::LibrarySongs => &["song", "song_artist_map", "artist"],
            ListSpec::Playlist { .. } => {
                &["playlist_song_map", "song", "song_artist_map", "artist"]
            }
            ListSpec::History | ListSpec::RecentSongs | ListSpec::Top { .. } => {
                &["event", "song", "song_artist_map", "artist"]
            }
            ListSpec::Album { .. } => &["song_album_map", "song", "song_artist_map", "artist"],
            ListSpec::Artist { .. } => &["song_artist_map", "song", "artist"],
            _ => &[],
        }
    }
}

/// The heading of a page (album or artist): what the UI shows above the rows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListHeader {
    pub title: String,
    /// «Альбом · Исполнитель · 2021», «Исполнитель».
    pub subtitle: String,
    pub image: Option<String>,
    /// An artist's albums and singles, newest first (empty for other pages).
    pub releases: Vec<drx_catalog::model::Release>,
    /// An artist's «Участие» and similar artists.
    pub appears_on: Vec<drx_catalog::model::AlbumSummary>,
    pub related: Vec<drx_catalog::model::ArtistSummary>,
}

/// One song row as lists show it.
#[derive(Debug, Clone, PartialEq)]
pub struct SongRow {
    pub id: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub duration_s: i64,
    pub thumbnail: Option<String>,
    pub liked: bool,
    /// The first artist's id and the album's id, when known: rows link to those pages.
    pub artist_id: Option<String>,
    pub album_id: Option<String>,
    /// Spotify's play count (catalog album pages).
    pub plays: Option<u64>,
}

struct Entry {
    spec: ListSpec,
    version: u64,
}

#[derive(Default)]
pub(crate) struct Lists {
    next: AtomicU64,
    open: Mutex<HashMap<ListHandle, Entry>>,
}

impl Lists {
    pub fn open(&self, spec: ListSpec) -> ListHandle {
        let h = ListHandle(self.next.fetch_add(1, Ordering::Relaxed) + 1);
        self.open
            .lock()
            .expect("lists lock")
            .insert(h, Entry { spec, version: 0 });
        h
    }
    pub fn close(&self, h: ListHandle) {
        self.open.lock().expect("lists lock").remove(&h);
    }
    pub fn clear(&self) {
        self.open.lock().expect("lists lock").clear();
    }
    pub fn spec(&self, h: ListHandle) -> Option<ListSpec> {
        self.open
            .lock()
            .expect("lists lock")
            .get(&h)
            .map(|e| e.spec.clone())
    }
    /// Bumps the version of every list that depends on one of `tables`; returns them.
    pub fn touch(&self, tables: &[String]) -> Vec<(ListHandle, u64)> {
        let mut out = Vec::new();
        for (h, e) in self.open.lock().expect("lists lock").iter_mut() {
            if e.spec
                .tables()
                .iter()
                .any(|t| tables.iter().any(|c| c == t))
            {
                e.version += 1;
                out.push((*h, e.version));
            }
        }
        out
    }
}

const ARTISTS: &str = "(SELECT group_concat(a.name, ', ') FROM (SELECT ar.name FROM song_artist_map m JOIN artist ar ON ar.id = m.artistId WHERE m.songId = s.id ORDER BY m.position) a)";

const FIRST_ARTIST: &str =
    "(SELECT m.artistId FROM song_artist_map m WHERE m.songId = s.id ORDER BY m.position LIMIT 1)";

pub(crate) const SONG_COVER: &str = "coalesce(nullif(s.thumbnailUrl, ''), (SELECT nullif(al.thumbnailUrl, '') FROM album al WHERE al.id = s.albumId AND al.thumbnailUrl IS NOT NULL AND al.thumbnailUrl != '' LIMIT 1), (SELECT nullif(ar.thumbnailUrl, '') FROM song_artist_map sam JOIN artist ar ON ar.id = sam.artistId WHERE sam.songId = s.id AND ar.thumbnailUrl IS NOT NULL AND ar.thumbnailUrl != '' LIMIT 1), (SELECT nullif(ar2.thumbnailUrl, '') FROM song_artist_map sam JOIN artist ar ON ar.id = sam.artistId JOIN artist ar2 ON ar2.thumbnailUrl IS NOT NULL AND ar2.thumbnailUrl != '' AND (ar.name LIKE ar2.name || '%' OR ar2.name LIKE ar.name || '%') WHERE sam.songId = s.id LIMIT 1))";

/// The liked songs, one row per song. The phone often keeps one song twice: as the YouTube video it played
/// through and as its catalog row (`mfb:<Spotify id>`, older `spotify:<id>`). Rows are grouped by the Spotify track
/// behind them (the id itself, or the video's `spotify_match`); the most recently liked row stays. Videos without a
/// Spotify track and source tracks (VK, SoundCloud…) are their own songs.
pub(crate) const LIKED_UNIQUE: &str = "(SELECT * FROM (SELECT s0.*, ROW_NUMBER() OVER (PARTITION BY
     CASE WHEN s0.id LIKE 'mfb:%' AND length(s0.id) = 26
               AND instr(substr(s0.id, 5), ':') = 0 AND instr(substr(s0.id, 5), '_') = 0 THEN substr(s0.id, 5)
          WHEN s0.id LIKE 'spotify:%' AND length(s0.id) = 30 THEN substr(s0.id, 9)
          ELSE coalesce((SELECT m.spotifyId FROM spotify_match m WHERE m.youtubeId = s0.id
               ORDER BY m.isManualOverride DESC, m.matchScore DESC LIMIT 1), s0.id) END
     ORDER BY s0.likedDate DESC, s0.thumbnailUrl IS NULL) AS dup FROM song s0 WHERE s0.liked = 1) WHERE dup = 1)";

fn base(spec: &ListSpec) -> (String, Vec<String>) {
    match spec {
        ListSpec::LikedSongs => (format!("FROM {LIKED_UNIQUE} s ORDER BY s.likedDate DESC"), vec![]),
        ListSpec::LibrarySongs => ("FROM song s WHERE s.inLibrary IS NOT NULL ORDER BY s.title COLLATE NOCASE".into(), vec![]),
        ListSpec::Playlist { id } => (
            "FROM playlist_song_map p JOIN song s ON s.id = p.songId WHERE p.playlistId = ?1 ORDER BY p.position".into(),
            vec![id.clone()],
        ),
        ListSpec::History => ("FROM event e JOIN song s ON s.id = e.songId ORDER BY e.timestamp DESC".into(), vec![]),
        ListSpec::RecentSongs => (
            "FROM (SELECT songId, max(timestamp) AS last FROM event GROUP BY songId ORDER BY last DESC LIMIT 200) r              JOIN song s ON s.id = r.songId ORDER BY r.last DESC"
                .into(),
            vec![],
        ),
        ListSpec::Album { id } => (
            "FROM song_album_map m JOIN song s ON s.id = m.songId WHERE m.albumId = ?1 ORDER BY m.`index`".into(),
            vec![id.clone()],
        ),
        ListSpec::Artist { id } => (
            "FROM song_artist_map m JOIN song s ON s.id = m.songId WHERE m.artistId = ?1              ORDER BY s.totalPlayTime DESC, s.liked DESC, s.title COLLATE NOCASE"
                .into(),
            vec![id.clone()],
        ),
        ListSpec::Top { period, size } => (
            "FROM (SELECT songId, sum(playTime) AS ms FROM event WHERE timestamp >= ?1 GROUP BY songId ORDER BY ms DESC LIMIT ?2) t
             JOIN song s ON s.id = t.songId ORDER BY t.ms DESC"
                .into(),
            vec![period.from_ms().to_string(), size.to_string()],
        ),
        // Remote lists never reach SQL; an empty query keeps the match total.
        _ => ("FROM song s WHERE 0".into(), vec![]),
    }
}

pub(crate) fn count(c: &mut Connection, spec: &ListSpec) -> drx_core::Result<usize> {
    let (from, params) = base(spec);
    let sql = format!("SELECT count(*) {from}");
    let n: i64 = c.query_row(&sql, rusqlite::params_from_iter(params.iter()), |r| {
        r.get(0)
    })?;
    Ok(n as usize)
}

pub(crate) fn total_seconds(c: &mut Connection, spec: &ListSpec) -> drx_core::Result<i64> {
    let (from, params) = base(spec);
    // ORDER BY inside `from` is harmless for a sum.
    let sql = format!("SELECT coalesce(sum(max(s.duration, 0)), 0) {from}");
    Ok(
        c.query_row(&sql, rusqlite::params_from_iter(params.iter()), |r| {
            r.get(0)
        })?,
    )
}

pub(crate) fn rows(
    c: &mut Connection,
    spec: &ListSpec,
    range: std::ops::Range<usize>,
) -> drx_core::Result<Vec<SongRow>> {
    let (from, mut params) = base(spec);
    let n = params.len();
    let sql = format!(
        "SELECT s.id, s.title, {ARTISTS}, s.albumName, s.duration, {SONG_COVER}, s.liked, {FIRST_ARTIST}, s.albumId {from} LIMIT ?{} OFFSET ?{}",
        n + 1,
        n + 2
    );
    params.push(range.len().to_string());
    params.push(range.start.to_string());
    let mut st = c.prepare_cached(&sql)?;
    let out = st
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(SongRow {
                id: r.get(0)?,
                title: r.get(1)?,
                artists: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                album: r.get(3)?,
                duration_s: r.get(4)?,
                thumbnail: r.get(5)?,
                liked: r.get(6)?,
                artist_id: r.get(7)?,
                album_id: r.get(8)?,
                plays: None,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// Liked songs liked before `cut` (Unix ms) and not played since (home «Давно не слушали»).
pub(crate) fn forgotten(c: &mut Connection, cut: i64) -> drx_core::Result<Vec<SongRow>> {
    let sql = format!(
        "SELECT s.id, s.title, {ARTISTS}, s.albumName, s.duration, {SONG_COVER}, s.liked, {FIRST_ARTIST}, s.albumId          FROM song s WHERE s.liked = 1 AND s.likedDate > 0 AND s.likedDate < ?1          AND NOT EXISTS (SELECT 1 FROM event e WHERE e.songId = s.id AND e.timestamp > ?1) LIMIT 2000"
    );
    let mut st = c.prepare_cached(&sql)?;
    let out = st
        .query_map([cut], |r| {
            Ok(SongRow {
                id: r.get(0)?,
                title: r.get(1)?,
                artists: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                album: r.get(3)?,
                duration_s: r.get(4)?,
                thumbnail: r.get(5)?,
                liked: r.get(6)?,
                artist_id: r.get(7)?,
                album_id: r.get(8)?,
                plays: None,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// Every song of the library (liked or added), liked first (the library search walks them).
pub(crate) fn all_library_rows(c: &mut Connection) -> drx_core::Result<Vec<SongRow>> {
    let sql = format!(
        "SELECT s.id, s.title, {ARTISTS}, s.albumName, s.duration, {SONG_COVER}, s.liked, {FIRST_ARTIST}, s.albumId          FROM song s WHERE s.liked = 1 OR s.inLibrary IS NOT NULL ORDER BY s.liked DESC, s.totalPlayTime DESC"
    );
    let mut st = c.prepare_cached(&sql)?;
    let out = st
        .query_map([], |r| {
            Ok(SongRow {
                id: r.get(0)?,
                title: r.get(1)?,
                artists: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                album: r.get(3)?,
                duration_s: r.get(4)?,
                thumbnail: r.get(5)?,
                liked: r.get(6)?,
                artist_id: r.get(7)?,
                album_id: r.get(8)?,
                plays: None,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}
