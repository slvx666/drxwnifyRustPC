//! Radio from a track, as Android's `SimilarArtistsRadio` (what `SpotifyQueue` plays first): popular songs of the
//! artists the catalog lists as similar to the track's artist, taken round-robin (three each, so no artist plays
//! ten songs in a row), with one song of the artist itself after every four. Songs heard in the last three days
//! (`RecentlyPlayed`) wait at the back and come in only when fewer than eight new ones were found. With fewer
//! than ten songs from similar artists the artist's own popular songs and the rest of the album are used
//! (`SpotifyQueue.buildFallbackQueue`).

use crate::playback::Playable;
use crate::songs::{catalog_id_of, is_spotify_id, song_id_of};
use crate::{App, AppError, AppResult};
use drx_catalog::model::{ArtistPage, Source, Track};
use std::collections::HashSet;
use std::sync::Arc;

const RELATED_ARTISTS: usize = 12;
const TRACKS_PER_RELATED_ARTIST: usize = 3;
const SEED_ARTIST_TRACKS: usize = 6;
const SEED_EVERY: usize = 4;
const MIN_FRESH: usize = 8;
const MIN_SIMILAR_TRACKS: usize = 10;
const RECENT_DAYS: i64 = 3;
pub const RADIO_LENGTH: usize = 50;

/// RecentlyPlayed.keyOf: "first artist|title" in lower case, a bracketed tail cut.
pub fn recent_key(artist: Option<&str>, title: &str) -> String {
    let s = format!("{}|{title}", artist.unwrap_or("")).to_lowercase();
    // "\s*[(\[].*" removed: everything from the first bracket on, with the spaces before it.
    let end = s.find(['(', '[']).unwrap_or(s.len());
    format!("k:{}", s[..end].trim_end().trim())
}

fn key_of(t: &Track) -> String {
    recent_key(t.artists.first().map(|a| a.name.as_str()), &t.title)
}

fn shuffled<T>(mut v: Vec<T>) -> Vec<T> {
    crate::playback::shuffle_in_place(&mut v);
    v
}

/// The round-robin order of SimilarArtistsRadio.build: `related_tops` are the similar artists' popular songs
/// (already shuffled per artist), `seed_picks` the artist's own. `recent` holds the keys heard lately.
pub fn arrange(
    seed: &Track,
    related_tops: Vec<Vec<Track>>,
    mut seed_picks: Vec<Track>,
    recent: &HashSet<String>,
    limit: usize,
) -> Vec<Track> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut heard: Vec<Track> = Vec::new();
    let seed_key = key_of(seed);
    let mut accept = |t: &Track, heard: &mut Vec<Track>| -> bool {
        let k = key_of(t);
        if t.id.is_empty() || t.id == seed.id || k == seed_key {
            return false;
        }
        if seen.contains(&t.id) || seen.contains(&k) {
            return false;
        }
        seen.insert(t.id.clone());
        seen.insert(k.clone());
        if recent.contains(&k) {
            heard.push(t.clone());
            return false;
        }
        true
    };
    let per_artist: Vec<Vec<Track>> = related_tops
        .into_iter()
        .map(|t| t.into_iter().take(TRACKS_PER_RELATED_ARTIST).collect())
        .collect();
    let mut order = Vec::new();
    for round in 0..TRACKS_PER_RELATED_ARTIST {
        for picks in &per_artist {
            if let Some(t) = picks.get(round) {
                order.push(t.clone());
            }
        }
    }
    let mut out = Vec::new();
    let mut since_seed = 0;
    for t in order {
        if out.len() >= limit {
            break;
        }
        if since_seed >= SEED_EVERY && !seed_picks.is_empty() {
            let s = seed_picks.remove(0);
            if accept(&s, &mut heard) {
                out.push(s);
            }
            since_seed = 0;
        }
        if accept(&t, &mut heard) {
            out.push(t);
            since_seed += 1;
        }
    }
    for s in seed_picks {
        if out.len() >= limit {
            break;
        }
        if accept(&s, &mut heard) {
            out.push(s);
        }
    }
    if out.len() < MIN_FRESH {
        for t in heard {
            if out.len() >= MIN_FRESH {
                break;
            }
            out.push(t);
        }
    }
    out
}

