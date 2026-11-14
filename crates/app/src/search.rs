//! The search page (Android OnlineSearchScreen, LinkSearch.kt, the search history):
//!
//! - where to search: the main account's catalog, the library, or another linked account (SearchSource);
//! - every kind of result at once: tracks, artists, albums, playlists;
//! - a pasted Spotify / Yandex / YouTube / Drxwnify link opens what it names instead of a text search;
//! - suggestions while typing (YouTube Music's, as on Android) after the history's own matches;
//! - the history in the shared `search_history` table (the phone's 159 queries come with the backup);
//! - library search folds case and «ё»; explicit tracks are hidden when the phone's `hideExplicit` is on.

use crate::accounts::source_label;
use crate::playback::Playable;
use crate::{App, AppError, AppResult, ListSpec};
use drx_catalog::model::{
    AlbumSummary, ArtistRef, ArtistSummary, Image, Playlist, SearchResults, Source, Track,
};

/// Where a search looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    /// The main account's catalog (Spotify's public catalog when no account is linked).
    Main,
    /// The listener's own library (database).
    Library,
    /// Another linked account's catalog.
    Catalog(Source),
}

/// A finished search: results, and tracks ready to play (VK tracks carry their URLs).
#[derive(Debug, Clone, Default)]
pub struct SearchOutcome {
    pub results: SearchResults,
    pub playable: Vec<Playable>,
    /// Ids are the library's (pages open from the database).
    pub library: bool,
    /// The query was a link: the one thing it names.
    pub from_link: bool,
}

