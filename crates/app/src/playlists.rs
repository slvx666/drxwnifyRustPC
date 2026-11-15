//! The listener's own playlists (Android `PlaylistEntity` / `PlaylistSongMap` and the DAO's `move`): create,
//! rename, delete, add tracks, take one out, move one up or down. Positions stay 0..n-1 in order. The phone's
//! automatic lists (`LP_LIKED`, `LP_MONTHLY_MOST`…) and imported ones that are not editable are left alone.
//! Own Spotify playlists take tracks too (the web player's `addToPlaylist`).

use crate::songs::{ensure_song, now_ms};
use crate::{App, AppError, AppResult};
use drx_catalog::model::{Source, Track};
use rusqlite::{OptionalExtension, params};

/// A playlist a track can be put into («Добавить в плейлист…»).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistChoice {
    /// The database id (`LP…`), or `sp:<playlist id>` for an own Spotify playlist.
    pub id: String,
    pub name: String,
    pub count: i64,
    pub cover: Option<String>,
    /// Every one of the tracks asked about is in it already.
    pub has_all: bool,
}

/// id, name, entries, own cover, first track's cover.
type ChoiceRow = (String, String, i64, Option<String>, Option<String>);

/// A new playlist id, as `PlaylistEntity.generatePlaylistId()` ("LP" + 8 letters).
fn new_id() -> String {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut x = (now_ms() as u64).rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15 ^ std::process::id() as u64;
    let mut s = String::from("LP");
    for _ in 0..8 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.push(LETTERS[(x % LETTERS.len() as u64) as usize] as char);
    }
    s
}

/// Can the listener change this database playlist?
pub fn is_editable_id(id: &str) -> bool {
    id.starts_with("LP") && !id.starts_with("LP_")
}

fn err(e: impl std::fmt::Display) -> AppError {
    AppError::Other(e.to_string())
}

impl App {
    /// A new empty playlist; `name` empty gives «Новый плейлист N» (pc/). Returns its id.
    pub async fn create_playlist(&self, name: &str) -> AppResult<String> {
        let name = name.trim().to_string();
        Ok(self
            .store()
            .call(move |c| {
                let name = if name.is_empty() {
                    let n: i64 = c.query_row(
                        "SELECT count(*) FROM playlist WHERE id LIKE 'LP%' AND id NOT LIKE 'LP\\_%' ESCAPE '\\'",
                        [],
                        |r| r.get(0),
                    )?;
                    format!("Новый плейлист {}", n + 1)
                } else {
                    name
                };
                let id = new_id();
                let now = now_ms();
                c.execute(
                    "INSERT INTO playlist (id, name, createdAt, lastUpdateTime, isEditable) VALUES (?1, ?2, ?3, ?3, 1)",
                    params![id, name, now],
                )?;
                Ok(id)
            })
            .await?)
    }

    pub async fn rename_playlist(&self, id: &str, name: &str) -> AppResult<()> {
        let (id, name) = (id.to_string(), name.trim().to_string());
        if name.is_empty() {
            return Err(err("пустое название"));
        }
        Ok(self
            .store()
            .call(move |c| {
                c.execute(
                    "UPDATE playlist SET name = ?2, lastUpdateTime = ?3 WHERE id = ?1",
                    params![id, name, now_ms()],
                )?;
                Ok(())
            })
            .await?)
    }

