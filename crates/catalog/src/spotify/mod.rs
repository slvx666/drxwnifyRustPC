//! Spotify through the web player's GraphQL API (port of `spotify/Spotify.kt`, `SpotifyHashProvider.kt`).
//!
//! - Persisted queries: operation name + sha256 hash. Built-in hashes, refreshed from the community registry
//!   (`francescograzioso.github.io/Meld/spotify-gql-hashes.json`), with `previous_hash` as a fallback.
//! - `me/top/*` is never called over REST: the shared client id is rate-limited forever (PORTING-2.3.0 §1.1);
//!   tops come from GraphQL `userTopContent` (§4).

pub mod auth;
mod parse;

use crate::model::{Album, ArtistPage, ArtistSummary, Page, Playlist, SearchResults, Track, User};
use drx_core::http::{Http, HttpError, Method, StatusCode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;

const GQL_URL: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const HASH_REGISTRY: &str = "https://francescograzioso.github.io/Meld/spotify-gql-hashes.json";

/// Built-in hashes (SpotifyHashProvider.kt, 14.11.2026). The registry refresh overrides them.
const DEFAULT_HASHES: &[(&str, &str)] = &[
    (
        "profileAttributes",
        "53bcb064f6cd18c23f752bc324a791194d20df612d8e1239c735144ab0399ced",
    ),
    (
        "libraryV3",
        "973e511ca44261fda7eebac8b653155e7caee3675abb4fb110cc1b8c78b091c3",
    ),
    (
        "fetchPlaylist",
        "346811f856fb0b7e4f6c59f8ebea78dd081c6e2fb01b77c954b26259d5fc6763",
    ),
    (
        "fetchLibraryTracks",
        "087278b20b743578a6262c2b0b4bcd20d879c503cc359a2285baf083ef944240",
    ),
    (
        "searchDesktop",
        "4801118d4a100f756e833d33984436a3899cff359c532f8fd3aaf174b60b3b49",
    ),
    (
        "queryArtistOverview",
        "5b9e64f43843fa3a9b6a98543600299b0a2cbbbccfdcdcef2402eb9c1017ca4c",
    ),
    (
        "getAlbum",
        "b9bfabef66ed756e5e13f68a942deb60bd4125ec1f1be8cc42769dc0259b4b10",
    ),
    (
        "queryWhatsNewFeed",
        "3b53dede3c6054e8b7c962dd280eb6761c5d1c82b06b039f4110d76a62b4966b",
    ),
    (
        "addToPlaylist",
        "47b2a1234b17748d332dd0431534f22450e9ecbb3d5ddcdacbd83368636a0990",
    ),
    (
        "removeFromPlaylist",
        "47b2a1234b17748d332dd0431534f22450e9ecbb3d5ddcdacbd83368636a0990",
    ),
    (
        "moveItemsInPlaylist",
        "47b2a1234b17748d332dd0431534f22450e9ecbb3d5ddcdacbd83368636a0990",
    ),
    (
        "editPlaylistAttributes",
        "35a1a9ce3a2f4f8c32ee0e24c63c2069c6613c0a0b7e56d0e40dabe69a0b4f80",
    ),
    (
        "addToLibrary",
        "7c5a69420e2bfae3da5cc4e14cbc8bb3f6090f80afc00ffc179177f19be3f33d",
    ),
    (
        "removeFromLibrary",
        "7c5a69420e2bfae3da5cc4e14cbc8bb3f6090f80afc00ffc179177f19be3f33d",
    ),
    (
        "home",
        "23e37f2e58d82d567f27080101d36609009d8c3676457b1086cb0acc55b72a5d",
    ),
    (
        "userTopContent",
        "61b7353d3625780022dbc63d459eb64cd609e7e5e2a820f9160d65030efe2e15",
    ),
];

#[derive(Debug, thiserror::Error)]
pub enum SpotifyError {
    #[error(transparent)]
    Auth(#[from] auth::AuthError),
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Spotify changed the query {0}; hashes need an update")]
    HashRotated(String),
    #[error("GraphQL: {0}")]
    Graphql(String),
    #[error("unexpected answer from {0}")]
    Shape(&'static str),
}

pub type Result<T> = std::result::Result<T, SpotifyError>;

#[derive(Clone)]
struct HashEntry {
    hash: String,
    previous: Option<String>,
}

pub struct Spotify {
    http: Http,
    sp_dc: Option<String>,
    sp_key: Option<String>,
    token: tokio::sync::Mutex<Option<auth::Token>>,
    hashes: Mutex<HashMap<String, HashEntry>>,
}

impl Spotify {
    /// A client for the account (cookie from the vault) or, without one, anonymous (public catalog).
    pub fn new(http: Http, sp_dc: Option<String>, sp_key: Option<String>) -> Self {
        let hashes = DEFAULT_HASHES
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    HashEntry {
                        hash: v.to_string(),
                        previous: None,
                    },
                )
            })
            .collect();
        Self {
            http,
            sp_dc,
            sp_key,
            token: tokio::sync::Mutex::new(None),
            hashes: Mutex::new(hashes),
        }
    }

    pub fn has_account(&self) -> bool {
        self.sp_dc.is_some()
    }

    async fn token(&self, force: bool) -> Result<String> {
        let mut t = self.token.lock().await;
        if !force && let Some(tok) = t.as_ref().filter(|t| t.fresh()) {
            return Ok(tok.access_token.clone());
        }
        let fresh =
            auth::fetch_token(&self.http, self.sp_dc.as_deref(), self.sp_key.as_deref()).await?;
        let s = fresh.access_token.clone();
        *t = Some(fresh);
        Ok(s)
    }

    /// Pulls the current hashes from the community registry (operations.{name}.hash / previous_hash).
    pub async fn refresh_hashes(&self) -> Result<usize> {
        let v = self.http.get_json(HASH_REGISTRY).await?;
        let ops = v
            .get("operations")
            .and_then(Value::as_object)
            .ok_or(SpotifyError::Shape("hash registry"))?;
        let mut h = self.hashes.lock().expect("hashes lock");
        let mut n = 0;
        for (name, e) in ops {
            if let Some(hash) = e.get("hash").and_then(Value::as_str) {
                let previous = e
                    .get("previous_hash")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                h.insert(
                    name.clone(),
                    HashEntry {
                        hash: hash.to_string(),
                        previous,
                    },
                );
                n += 1;
            }
        }
        Ok(n)
    }

    fn hashes_for(&self, op: &str) -> Vec<String> {
        let h = self.hashes.lock().expect("hashes lock");
        match h.get(op) {
            Some(e) => std::iter::once(e.hash.clone())
                .chain(e.previous.clone().filter(|p| p != &e.hash))
                .collect(),
            None => Vec::new(),
        }
    }

    /// One GraphQL persisted query. A 401 gets one fresh token; a rejected hash tries `previous_hash`.
    pub async fn graphql(&self, op: &str, variables: Value) -> Result<Value> {
        let candidates = self.hashes_for(op);
        if candidates.is_empty() {
            return Err(SpotifyError::HashRotated(op.into()));
        }
        for hash in candidates {
            let body = json!({
                "variables": variables,
                "operationName": op,
                "extensions": { "persistedQuery": { "version": 1, "sha256Hash": hash } }
            });
            let mut refreshed = false;
            loop {
                let token = self.token(refreshed).await?;
                let req = self
                    .http
                    .request(Method::POST, GQL_URL)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("app-platform", "WebPlayer")
                    .header("Origin", "https://open.spotify.com")
                    .header("Referer", "https://open.spotify.com/")
                    .header("Accept", "application/json")
                    .json(&body);
                match self.http.send(req).await {
                    Ok(resp) => {
                        let v: Value = resp.json().await.map_err(HttpError::from)?;
                        if let Some(err) = v
                            .get("errors")
                            .and_then(Value::as_array)
                            .and_then(|e| e.first())
                        {
                            let msg = err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown error");
                            if msg.contains("PersistedQueryNotFound") {
                                break; // next hash
                            }
                            return Err(SpotifyError::Graphql(msg.to_string()));
                        }
                        return Ok(v);
                    }
                    Err(HttpError::Status { status, .. })
                        if status == StatusCode::UNAUTHORIZED && !refreshed =>
                    {
                        refreshed = true;
                        continue;
                    }
                    Err(HttpError::Status { status, .. })
                        if status == StatusCode::PRECONDITION_FAILED =>
                    {
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Err(SpotifyError::HashRotated(op.into()))
    }

    pub async fn me(&self) -> Result<User> {
        let v = self.graphql("profileAttributes", json!({})).await?;
        parse::user(&v).ok_or(SpotifyError::Shape("profileAttributes"))
    }

    /// Puts items (`spotify:track:…`, `spotify:album:…`, `spotify:artist:…` URIs) into the account's library or takes
    /// them out: the web player's own `addToLibrary` / `removeFromLibrary` (REST /me/tracks and /me/following refuse
    /// the web-player token on many accounts — Spotify.kt).
    pub async fn set_in_library(&self, uris: &[String], saved: bool) -> Result<()> {
        let op = if saved { "addToLibrary" } else { "removeFromLibrary" };
        self.graphql(op, json!({ "libraryItemUris": uris })).await?;
        Ok(())
    }

    /// Adds tracks (`spotify:track:…`) at the end of one of the listener's playlists (the web player's
    /// `addToPlaylist`, Spotify.addTracksToPlaylist).
    pub async fn add_to_playlist(&self, playlist_id: &str, uris: &[String]) -> Result<()> {
        let vars = json!({
            "playlistUri": format!("spotify:playlist:{playlist_id}"),
            "playlistItemUris": uris,
            "newPosition": { "moveType": "BOTTOM_OF_PLAYLIST", "fromUid": null }
        });
        self.graphql("addToPlaylist", vars).await?;
        Ok(())
    }

    /// Liked songs, newest first.
    pub async fn liked_songs(&self, offset: u32, limit: u32) -> Result<Page<Track>> {
        let v = self
            .graphql(
                "fetchLibraryTracks",
                json!({ "offset": offset, "limit": limit }),
            )
            .await?;
        parse::liked(&v, offset).ok_or(SpotifyError::Shape("fetchLibraryTracks"))
    }

    /// The user's playlists, flattened out of folders (libraryV3 with flatten=true, as Android).
    pub async fn my_playlists(&self, offset: u32, limit: u32) -> Result<Page<Playlist>> {
        let vars = json!({
            "filters": ["Playlists"], "order": null, "textFilter": "",
            "features": ["LIKED_SONGS", "YOUR_EPISODES_V2", "PRERELEASES", "EVENTS"],
            "limit": limit, "offset": offset, "flatten": true, "expandedFolders": [],
            "folderUri": null, "includeFoldersWhenFlattening": false
        });
        let v = self.graphql("libraryV3", vars).await?;
        parse::library_playlists(&v, offset).ok_or(SpotifyError::Shape("libraryV3"))
    }

    /// Every saved album or followed artist id (`filter`: "Albums" or "Artists"), up to 5000.
    pub async fn library_ids(&self, filter: &str) -> Result<std::collections::HashSet<String>> {
        let kind = if filter == "Artists" { "artist" } else { "album" };
        let mut out = std::collections::HashSet::new();
        let mut offset = 0u32;
        while offset < 5000 {
            let vars = json!({
                "filters": [filter], "order": null, "textFilter": "",
                "features": ["LIKED_SONGS", "YOUR_EPISODES_V2", "PRERELEASES", "EVENTS"],
                "limit": 50, "offset": offset, "flatten": false, "expandedFolders": [],
                "folderUri": null, "includeFoldersWhenFlattening": true
            });
            let v = self.graphql("libraryV3", vars).await?;
            let (ids, total) = parse::library_ids(&v, kind).ok_or(SpotifyError::Shape("libraryV3"))?;
            let n = ids.len();
            out.extend(ids);
            offset += 50;
            if n == 0 || offset >= total {
                break;
            }
        }
        Ok(out)
    }

    pub async fn playlist_tracks(
        &self,
        playlist_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Page<Track>> {
        let vars = json!({ "uri": format!("spotify:playlist:{playlist_id}"), "offset": offset, "limit": limit, "enableWatchFeedEntrypoint": false });
        let v = self.graphql("fetchPlaylist", vars).await?;
        parse::playlist_tracks(&v, offset).ok_or(SpotifyError::Shape("fetchPlaylist"))
    }

    /// An album with all its tracks (getAlbum gives 50 per page; long albums and compilations are paged).
    /// A playlist's name, owner, cover and size (a pasted link, a page heading).
    pub async fn playlist_info(&self, playlist_id: &str) -> Result<Playlist> {
        let vars = json!({ "uri": format!("spotify:playlist:{playlist_id}"), "offset": 0, "limit": 1, "enableWatchFeedEntrypoint": false });
        let v = self.graphql("fetchPlaylist", vars).await?;
        parse::playlist_info(&v, playlist_id).ok_or(SpotifyError::Shape("fetchPlaylist"))
    }

    pub async fn album(&self, album_id: &str) -> Result<Album> {
        let page = |offset: u32| json!({ "uri": format!("spotify:album:{album_id}"), "locale": "", "offset": offset, "limit": 50 });
        let v = self.graphql("getAlbum", page(0)).await?;
        let mut album = parse::album(&v, album_id).ok_or(SpotifyError::Shape("getAlbum"))?;
        while (album.tracks.len() as u32) < album.total_tracks && album.tracks.len() < 1000 {
            let v = self
                .graphql("getAlbum", page(album.tracks.len() as u32))
                .await?;
            let more = parse::album(&v, album_id)
                .map(|a| a.tracks)
                .unwrap_or_default();
            if more.is_empty() {
                break;
            }
            album.tracks.extend(more);
        }
        Ok(album)
    }

    /// Artist name, picture and popular tracks (GraphQL `queryArtistOverview`).
    pub async fn artist(&self, artist_id: &str) -> Result<ArtistPage> {
        let vars = json!({ "uri": format!("spotify:artist:{artist_id}"), "locale": "" });
        let v = self.graphql("queryArtistOverview", vars).await?;
        parse::artist_page(&v, artist_id).ok_or(SpotifyError::Shape("queryArtistOverview"))
    }

    /// Top tracks of the user from GraphQL `userTopContent` (PORTING-2.3.0 §4). `range`: SHORT_TERM, MID_TERM,
    /// LONG_TERM (note: MID_TERM, not MEDIUM_TERM).
    /// Track search (GraphQL `searchDesktop`).
    pub async fn search_tracks(&self, query: &str, limit: u32) -> Result<Vec<Track>> {
        let vars = json!({
            "searchTerm": query, "offset": 0, "limit": limit, "numberOfTopResults": 5,
            "includeAudiobooks": false, "includeArtistHasConcertsField": false, "includePreReleases": false,
            "includeLocalConcertsField": false, "includeAuthors": false
        });
        let v = self.graphql("searchDesktop", vars).await?;
        Ok(parse::search_tracks(&v))
    }

    /// Everything at once: tracks, artists, albums, playlists (`searchDesktop`, the search page).
    pub async fn search_all(&self, query: &str, limit: u32) -> Result<SearchResults> {
        let vars = json!({
            "searchTerm": query, "offset": 0, "limit": limit, "numberOfTopResults": 5,
            "includeAudiobooks": false, "includeArtistHasConcertsField": false, "includePreReleases": false,
            "includeLocalConcertsField": false, "includeAuthors": false
        });
        let v = self.graphql("searchDesktop", vars).await?;
        Ok(parse::search_all(&v))
    }

    /// Artist search (the same `searchDesktop` query, artists part).
    pub async fn search_artists(&self, query: &str, limit: u32) -> Result<Vec<ArtistSummary>> {
        let vars = json!({
            "searchTerm": query, "offset": 0, "limit": limit, "numberOfTopResults": 5,
            "includeAudiobooks": false, "includeArtistHasConcertsField": false, "includePreReleases": false,
            "includeLocalConcertsField": false, "includeAuthors": false
        });
        let v = self.graphql("searchDesktop", vars).await?;
        Ok(parse::search_artists(&v))
    }

    pub async fn top_tracks(&self, range: &str, limit: u32) -> Result<Vec<Track>> {
        let vars = json!({
            "includeTopArtists": false,
            "topArtistsInput": { "offset": 0, "limit": limit, "sortBy": "AFFINITY", "timeRange": range },
            "includeTopTracks": true,
            "topTracksInput": { "offset": 0, "limit": limit, "sortBy": "AFFINITY", "timeRange": range }
        });
        let v = self.graphql("userTopContent", vars).await?;
        Ok(parse::top_tracks(&v))
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Real account check with the session from an imported profile. Set DRX_TEST_PROFILE to the profile folder.
    /// Prints only counts, never names or secrets. Not run in CI.
    #[test]
    fn real_session_works() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let profile = drx_core::paths::Profile::at(dir);
        let vault = drx_core::secrets::Vault::load(&profile).unwrap();
        let dc = vault
            .pref(drx_core::keys::spotify::SP_DC)
            .map(str::to_string);
        let key = vault
            .pref(drx_core::keys::spotify::SP_KEY)
            .map(str::to_string);
        assert!(dc.is_some(), "no Spotify session in the profile");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let sp = Spotify::new(Http::new().unwrap(), dc, key);
            match sp.refresh_hashes().await {
                Ok(n) => eprintln!("registry hashes: {n}"),
                Err(e) => eprintln!("registry refresh failed (built-in hashes used): {e}"),
            }
            let me = sp.me().await.unwrap();
            eprintln!("me: id_len={} has_name={}", me.id.len(), me.name.is_some());
            let liked = sp.liked_songs(0, 5).await.unwrap();
            eprintln!(
                "liked total={} first_page={}",
                liked.total,
                liked.items.len()
            );
            assert!(!liked.items.is_empty());
            let pls = sp.my_playlists(0, 50).await.unwrap();
            eprintln!("playlists total={} page={}", pls.total, pls.items.len());
            if let Some(p) = pls.items.first() {
                let t = sp.playlist_tracks(&p.id, 0, 10).await.unwrap();
                eprintln!(
                    "first playlist tracks total={} page={}",
                    t.total,
                    t.items.len()
                );
            }
            if let Some(album) = liked.items[0].album.as_ref() {
                let a = sp.album(&album.id).await.unwrap();
                eprintln!("album tracks={} total={}", a.tracks.len(), a.total_tracks);
            }
            match sp.top_tracks("SHORT_TERM", 10).await {
                Ok(t) => eprintln!("top tracks (4 weeks): {}", t.len()),
                Err(e) => eprintln!("top tracks failed: {e}"),
            }
        });
    }
}