fn err(e: impl std::fmt::Display) -> AppError {
    AppError::Other(e.to_string())
}

impl App {
    fn spotify_or_anonymous(&self) -> Arc<drx_catalog::spotify::Spotify> {
        self.spotify().unwrap_or_else(|| {
            Arc::new(drx_catalog::spotify::Spotify::new(
                self.inner.http.clone(),
                None,
                None,
            ))
        })
    }

    /// An artist's page from its catalog (similar artists included; Yandex lists them separately).
    async fn artist_for_radio(&self, id: &str) -> AppResult<ArtistPage> {
        match Source::of_id(id) {
            Source::Spotify => self.spotify_or_anonymous().artist(id).await.map_err(err),
            Source::Yandex => {
                let y = self.yandex_any();
                let mut page = y.artist(id).await.map_err(err)?;
                if page.related.is_empty() {
                    page.related = y.similar_artists(id).await.unwrap_or_default();
                }
                Ok(page)
            }
            Source::Vk => {
                let vk = self.vk().ok_or_else(|| err("нет входа во ВКонтакте"))?;
                let (mut page, audios) = vk.artist(id).await.map_err(err)?;
                page.top_tracks = audios.into_iter().map(|a| a.track).collect();
                Ok(page)
            }
        }
    }

    /// The catalog artist to start from: the track's first artist id (a library `SP_` row means its Spotify id),
    /// else the Spotify artist of that name.
    async fn seed_artist(&self, t: &Track) -> Option<String> {
        let a = t.artists.first()?;
        let id = a.id.strip_prefix("SP_").unwrap_or(&a.id);
        if is_spotify_id(id)
            || id.starts_with(drx_catalog::yandex::PREFIX)
            || id.starts_with("vkar_")
        {
            return Some(id.to_string());
        }
        // A library artist without a catalog id: its spotifyId, or the Spotify artist of the same name.
        let (row, name) = (a.id.clone(), a.name.clone());
        let linked: Option<String> = self
            .store()
            .call(move |c| {
                Ok(c.query_row(
                    "SELECT spotifyId FROM artist WHERE id = ?1 AND spotifyId IS NOT NULL",
                    [&row],
                    |r| r.get(0),
                )
                .ok())
            })
            .await
            .ok()
            .flatten();
        if linked.is_some() {
            return linked;
        }
        let wanted = crate::likes::cross_norm(&name);
        let results = self
            .spotify_or_anonymous()
            .search_artists(&name, 10)
            .await
            .ok()?;
        results
            .iter()
            .find(|x| crate::likes::cross_norm(&x.name) == wanted)
            .map(|x| x.id.clone())
            .or_else(|| results.into_iter().next().map(|x| x.id))
    }

