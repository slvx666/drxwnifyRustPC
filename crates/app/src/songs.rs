//! Songs written into the library the way Android's `DatabaseDao.insert(MediaMetadata)` does it, so a listen, a
//! like or a playlist entry always has a row to point at (Android lost listens on the foreign key without one).
//!
//! Ids (Android `FallbackIds`, `SpotifyYouTubeMapper.buildMediaMetadata`, `artistEntityFor`):
//! - a catalog track is the row `mfb:<catalog id>` (Spotify id, `ym_…`, `src:VK:…`); a library row keeps its id;
//! - album id `spotify:<album id>` (Yandex albums too, as on the phone);
//! - a catalog artist is `SP_<id>` with `spotifyId = <id>` (Spotify and Yandex ids), found again by `spotifyId`
//!   or by name; any other artist keeps its id or gets a new local one (`LA` + 8 letters).

use drx_catalog::model::Track;
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const FALLBACK_PREFIX: &str = "mfb:";

/// YouTube video ids are 11 characters of `[A-Za-z0-9_-]` (library songs from YouTube Music use them as ids).
pub(crate) fn is_youtube_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A bare Spotify id (22 base-62 characters).
pub(crate) fn is_spotify_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The song row id of a track: a library row keeps its own id, a catalog track is `mfb:<catalog id>`.
pub fn song_id_of(track_id: &str) -> String {
    if track_id.starts_with(FALLBACK_PREFIX)
        || track_id.starts_with("qbzfb:")
        || track_id.starts_with("spotify:")
        || is_youtube_video_id(track_id)
    {
        track_id.to_string()
    } else {
        format!("{FALLBACK_PREFIX}{track_id}")
    }
}

/// The catalog track behind a song row, from the id alone: `mfb:<id>` and the older `spotify:<id>`.
/// A YouTube video needs the database ([`catalog_id_of`]).
pub fn catalog_id_fast(song_id: &str) -> Option<String> {
    if let Some(rest) = song_id.strip_prefix(FALLBACK_PREFIX) {
        return Some(rest.to_string());
    }
    if let Some(rest) = song_id.strip_prefix("spotify:")
        && is_spotify_id(rest)
    {
        return Some(rest.to_string());
    }
    None
}

