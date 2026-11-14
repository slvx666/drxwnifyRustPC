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
    Playlist { id: String },
    /// Listening history, newest first.
    History,
    /// VK «Моя музыка», loaded from VK (not stored in the database).
    VkMyMusic,
    /// A VK album or playlist (`vk_<owner>_<id>[_<key>]`), loaded from VK.
    VkPlaylist { id: String },
}

impl ListSpec {
    /// Lists that live in memory, filled from a catalog over the network.
    pub fn is_remote(&self) -> bool {
        matches!(self, ListSpec::VkMyMusic | ListSpec::VkPlaylist { .. })
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
            ListSpec::History => &["event", "song", "song_artist_map", "artist"],
            ListSpec::VkMyMusic | ListSpec::VkPlaylist { .. } => &[],
        }
    }
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

fn base(spec: &ListSpec) -> (String, Vec<String>) {
    match spec {
        ListSpec::LikedSongs => ("FROM song s WHERE s.liked = 1 ORDER BY s.likedDate DESC".into(), vec![]),
        ListSpec::LibrarySongs => ("FROM song s WHERE s.inLibrary IS NOT NULL ORDER BY s.title COLLATE NOCASE".into(), vec![]),
        ListSpec::Playlist { id } => (
            "FROM playlist_song_map p JOIN song s ON s.id = p.songId WHERE p.playlistId = ?1 ORDER BY p.position".into(),
            vec![id.clone()],
        ),
        ListSpec::History => ("FROM event e JOIN song s ON s.id = e.songId ORDER BY e.timestamp DESC".into(), vec![]),
        // Remote lists never reach SQL; an empty query keeps the match total.
        ListSpec::VkMyMusic | ListSpec::VkPlaylist { .. } => ("FROM song s WHERE 0".into(), vec![]),
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

pub(crate) fn rows(
    c: &mut Connection,
    spec: &ListSpec,
    range: std::ops::Range<usize>,
) -> drx_core::Result<Vec<SongRow>> {
    let (from, mut params) = base(spec);
    let n = params.len();
    let sql = format!(
        "SELECT s.id, s.title, {ARTISTS}, s.albumName, s.duration, s.thumbnailUrl, s.liked {from} LIMIT ?{} OFFSET ?{}",
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
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}