    /// Keys of what was heard in the last three days.
    async fn recent_keys(&self) -> HashSet<String> {
        let since = crate::songs::now_ms() - RECENT_DAYS * 86_400_000;
        self.store()
            .call(move |c| {
                let mut st = c.prepare(
                    "SELECT (SELECT a.name FROM song_artist_map m JOIN artist a ON a.id = m.artistId
                             WHERE m.songId = s.id ORDER BY m.position LIMIT 1), s.title
                     FROM event e JOIN song s ON s.id = e.songId WHERE e.timestamp >= ?1",
                )?;
                let keys = st
                    .query_map([since], |r| {
                        Ok(recent_key(
                            r.get::<_, Option<String>>(0)?.as_deref(),
                            &r.get::<_, String>(1)?,
                        ))
                    })?
                    .flatten()
                    .collect();
                Ok(keys)
            })
            .await
            .unwrap_or_default()
    }

    /// Up to [`RADIO_LENGTH`] tracks to follow `seed` (see the module notes).
    pub async fn radio_tracks(&self, seed: &Track) -> AppResult<Vec<Track>> {
        let artist = self
            .seed_artist(seed)
            .await
            .ok_or_else(|| err("у трека нет исполнителя в каталоге"))?;
        let page = self.artist_for_radio(&artist).await?;
        let related: Vec<String> = page
            .related
            .iter()
            .filter(|r| !r.id.is_empty() && r.id != artist)
            .take(RELATED_ARTISTS)
            .map(|r| r.id.clone())
            .collect();
        let mut set = tokio::task::JoinSet::new();
        for (i, id) in related.iter().enumerate() {
            let (app, id) = (self.clone(), id.clone());
            set.spawn(async move {
                let tops = app
                    .artist_for_radio(&id)
                    .await
                    .map(|p| p.top_tracks)
                    .unwrap_or_default();
                (i, tops)
            });
        }
        let mut tops: Vec<(usize, Vec<Track>)> = Vec::new();
        while let Some(r) = set.join_next().await {
            if let Ok(x) = r {
                tops.push(x);
            }
        }
        tops.sort_by_key(|(i, _)| *i);
        let related_tops: Vec<Vec<Track>> = tops
            .into_iter()
            .map(|(_, t)| shuffled(t.into_iter().take(8).collect()))
            .collect();
        let seed_picks: Vec<Track> = shuffled(page.top_tracks.clone())
            .into_iter()
            .take(SEED_ARTIST_TRACKS)
            .collect();
        let recent = self.recent_keys().await;
        let mut out = arrange(seed, related_tops, seed_picks, &recent, RADIO_LENGTH);
        if out.len() < MIN_SIMILAR_TRACKS {
            // The artist's songs and the rest of the album, shuffled.
            let mut seen: HashSet<String> = out.iter().map(|t| t.id.clone()).collect();
            seen.insert(seed.id.clone());
            let mut more: Vec<Track> = page
                .top_tracks
                .into_iter()
                .filter(|t| seen.insert(t.id.clone()))
                .collect();
            if let Some(album) = seed.album.as_ref().filter(|a| !a.id.is_empty())
                && Source::of_id(&album.id) != Source::Vk
            {
                let album_id = album.id.strip_prefix("spotify:").unwrap_or(&album.id);
                let tracks = match Source::of_id(album_id) {
                    Source::Yandex => self
                        .yandex_any()
                        .album(album_id)
                        .await
                        .map(|a| a.tracks)
                        .ok(),
                    _ if is_spotify_id(album_id) => self
                        .spotify_or_anonymous()
                        .album(album_id)
                        .await
                        .map(|a| a.tracks)
                        .ok(),
                    _ => None,
                };
                more.extend(
                    tracks
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|t| seen.insert(t.id.clone())),
                );
            }
            out.extend(shuffled(more));
            out.truncate(RADIO_LENGTH);
        }
        Ok(out)
    }

    /// Radio from the playing track without cutting it off: what was queued after it is replaced. Returns how
    /// many tracks were lined up.
    pub async fn radio_after_current(&self) -> AppResult<usize> {
        let seed = self
            .inner
            .playback
            .now
            .borrow()
            .track
            .clone()
            .ok_or_else(|| err("ничего не играет"))?;
        // A library row's catalog track gives a better seed (its artist ids are the catalog's).
        let mut start = seed.clone();
        let song = song_id_of(&seed.id);
        if let Some(cid) = self
            .store()
            .call(move |c| Ok(catalog_id_of(c, &song)))
            .await
            .ok()
            .flatten()
            && let Some(t) = self.inner.known.lock().expect("known").get(&cid).cloned()
        {
            start = t;
        }
        let tracks = self.radio_tracks(&start).await?;
        let n = tracks.len();
        if n > 0 {
            self.remember(tracks.iter());
            let items: Vec<Playable> = tracks
                .into_iter()
                .map(|track| Playable { track, url: None })
                .collect();
            self.inner.playback.replace_upcoming(&seed.id, items);
        }
        Ok(n)
    }

    /// «Играть дальше» (`next`) or «Добавить в очередь». An empty queue starts playing it.
    pub fn enqueue_track(&self, track: Track, next: bool) {
        let idle = self.inner.playback.now.borrow().track.is_none();
        self.remember(std::iter::once(&track));
        if idle {
            self.play_tracks(vec![track], 0);
        } else {
            self.inner
                .playback
                .enqueue(Playable { track, url: None }, next);
        }
    }

    /// The endless queue (Android autoLoadMore, on by default): when the last track of the queue starts and nothing
    /// repeats, its radio is added after it, so the music goes on. Not while following a room or a friend.
    pub fn keep_queue_going(&self) {
        let app = self.clone();
        self.runtime.spawn(async move {
            let mut rx = app.now_playing();
            let mut done_for: Option<String> = None;
            while rx.changed().await.is_ok() {
                let (track, index, repeat) = {
                    let n = rx.borrow();
                    (n.track.clone(), n.index, n.repeat)
                };
                let Some(track) = track else { continue };
                let (len, _) = {
                    let (q, i) = app.queue();
                    (q.len(), i)
                };
                let last = index.is_some_and(|i| i + 1 >= len);
                if !last
                    || repeat != crate::playback::Repeat::Off
                    || done_for.as_deref() == Some(track.id.as_str())
                {
                    continue;
                }
                if app.pref_bool("autoLoadMore") == Some(false)
                    || app.listening_along().is_some()
                    || app
                        .room()
                        .is_some_and(|r| r.borrow().role == drx_together::Role::Guest)
                {
                    continue;
                }
                done_for = Some(track.id.clone());
                if let Ok(more) = app.radio_tracks(&track).await {
                    let fresh: Vec<Playable> = more
                        .into_iter()
                        .filter(|t| t.id != track.id)
                        .map(|t| Playable {
                            track: t,
                            url: None,
                        })
                        .collect();
                    app.remember(fresh.iter().map(|p| &p.track));
                    app.inner.playback.append(fresh);
                }
            }
        });
    }

    /// An endless start from one track: it plays, its radio follows.
    pub async fn play_radio(&self, seed: Track) -> AppResult<usize> {
        self.play_tracks(vec![seed], 0);
        self.radio_after_current().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drx_catalog::model::ArtistRef;

    fn t(id: &str, artist: &str) -> Track {
        Track {
            id: id.into(),
            title: format!("song {id}"),
            artists: vec![ArtistRef {
                id: String::new(),
                name: artist.into(),
            }],
            album: None,
            duration_ms: 200_000,
            isrc: None,
            explicit: false,
            plays: None,
        }
    }

    #[test]
    fn keys_like_android() {
        assert_eq!(
            recent_key(Some("Band"), "Song (Live) [2020]"),
            "k:band|song"
        );
        assert_eq!(recent_key(None, "Song"), "k:|song");
    }

    #[test]
    fn round_robin_with_seed_every_four() {
        let seed = t("s0", "Seed");
        let related = vec![
            vec![t("a1", "A"), t("a2", "A"), t("a3", "A")],
            vec![t("b1", "B"), t("b2", "B"), t("b3", "B")],
            vec![t("c1", "C"), t("c2", "C"), t("c3", "C")],
        ];
        let seed_picks = vec![t("s1", "Seed"), t("s2", "Seed")];
        let out = arrange(&seed, related, seed_picks, &HashSet::new(), 50);
        let ids: Vec<&str> = out.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "a1", "b1", "c1", "a2", "s1", "b2", "c2", "a3", "b3", "s2", "c3"
            ]
        );
    }

    #[test]
    fn recent_songs_wait_at_the_back() {
        let seed = t("s0", "Seed");
        let related = vec![vec![t("a1", "A"), t("a2", "A")]];
        let recent: HashSet<String> = [recent_key(Some("A"), "song a1")].into();
        let out = arrange(&seed, related, Vec::new(), &recent, 50);
        let ids: Vec<&str> = out.iter().map(|t| t.id.as_str()).collect();
        // Fewer than eight fresh: the recent one comes back, after the fresh ones.
        assert_eq!(ids, ["a2", "a1"]);
    }
}