/// Lower case, «ё» → «е», letters and digits only (Android normalizeForSearch).
pub fn fold(s: &str) -> String {
    s.to_lowercase()
        .replace('ё', "е")
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn url_in(text: &str) -> Option<String> {
    let t = text.trim();
    let start = t
        .find("https://")
        .or_else(|| t.find("http://"))
        .or_else(|| t.find("drxwnify://"))?;
    let rest = &t[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(
        rest[..end]
            .trim_end_matches(['.', ',', ')', '»', '"'])
            .to_string(),
    )
}

#[derive(Debug, Clone, PartialEq)]
enum LinkTarget {
    Track(String),
    Album(String),
    Artist(String),
    Playlist(String),
    YouTube(String),
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.split('#').next().unwrap_or(v).to_string())
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What a link names (AppLinks.parse + LinkSearch.youTubeVideoId).
fn parse_link(url: &str) -> Option<LinkTarget> {
    let (scheme, rest) = url.split_once("://")?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_lowercase();
    let host = host.trim_start_matches("www.").trim_start_matches("m.");
    let path = path.split(['?', '#']).next().unwrap_or("");
    let seg: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let numeric = |s: &&str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if host == "youtu.be" {
        return seg
            .first()
            .filter(|s| is_video_id(s))
            .map(|s| LinkTarget::YouTube(s.to_string()));
    }
    if host == "youtube.com" || host == "music.youtube.com" {
        let id = query_param(url, "v").or_else(|| {
            (seg.len() >= 2 && matches!(seg[0], "shorts" | "live" | "embed"))
                .then(|| seg[1].to_string())
        })?;
        return is_video_id(&id).then_some(LinkTarget::YouTube(id));
    }
    let (kind, id) = if scheme == "drxwnify" {
        (host.to_string(), seg.first()?.to_string())
    } else if host == "drxwnify.pages.dev" {
        (seg.first()?.to_string(), seg.get(1)?.to_string())
    } else if host == "open.spotify.com" {
        let s: Vec<&&str> = seg.iter().skip_while(|s| s.starts_with("intl-")).collect();
        (s.first()?.to_string(), s.get(1)?.to_string())
    } else if host
        .trim_start_matches("next.")
        .starts_with("music.yandex.")
    {
        let at = |i: usize| seg.get(i).copied();
        return match (at(0), at(1), at(2), at(3)) {
            (Some("album"), _, Some("track"), Some(t)) if numeric(&t) => {
                Some(LinkTarget::Track(format!("ym_{t}")))
            }
            (Some("track"), Some(t), _, _) if numeric(&t) => {
                Some(LinkTarget::Track(format!("ym_{t}")))
            }
            (Some("album"), Some(a), _, _) if numeric(&a) => {
                Some(LinkTarget::Album(format!("ym_{a}")))
            }
            (Some("artist"), Some(a), _, _) if numeric(&a) => {
                Some(LinkTarget::Artist(format!("ym_{a}")))
            }
            (Some("users"), Some(u), Some("playlists"), Some(k)) if numeric(&k) => {
                Some(LinkTarget::Playlist(format!("ym_{u}_{k}")))
            }
            _ => None,
        };
    } else {
        return None;
    };
    let id = id.replace("%3A", ":");
    if let Some(v) = id.strip_prefix("yt:") {
        return is_video_id(v).then(|| LinkTarget::YouTube(v.to_string()));
    }
    match kind.as_str() {
        "track" => Some(LinkTarget::Track(id)),
        "album" => Some(LinkTarget::Album(id)),
        "artist" => Some(LinkTarget::Artist(id)),
        "playlist" => Some(LinkTarget::Playlist(id)),
        _ => None,
    }
}

impl App {
    /// The search places for the chips: the main catalog first, the library, then the other accounts.
    pub fn search_sources(&self) -> Vec<(SearchSource, String)> {
        let main = self.main_account();
        let mut out = vec![(
            SearchSource::Main,
            main.map(source_label).unwrap_or("Spotify").to_string(),
        )];
        out.push((SearchSource::Library, "Медиатека".to_string()));
        for s in [Source::Spotify, Source::Yandex, Source::Vk] {
            if Some(s) != main && self.has_account(s) {
                out.push((SearchSource::Catalog(s), source_label(s).to_string()));
            }
        }
        out
    }

    fn hide_explicit(&self) -> bool {
        std::fs::read(self.profile().prefs_pb())
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .and_then(|p| p.get_bool("hideExplicit"))
            .unwrap_or(false)
    }

    /// Searches `query` in `source` (or opens the link it is).
    pub async fn search_page(&self, query: &str, source: SearchSource) -> AppResult<SearchOutcome> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(SearchOutcome::default());
        }
        let _ = self.remember_search(q).await;
        if let Some(url) = url_in(q) {
            let url = self.expand_short_link(&url).await;
            if let Some(target) = parse_link(&url) {
                return self.open_link(target).await;
            }
        }
        let mut out = match source {
            SearchSource::Library => self.search_library(q).await?,
            SearchSource::Main => {
                let s = self.main_account().unwrap_or(Source::Spotify);
                self.search_catalog(q, s).await?
            }
            SearchSource::Catalog(s) => self.search_catalog(q, s).await?,
        };
        if self.hide_explicit() {
            out.results.tracks.retain(|t| !t.explicit);
            out.playable.retain(|p| !p.track.explicit);
        }
        Ok(out)
    }

    async fn search_catalog(&self, q: &str, s: Source) -> AppResult<SearchOutcome> {
        let err = |e: &dyn std::fmt::Display| AppError::Other(e.to_string());
        let (results, playable) = match s {
            Source::Spotify => {
                let r = self
                    .spotify_or_public()
                    .search_all(q, 30)
                    .await
                    .map_err(|e| err(&e))?;
                let p = plain(&r.tracks);
                (r, p)
            }
            Source::Yandex => {
                let y = self.yandex().unwrap_or_else(|| {
                    std::sync::Arc::new(drx_catalog::yandex::Yandex::new(
                        self.inner.http.clone(),
                        None,
                        None,
                    ))
                });
                let r = y.search_all(q).await.map_err(|e| err(&e))?;
                let p = plain(&r.tracks);
                (r, p)
            }
            Source::Vk => {
                let vk = self
                    .vk()
                    .ok_or_else(|| AppError::Other("нет входа во ВКонтакте".into()))?;
                let (r, audios) = vk.search_all(q).await.map_err(|e| err(&e))?;
                let p = audios
                    .into_iter()
                    .map(|a| Playable {
                        track: a.track,
                        url: a.url,
                    })
                    .collect();
                (r, p)
            }
        };
        Ok(SearchOutcome {
            results,
            playable,
            library: false,
            from_link: false,
        })
    }

    /// The library: songs (liked or in the library), artists, albums and playlists whose names match.
    async fn search_library(&self, q: &str) -> AppResult<SearchOutcome> {
        let needle = fold(q);
        let n2 = needle.clone();
        let covers = self.profile().covers_dir();
        let (songs, artists, albums, playlists) = self
            .store()
            .call(move |c| {
                let hit = |text: &str| fold(text).contains(&n2);
                let mut songs = Vec::new();
                for r in crate::lists::all_library_rows(c)? {
                    if hit(&r.title) || hit(&r.artists) || r.album.as_deref().is_some_and(hit) {
                        songs.push(r);
                        if songs.len() >= 50 {
                            break;
                        }
                    }
                }
                let mut artists = Vec::new();
                let mut st = c.prepare(
                    "SELECT a.id, a.name, a.thumbnailUrl FROM artist a WHERE a.bookmarkedAt IS NOT NULL
                     OR EXISTS (SELECT 1 FROM song_artist_map m JOIN song s ON s.id = m.songId
                                WHERE m.artistId = a.id AND (s.liked = 1 OR s.inLibrary IS NOT NULL))",
                )?;
                for row in st.query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
                })? {
                    let (id, name, thumb) = row?;
                    if hit(&name) && artists.len() < 20 {
                        artists.push(ArtistSummary {
                            id,
                            name,
                            images: thumb.map(|url| vec![Image { url, width: None }]).unwrap_or_default(),
                        });
                    }
                }
                let mut albums = Vec::new();
                let mut st = c.prepare(
                    "SELECT a.id, a.title, a.thumbnailUrl, a.year,
                     (SELECT group_concat(ar.name, ', ') FROM album_artist_map m JOIN artist ar ON ar.id = m.artistId WHERE m.albumId = a.id)
                     FROM album a",
                )?;
                for row in st.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })? {
                    let (id, name, thumb, year, by) = row?;
                    let by = by.unwrap_or_default();
                    if (hit(&name) || hit(&by)) && albums.len() < 20 {
                        albums.push(AlbumSummary {
                            id,
                            name,
                            artists: vec![ArtistRef { id: String::new(), name: by }],
                            images: thumb.map(|url| vec![Image { url, width: None }]).unwrap_or_default(),
                            date: year.map(|y| y.to_string()),
                            kind: "album".into(),
                        });
                    }
                }
                let mut playlists = Vec::new();
                let mut st = c.prepare(&format!(
                    "SELECT p.id, p.name, p.thumbnailUrl, (SELECT count(*) FROM playlist_song_map m WHERE m.playlistId = p.id), {} FROM playlist p",
                    crate::PLAYLIST_FIRST_COVER
                ))?;
                for row in st.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        crate::playlist_cover(&covers, r.get(2)?, r.get(4)?),
                        r.get::<_, i64>(3)?,
                    ))
                })? {
                    let (id, name, thumb, n) = row?;
                    if hit(&name) {
                        playlists.push(Playlist {
                            id,
                            name,
                            description: None,
                            owner: None,
                            images: thumb.map(|url| vec![Image { url, width: None }]).unwrap_or_default(),
                            total: n as u32,
                        });
                    }
                }
                Ok((songs, artists, albums, playlists))
            })
            .await?;
        let playable: Vec<Playable> = songs.into_iter().map(crate::playable_of).collect();
        Ok(SearchOutcome {
            results: SearchResults {
                tracks: playable.iter().map(|p| p.track.clone()).collect(),
                artists,
                albums,
                playlists,
            },
            playable,
            library: true,
            from_link: false,
        })
    }

    /// spotify.link/… → the open.spotify.com address it stands for (LinkSearch.expand).
    async fn expand_short_link(&self, url: &str) -> String {
        let host = url
            .split("://")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap_or("");
        if !matches!(host, "spotify.link" | "spotify.app.link") {
            return url.to_string();
        }
        let Ok(resp) = self.inner.http.client().get(url).send().await else {
            return url.to_string();
        };
        let finalu = resp.url().to_string();
        if finalu.contains("open.spotify.com/") {
            return finalu;
        }
        // Some short links answer with a page carrying the address instead of a redirect.
        let body = resp.text().await.unwrap_or_default();
        body.find("https://open.spotify.com/")
            .map(|i| {
                body[i..]
                    .chars()
                    .take_while(|c| !matches!(c, '"' | '\'' | '<' | ' ' | '?'))
                    .collect()
            })
            .unwrap_or(finalu)
    }

    async fn open_link(&self, target: LinkTarget) -> AppResult<SearchOutcome> {
        let mut r = SearchResults::default();
        let mut playable = Vec::new();
        match target {
            LinkTarget::YouTube(id) => {
                if let Some(t) = self.youtube_track(&id).await {
                    playable.push(Playable {
                        track: t.clone(),
                        url: None,
                    });
                    r.tracks.push(t);
                }
            }
            LinkTarget::Track(id) => {
                if let Some(t) = self.track_by_id(&id).await {
                    playable.push(Playable {
                        track: t.clone(),
                        url: None,
                    });
                    r.tracks.push(t);
                }
            }
            LinkTarget::Album(id) => {
                if let Some(a) = self.catalog_album_any(&id).await {
                    r.albums.push(AlbumSummary {
                        id: a.id,
                        name: a.name,
                        artists: a.artists,
                        images: a.images,
                        date: a.release_date,
                        kind: a.album_type.unwrap_or_else(|| "album".into()),
                    });
                }
            }
            LinkTarget::Artist(id) => {
                if let Some(p) = self.catalog_artist_any(&id).await {
                    r.artists.push(ArtistSummary {
                        id: p.id,
                        name: p.name,
                        images: p.images,
                    });
                }
            }
            LinkTarget::Playlist(id) => {
                let p = match Source::of_id(&id) {
                    Source::Yandex => self.yandex_any().playlist_info(&id).await.ok(),
                    Source::Spotify => self.spotify_or_public().playlist_info(&id).await.ok(),
                    Source::Vk => None,
                };
                r.playlists.extend(p);
            }
        }
        Ok(SearchOutcome {
            results: r,
            playable,
            library: false,
            from_link: true,
        })
    }

    pub(crate) fn yandex_any(&self) -> std::sync::Arc<drx_catalog::yandex::Yandex> {
        self.yandex().unwrap_or_else(|| {
            std::sync::Arc::new(drx_catalog::yandex::Yandex::new(
                self.inner.http.clone(),
                None,
                None,
            ))
        })
    }

    async fn catalog_album_any(&self, id: &str) -> Option<drx_catalog::model::Album> {
        match Source::of_id(id) {
            Source::Yandex => self.yandex_any().album(id).await.ok(),
            Source::Spotify => self.spotify_or_public().album(id).await.ok(),
            Source::Vk => None,
        }
    }

    async fn catalog_artist_any(&self, id: &str) -> Option<drx_catalog::model::ArtistPage> {
        match Source::of_id(id) {
            Source::Yandex => self.yandex_any().artist(id).await.ok(),
            Source::Spotify => self.spotify_or_public().artist(id).await.ok(),
            Source::Vk => self.vk()?.artist(id).await.ok().map(|(p, _)| p),
        }
    }

    /// A track by catalog id: Yandex directly; Spotify via its embed page (the track lookup is refused for
    /// everyone), then the search under the same id for the album (LinkSearch.track).
    async fn track_by_id(&self, id: &str) -> Option<Track> {
        if Source::of_id(id) == Source::Yandex {
            return self
                .yandex_any()
                .tracks(&[id.trim_start_matches("ym_").to_string()])
                .await
                .ok()?
                .into_iter()
                .next();
        }
        let html = self
            .inner
            .http
            .client()
            .get(format!("https://open.spotify.com/embed/track/{id}"))
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        let start = html.find("<script id=\"__NEXT_DATA__\" type=\"application/json\">")?;
        let json = &html[start..];
        let json = &json[json.find('>')? + 1..];
        let json = &json[..json.find("</script>")?];
        let v: serde_json::Value = serde_json::from_str(json).ok()?;
        let e = v.pointer("/props/pageProps/state/data/entity")?;
        let name = e.get("name")?.as_str()?.to_string();
        let artists: Vec<ArtistRef> = e
            .get("artists")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| ArtistRef {
                        id: o
                            .get("uri")
                            .and_then(|u| u.as_str())
                            .and_then(|u| u.rsplit(':').next())
                            .unwrap_or("")
                            .to_string(),
                        name: o
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let query = format!(
            "{} {name}",
            artists.first().map(|a| a.name.as_str()).unwrap_or("")
        );
        if let Ok(found) = self
            .spotify_or_public()
            .search_tracks(query.trim(), 20)
            .await
            && let Some(t) = found.into_iter().find(|t| t.id == id)
        {
            return Some(t);
        }
        let images: Vec<Image> = e
            .pointer("/visualIdentity/image")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        Some(Image {
                            url: o.get("url")?.as_str()?.to_string(),
                            width: o.get("maxWidth").and_then(|w| w.as_u64()).map(|w| w as u32),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Track {
            id: id.to_string(),
            title: name,
            artists,
            album: Some(drx_catalog::model::AlbumRef {
                id: String::new(),
                name: String::new(),
                images,
            }),
            duration_ms: e.get("duration").and_then(|d| d.as_u64()).unwrap_or(0),
            isrc: None,
            plays: None,
            explicit: e
                .get("isExplicit")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        })
    }

    /// A YouTube video as a track (its id plays directly: the resolver takes video ids as they are).
    async fn youtube_track(&self, id: &str) -> Option<Track> {
        let yt = drx_resolver::providers::youtube::YouTube::new(self.inner.http.clone());
        let (title, author, secs, thumb) = yt.video_info(id).await?;
        Some(Track {
            id: id.to_string(),
            title,
            artists: vec![ArtistRef {
                id: String::new(),
                name: author,
            }],
            album: thumb.map(|url| drx_catalog::model::AlbumRef {
                id: String::new(),
                name: String::new(),
                images: vec![Image { url, width: None }],
            }),
            duration_ms: secs * 1000,
            isrc: None,
            plays: None,
            explicit: false,
        })
    }

    /// Suggestions while typing: matching history first (up to 3), then YouTube Music's (Android's source).
    pub async fn search_suggestions(&self, query: &str) -> (Vec<String>, Vec<String>) {
        let history = self.search_history(query, 3).await;
        if query.trim().is_empty() || url_in(query).is_some() {
            return (self.search_history("", 12).await, Vec::new());
        }
        let yt = drx_resolver::providers::youtube::YouTube::new(self.inner.http.clone());
        let online: Vec<String> = yt
            .suggestions(query.trim())
            .await
            .into_iter()
            .filter(|s| !history.iter().any(|h| h.eq_ignore_ascii_case(s)))
            .take(8)
            .collect();
        (history, online)
    }

    /// The history, newest first, filtered by `prefix` (contains, folded).
    pub async fn search_history(&self, prefix: &str, limit: usize) -> Vec<String> {
        let needle = fold(prefix);
        self.store()
            .call(move |c| {
                let mut st =
                    c.prepare("SELECT query FROM search_history ORDER BY id DESC LIMIT 500")?;
                let mut out: Vec<String> = Vec::new();
                for q in st.query_map([], |r| r.get::<_, String>(0))? {
                    let q = q?;
                    if (needle.is_empty() || fold(&q).contains(&needle))
                        && !out.iter().any(|x| x == &q)
                    {
                        out.push(q);
                        if out.len() >= limit {
                            break;
                        }
                    }
                }
                Ok(out)
            })
            .await
            .unwrap_or_default()
    }

    /// Adds a query to the history (a repeat moves to the top).
    pub async fn remember_search(&self, query: &str) -> AppResult<()> {
        let q = query.trim().to_string();
        self.store()
            .call(move |c| {
                c.execute("DELETE FROM search_history WHERE query = ?1", [&q])?;
                c.execute("INSERT INTO search_history (query) VALUES (?1)", [&q])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn forget_search(&self, query: &str) -> AppResult<()> {
        let q = query.to_string();
        self.store()
            .call(move |c| {
                c.execute("DELETE FROM search_history WHERE query = ?1", [&q])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn clear_search_history(&self) -> AppResult<()> {
        self.store()
            .call(|c| {
                c.execute("DELETE FROM search_history", [])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Plays search results (VK ones with their URLs) from `index`.
    pub fn play_found(&self, tracks: Vec<Playable>, index: usize) {
        self.inner.playback.play_list(tracks, index);
    }

    /// The page a playlist result opens.
    pub fn playlist_page(&self, id: &str, library: bool) -> ListSpec {
        if library {
            return ListSpec::Playlist { id: id.to_string() };
        }
        match Source::of_id(id) {
            Source::Yandex => ListSpec::YandexPlaylist { id: id.to_string() },
            Source::Vk => ListSpec::VkPlaylist { id: id.to_string() },
            Source::Spotify => ListSpec::SpotifyPlaylist { id: id.to_string() },
        }
    }
}

fn plain(tracks: &[Track]) -> Vec<Playable> {
    tracks
        .iter()
        .map(|t| Playable {
            track: t.clone(),
            url: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_and_yo() {
        assert_eq!(fold("  Ёлка — ЛЁД!  "), "елка лед");
    }

    #[test]
    fn links() {
        let p = |u: &str| parse_link(u);
        assert_eq!(
            p("https://open.spotify.com/intl-de/track/4cOdK2wGLETKBW3PvgPWqT?si=x"),
            Some(LinkTarget::Track("4cOdK2wGLETKBW3PvgPWqT".into()))
        );
        assert_eq!(
            p("https://open.spotify.com/album/1ATL5GLyefJaxhQzSPVrLX"),
            Some(LinkTarget::Album("1ATL5GLyefJaxhQzSPVrLX".into()))
        );
        assert_eq!(
            p("https://music.yandex.ru/album/123/track/456"),
            Some(LinkTarget::Track("ym_456".into()))
        );
        assert_eq!(
            p("https://music.yandex.com/users/someone/playlists/1001"),
            Some(LinkTarget::Playlist("ym_someone_1001".into()))
        );
        assert_eq!(
            p("https://youtu.be/dQw4w9WgXcQ"),
            Some(LinkTarget::YouTube("dQw4w9WgXcQ".into()))
        );
        assert_eq!(
            p("https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=x"),
            Some(LinkTarget::YouTube("dQw4w9WgXcQ".into()))
        );
        assert_eq!(
            p("https://drxwnify.pages.dev/track/yt%3AdQw4w9WgXcQ?n=x"),
            Some(LinkTarget::YouTube("dQw4w9WgXcQ".into()))
        );
        assert_eq!(p("https://example.com/track/1"), None);
        assert_eq!(
            url_in("Artist — Title https://youtu.be/dQw4w9WgXcQ."),
            Some("https://youtu.be/dQw4w9WgXcQ".into())
        );
    }
}