    /// Deletes a playlist and its entries (the songs stay in the library).
    pub async fn delete_playlist(&self, id: &str) -> AppResult<()> {
        let id = id.to_string();
        Ok(self
            .store()
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute("DELETE FROM playlist_song_map WHERE playlistId = ?1", [&id])?;
                tx.execute("DELETE FROM playlist WHERE id = ?1", [&id])?;
                tx.commit()?;
                Ok(())
            })
            .await?)
    }

    /// Adds tracks at the end of a playlist (songs written into the library when they have no row). Returns how
    /// many were added; ones already in it are skipped.
    pub async fn add_to_playlist(&self, id: &str, tracks: Vec<Track>) -> AppResult<usize> {
        if let Some(sp_id) = id.strip_prefix("sp:") {
            return self.add_to_spotify_playlist(sp_id, &tracks).await;
        }
        let id = id.to_string();
        Ok(self
            .store()
            .call(move |c| {
                let tx = c.transaction()?;
                let mut pos: i64 = tx.query_row(
                    "SELECT coalesce(max(position) + 1, 0) FROM playlist_song_map WHERE playlistId = ?1",
                    [&id],
                    |r| r.get(0),
                )?;
                let mut added = 0;
                for t in &tracks {
                    let song = ensure_song(&tx, t)?;
                    let there: Option<i64> = tx
                        .query_row(
                            "SELECT id FROM playlist_song_map WHERE playlistId = ?1 AND songId = ?2",
                            params![id, song],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if there.is_some() {
                        continue;
                    }
                    tx.execute(
                        "INSERT INTO playlist_song_map (playlistId, songId, position) VALUES (?1, ?2, ?3)",
                        params![id, song, pos],
                    )?;
                    pos += 1;
                    added += 1;
                }
                tx.execute(
                    "UPDATE playlist SET lastUpdateTime = ?2 WHERE id = ?1",
                    params![id, now_ms()],
                )?;
                tx.commit()?;
                Ok(added)
            })
            .await?)
    }

    async fn add_to_spotify_playlist(&self, playlist_id: &str, tracks: &[Track]) -> AppResult<usize> {
        let sp = self.spotify().ok_or_else(|| err("нет входа в Spotify"))?;
        let mut uris = Vec::new();
        for t in tracks {
            let song = crate::songs::song_id_of(&t.id);
            let cid = self
                .store()
                .call(move |c| Ok(crate::songs::catalog_id_of(c, &song)))
                .await
                .ok()
                .flatten();
            match cid.filter(|c| Source::of_id(c) == Source::Spotify && crate::songs::is_spotify_id(c)) {
                Some(c) => uris.push(format!("spotify:track:{c}")),
                None => return Err(err(format!("«{}» нет в Spotify", t.title))),
            }
        }
        sp.add_to_playlist(playlist_id, &uris).await.map_err(err)?;
        Ok(uris.len())
    }

    /// Takes the entry at `position` out of a playlist; the ones after it move up.
    pub async fn remove_from_playlist(&self, id: &str, position: i64) -> AppResult<()> {
        let id = id.to_string();
        Ok(self
            .store()
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute(
                    "DELETE FROM playlist_song_map WHERE playlistId = ?1 AND position = ?2",
                    params![id, position],
                )?;
                tx.execute(
                    "UPDATE playlist_song_map SET position = position - 1 WHERE playlistId = ?1 AND position > ?2",
                    params![id, position],
                )?;
                tx.execute(
                    "UPDATE playlist SET lastUpdateTime = ?2 WHERE id = ?1",
                    params![id, now_ms()],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?)
    }

    /// Moves an entry from one position to another (DatabaseDao.move); the ones between shift by one.
    pub async fn move_in_playlist(&self, id: &str, from: i64, to: i64) -> AppResult<()> {
        if from == to {
            return Ok(());
        }
        let id = id.to_string();
        Ok(self
            .store()
            .call(move |c| {
                let tx = c.transaction()?;
                tx.execute(
                    "UPDATE playlist_song_map SET position =
                         CASE WHEN position = ?2 THEN ?3
                              WHEN ?2 < ?3 THEN position - 1
                              ELSE position + 1 END
                     WHERE playlistId = ?1 AND position BETWEEN min(?2, ?3) AND max(?2, ?3)",
                    params![id, from, to],
                )?;
                tx.execute(
                    "UPDATE playlist SET lastUpdateTime = ?2 WHERE id = ?1",
                    params![id, now_ms()],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?)
    }

    /// The playlists a track can go into, newest change first: own database playlists, then own Spotify ones.
    pub async fn playlist_choices(&self, track_ids: Vec<String>) -> Vec<PlaylistChoice> {
        let songs: Vec<String> = track_ids.iter().map(|t| crate::songs::song_id_of(t)).collect();
        let covers = self.inner.profile.covers_dir();
        let mut out = self
            .store()
            .call(move |c| {
                let mut st = c.prepare(&format!(
                    "SELECT p.id, p.name, (SELECT count(*) FROM playlist_song_map m WHERE m.playlistId = p.id),
                            p.thumbnailUrl, {first}
                     FROM playlist p
                     WHERE p.id LIKE 'LP%' AND p.id NOT LIKE 'LP\\_%' ESCAPE '\\' AND p.isEditable = 1
                     ORDER BY coalesce(p.lastUpdateTime, p.createdAt, 0) DESC",
                    first = crate::PLAYLIST_FIRST_COVER
                ))?;
                let rows: Vec<ChoiceRow> = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                    .flatten()
                    .collect();
                let mut has = c.prepare("SELECT EXISTS(SELECT 1 FROM playlist_song_map WHERE playlistId = ?1 AND songId = ?2)")?;
                Ok(rows
                    .into_iter()
                    .map(|(id, name, count, own, first)| {
                        let has_all = !songs.is_empty()
                            && songs.iter().all(|s| has.query_row(params![id, s], |r| r.get::<_, bool>(0)).unwrap_or(false));
                        PlaylistChoice {
                            cover: crate::playlist_cover(&covers, own, first),
                            id,
                            name,
                            count,
                            has_all,
                        }
                    })
                    .collect::<Vec<_>>())
            })
            .await
            .unwrap_or_default();
        out.extend(self.own_spotify_playlists().await);
        out
    }

    /// The listener's own Spotify playlists (from the library panel's account lists).
    async fn own_spotify_playlists(&self) -> Vec<PlaylistChoice> {
        let Some(sp) = self.spotify() else {
            return Vec::new();
        };
        let Ok(me) = sp.me().await else {
            return Vec::new();
        };
        let me_name = me.name.unwrap_or_default();
        let Ok(page) = sp.my_playlists(0, 50).await else {
            return Vec::new();
        };
        page.items
            .into_iter()
            .filter(|p| !me_name.is_empty() && p.owner.as_deref() == Some(me_name.as_str()))
            .map(|p| PlaylistChoice {
                id: format!("sp:{}", p.id),
                cover: drx_catalog::model::best_image(&p.images, 120).map(|i| i.url.clone()),
                count: p.total as i64,
                name: p.name,
                has_all: false,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditionProfile;
    use drx_catalog::model::ArtistRef;
    use drx_core::paths::Profile;

    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: format!("Song {id}"),
            artists: vec![ArtistRef { id: String::new(), name: "Band".into() }],
            album: None,
            duration_ms: 180_000,
            isrc: None,
            explicit: false,
            plays: None,
        }
    }

    #[test]
    fn edit_a_playlist() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::start(Profile::at(dir.path()), EditionProfile::mini()).unwrap();
        let rt = app.runtime();
        let id = rt.block_on(app.create_playlist("")).unwrap();
        assert!(is_editable_id(&id));
        let tracks = vec![track("a"), track("b"), track("c"), track("d")];
        assert_eq!(rt.block_on(app.add_to_playlist(&id, tracks.clone())).unwrap(), 4);
        // Already there: skipped.
        assert_eq!(rt.block_on(app.add_to_playlist(&id, vec![track("b")])).unwrap(), 0);
        let order = |app: &App| {
            let id = id.clone();
            app.runtime()
                .block_on(app.store().call(move |c| {
                    let mut st = c.prepare("SELECT songId, position FROM playlist_song_map WHERE playlistId = ?1 ORDER BY position")?;
                    let v: Vec<(String, i64)> = st.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?.flatten().collect();
                    Ok(v)
                }))
                .unwrap()
        };
        let ids = |v: Vec<(String, i64)>| v.into_iter().map(|(s, _)| s.trim_start_matches("mfb:").to_string()).collect::<Vec<_>>();
        assert_eq!(ids(order(&app)), ["a", "b", "c", "d"]);
        rt.block_on(app.move_in_playlist(&id, 3, 0)).unwrap();
        assert_eq!(ids(order(&app)), ["d", "a", "b", "c"]);
        rt.block_on(app.move_in_playlist(&id, 1, 2)).unwrap();
        assert_eq!(ids(order(&app)), ["d", "b", "a", "c"]);
        rt.block_on(app.remove_from_playlist(&id, 1)).unwrap();
        let after = order(&app);
        assert_eq!(after.iter().map(|(_, p)| *p).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(ids(after), ["d", "a", "c"]);
        rt.block_on(app.rename_playlist(&id, "Мой")).unwrap();
        let choices = rt.block_on(app.playlist_choices(vec!["a".into()]));
        assert_eq!(choices.len(), 1);
        assert_eq!((choices[0].name.as_str(), choices[0].count, choices[0].has_all), ("Мой", 3, true));
        rt.block_on(app.delete_playlist(&id)).unwrap();
        assert!(rt.block_on(app.playlist_choices(Vec::new())).is_empty());
    }

    #[test]
    fn editable_ids() {
        assert!(is_editable_id("LPjRBmJyTs"));
        assert!(!is_editable_id("LP_MONTHLY_MOST"));
        assert!(!is_editable_id("RDCLAK5uy"));
    }
}