/// The catalog track behind a song row; a YouTube video through its Spotify match (Android `LikeTarget.catalogId`).
pub(crate) fn catalog_id_of(c: &Connection, song_id: &str) -> Option<String> {
    if let Some(id) = catalog_id_fast(song_id) {
        return Some(id);
    }
    if !is_youtube_video_id(song_id) {
        return None;
    }
    c.query_row(
        "SELECT spotifyId FROM spotify_match WHERE youtubeId = ?1
         ORDER BY isManualOverride DESC, matchScore DESC LIMIT 1",
        [song_id],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A new local artist id, as `ArtistEntity.generateArtistId()` ("LA" + 8 letters).
fn local_artist_id() -> String {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut x = now_ms() as u64 ^ (std::process::id() as u64).rotate_left(32);
    let mut s = String::from("LA");
    for _ in 0..8 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.push(LETTERS[(x % LETTERS.len() as u64) as usize] as char);
    }
    s
}

/// The artist row for a track's artist (`artistEntityFor`): inserted when new, its id returned.
fn artist_row(c: &Connection, id: &str, name: &str) -> rusqlite::Result<String> {
    let catalog = if is_spotify_id(id) || id.starts_with(drx_catalog::yandex::PREFIX) {
        Some(id)
    } else {
        id.strip_prefix("SP_")
    };
    if let Some(raw) = catalog.filter(|r| !r.is_empty()) {
        if let Some(found) = c
            .query_row(
                "SELECT id FROM artist WHERE spotifyId = ?1 LIMIT 1",
                [raw],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(found);
        }
        // The same name without a catalog link: linked now (Android does the same).
        if let Some(found) = c
            .query_row(
                "SELECT id FROM artist WHERE name = ?1 AND spotifyId IS NULL LIMIT 1",
                [name],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            c.execute(
                "UPDATE artist SET spotifyId = ?1 WHERE id = ?2",
                params![raw, found],
            )?;
            return Ok(found);
        }
        let new_id = format!("SP_{raw}");
        c.execute(
            "INSERT OR IGNORE INTO artist (id, name, lastUpdateTime, spotifyId) VALUES (?1, ?2, ?3, ?4)",
            params![new_id, name, now_ms(), raw],
        )?;
        return Ok(new_id);
    }
    let existing = if id.is_empty() {
        c.query_row(
            "SELECT id FROM artist WHERE name = ?1 LIMIT 1",
            [name],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    } else {
        Some(id.to_string())
    };
    let row_id = existing.unwrap_or_else(local_artist_id);
    let channel = (!id.is_empty()).then_some(id);
    c.execute(
        "INSERT OR IGNORE INTO artist (id, name, lastUpdateTime, channelId) VALUES (?1, ?2, ?3, ?4)",
        params![row_id, name, now_ms(), channel],
    )?;
    Ok(row_id)
}

/// Makes sure `t` has a song row (with its artists) and returns the row's id. An existing row is left as it is.
pub(crate) fn ensure_song(c: &Connection, t: &Track) -> rusqlite::Result<String> {
    let id = song_id_of(&t.id);
    let cover = t
        .album
        .as_ref()
        .and_then(|a| drx_catalog::model::best_image(&a.images, 300))
        .map(|i| i.url.clone());
    let (album_id, album_name) = match &t.album {
        Some(a) if !a.id.is_empty() => (
            Some(if a.id.starts_with("spotify:") {
                a.id.clone()
            } else {
                format!("spotify:{}", a.id)
            }),
            Some(a.name.clone()),
        ),
        Some(a) => (None, Some(a.name.clone())),
        None => (None, None),
    };
    let written = c.execute(
        "INSERT OR IGNORE INTO song (id, title, duration, thumbnailUrl, albumId, albumName, explicit, liked,
             totalPlayTime, isrc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, ?8)",
        params![
            id,
            t.title,
            (t.duration_ms / 1000) as i64,
            cover,
            album_id,
            album_name,
            t.explicit,
            t.isrc
        ],
    )?;
    if written == 0 {
        return Ok(id);
    }
    for (pos, a) in t.artists.iter().filter(|a| !a.name.is_empty()).enumerate() {
        let artist = artist_row(c, &a.id, &a.name)?;
        c.execute(
            "INSERT OR IGNORE INTO song_artist_map (songId, artistId, position) VALUES (?1, ?2, ?3)",
            params![id, artist, pos as i64],
        )?;
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use drx_catalog::model::{AlbumRef, ArtistRef, Image};

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for st in drx_core::db::schema_statements().unwrap() {
            c.execute_batch(&st).unwrap();
        }
        c
    }

    fn track(id: &str, artist_id: &str) -> Track {
        Track {
            id: id.into(),
            title: "Song".into(),
            artists: vec![ArtistRef {
                id: artist_id.into(),
                name: "Band".into(),
            }],
            album: Some(AlbumRef {
                id: "4HXEAKXVGlx5sTryGadzHO".into(),
                name: "Album".into(),
                images: vec![Image {
                    url: "https://i.scdn.co/image/300".into(),
                    width: Some(300),
                }],
            }),
            duration_ms: 250_400,
            isrc: None,
            explicit: false,
            plays: None,
        }
    }

    #[test]
    fn ids_follow_android() {
        assert_eq!(
            song_id_of("0tu9Ra97hNCaD20tw93xQu"),
            "mfb:0tu9Ra97hNCaD20tw93xQu"
        );
        assert_eq!(song_id_of("ym_123"), "mfb:ym_123");
        assert_eq!(song_id_of("src:VK:1_2_k"), "mfb:src:VK:1_2_k");
        assert_eq!(song_id_of("mfb:ym_123"), "mfb:ym_123");
        assert_eq!(song_id_of("V2pw1F8G_xQ"), "V2pw1F8G_xQ");
        assert_eq!(
            catalog_id_fast("spotify:0tu9Ra97hNCaD20tw93xQu").as_deref(),
            Some("0tu9Ra97hNCaD20tw93xQu")
        );
        assert_eq!(catalog_id_fast("V2pw1F8G_xQ"), None);
    }

    #[test]
    fn catalog_track_gets_row_and_artist_once() {
        let c = db();
        let id = ensure_song(
            &c,
            &track("0tu9Ra97hNCaD20tw93xQu", "7aPT87bbFE2IO9d3qpzf9U"),
        )
        .unwrap();
        assert_eq!(id, "mfb:0tu9Ra97hNCaD20tw93xQu");
        let (dur, album, cover): (i64, String, String) = c
            .query_row(
                "SELECT duration, albumId, thumbnailUrl FROM song WHERE id = ?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (dur, album.as_str(), cover.as_str()),
            (
                250,
                "spotify:4HXEAKXVGlx5sTryGadzHO",
                "https://i.scdn.co/image/300"
            )
        );
        let artist: String = c
            .query_row(
                "SELECT artistId FROM song_artist_map WHERE songId = ?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(artist, "SP_7aPT87bbFE2IO9d3qpzf9U");
        // A second track by the same artist reuses the row; a second call changes nothing.
        ensure_song(
            &c,
            &track("1tu9Ra97hNCaD20tw93xQu", "7aPT87bbFE2IO9d3qpzf9U"),
        )
        .unwrap();
        ensure_song(
            &c,
            &track("0tu9Ra97hNCaD20tw93xQu", "7aPT87bbFE2IO9d3qpzf9U"),
        )
        .unwrap();
        let n: i64 = c
            .query_row("SELECT count(*) FROM artist", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn artist_by_name_gets_linked() {
        let c = db();
        c.execute(
            "INSERT INTO artist (id, name, lastUpdateTime) VALUES ('UCx', 'Band', 0)",
            [],
        )
        .unwrap();
        ensure_song(&c, &track("ym_5", "ym_7")).unwrap();
        let (id, sp): (String, String) = c
            .query_row("SELECT id, spotifyId FROM artist", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((id.as_str(), sp.as_str()), ("UCx", "ym_7"));
    }
}
