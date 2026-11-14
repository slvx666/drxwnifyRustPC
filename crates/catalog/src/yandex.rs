//! Yandex Music as a catalog (metadata and library only; audio comes from the resolver). Port of
//! `yandex/YandexMusic.kt` and `YandexParser.kt`. Ids: `ym_<raw>`, playlists `ym_<uid>_<kind>`.

use crate::js::arr;
use crate::model::{
    Album, AlbumRef, ArtistPage, ArtistRef, Image, Page, Playlist, Release, Track, User,
};
use drx_core::http::{Http, HttpError, Method, RequestBuilder};
use serde_json::Value;
use tokio::sync::Mutex;

const BASE: &str = "https://api.music.yandex.net";
const TRACKS_CHUNK: usize = 200;
pub const PREFIX: &str = "ym_";
pub const LIKED_ID: &str = "ym_liked";

#[derive(Debug, thiserror::Error)]
pub enum YandexError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Yandex: {0}")]
    Api(String),
    #[error("not signed in to Yandex")]
    NoAccount,
}

pub type Result<T> = std::result::Result<T, YandexError>;

pub fn raw(id: &str) -> &str {
    id.strip_prefix(PREFIX).unwrap_or(id)
}

/// `ym_<uid>_<kind>` → (uid, kind).
pub fn parse_playlist_id(id: &str) -> Option<(&str, &str)> {
    let r = id.strip_prefix(PREFIX)?;
    let sep = r.rfind('_')?;
    (sep > 0 && sep + 1 < r.len()).then(|| (&r[..sep], &r[sep + 1..]))
}

/// Ids are numbers or strings in this API; blank strings count as missing (YandexParser.str).
fn st(v: &Value, k: &str) -> Option<String> {
    match v.get(k)? {
        Value::String(x) if !x.trim().is_empty() => Some(x.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// "avatars.yandex.net/get-music-content/…/%%" → https URL of the size (YandexParser.coverUrl).
pub fn cover_url(uri: &str, size: u32) -> String {
    let sized = uri.trim().replace("%%", &format!("{size}x{size}"));
    if sized.starts_with("http") {
        sized
    } else {
        format!("https://{sized}")
    }
}

fn cover_images(uri: Option<String>) -> Vec<Image> {
    match uri.filter(|u| !u.trim().is_empty()) {
        None => Vec::new(),
        Some(u) => [1000, 400, 200]
            .iter()
            .map(|&w| Image {
                url: cover_url(&u, w),
                width: Some(w),
            })
            .collect(),
    }
}

fn artist(v: &Value) -> Option<ArtistRef> {
    Some(ArtistRef {
        id: format!("{PREFIX}{}", st(v, "id")?),
        name: st(v, "name").unwrap_or_default(),
    })
}

/// Album reference, its artists, release date, album type, track count.
type SimpleAlbum = (AlbumRef, Vec<ArtistRef>, Option<String>, String, u32);

fn simple_album(v: &Value) -> Option<SimpleAlbum> {
    let id = st(v, "id")?;
    let tc = v.get("trackCount").and_then(Value::as_u64).unwrap_or(0) as u32;
    let kind = match st(v, "type").as_deref().map(str::to_lowercase).as_deref() {
        Some("single") => {
            if tc > 3 {
                "ep"
            } else {
                "single"
            }
        }
        Some("compilation") => "compilation",
        _ => "album",
    };
    Some((
        AlbumRef {
            id: format!("{PREFIX}{id}"),
            name: st(v, "title").unwrap_or_default(),
            images: cover_images(st(v, "coverUri").or_else(|| st(v, "ogImage"))),
        },
        arr(v, "artists").iter().filter_map(artist).collect(),
        st(v, "releaseDate")
            .map(|d| d.chars().take(10).collect())
            .or_else(|| st(v, "year")),
        kind.to_string(),
        tc,
    ))
}

pub fn track(v: &Value, fallback_album: Option<&AlbumRef>) -> Option<Track> {
    let id = st(v, "id").or_else(|| st(v, "realId"))?;
    let title = st(v, "title").unwrap_or_default();
    let name = match st(v, "version") {
        Some(ver) if !title.to_lowercase().contains(&ver.to_lowercase()) => {
            format!("{title} ({ver})")
        }
        _ => title,
    };
    let album = arr(v, "albums")
        .first()
        .and_then(simple_album)
        .map(|(mut a, ..)| {
            if a.images.is_empty() {
                a.images = cover_images(st(v, "coverUri"));
            }
            a
        });
    Some(Track {
        id: format!("{PREFIX}{id}"),
        title: name,
        artists: arr(v, "artists").iter().filter_map(artist).collect(),
        album: album.or_else(|| fallback_album.cloned()),
        duration_ms: v.get("durationMs").and_then(Value::as_u64).unwrap_or(0),
        isrc: None,
        explicit: st(v, "contentWarning").is_some_and(|w| w.eq_ignore_ascii_case("explicit")),
    })
}

pub fn playlist(v: &Value) -> Option<Playlist> {
    let kind = st(v, "kind")?;
    let owner = v.get("owner");
    let uid = st(v, "uid").or_else(|| owner.and_then(|o| st(o, "uid")))?;
    let cover = v.get("cover");
    let cover_uri = cover
        .and_then(|c| st(c, "uri"))
        .or_else(|| {
            cover
                .and_then(|c| arr(c, "itemsUri").first())
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| st(v, "ogImage"));
    Some(Playlist {
        id: format!("{PREFIX}{uid}_{kind}"),
        name: st(v, "title").unwrap_or_default(),
        description: st(v, "description"),
        owner: owner.and_then(|o| st(o, "name").or_else(|| st(o, "login"))),
        images: cover_images(cover_uri),
        total: v.get("trackCount").and_then(Value::as_u64).unwrap_or(0) as u32,
    })
}

/// One liked-track reference: raw id, album id (pins the release), time.
#[derive(Debug, Clone)]
struct TrackRef {
    id: String,
    album_id: Option<String>,
}

impl TrackRef {
    fn request_id(&self) -> String {
        match &self.album_id {
            Some(a) => format!("{}:{a}", self.id),
            None => self.id.clone(),
        }
    }
}

pub struct Yandex {
    http: Http,
    token: Option<String>,
    uid: Mutex<Option<String>>,
}

impl Yandex {
    pub fn new(http: Http, token: Option<String>, uid: Option<String>) -> Self {
        Self {
            http,
            token,
            uid: Mutex::new(uid),
        }
    }

    pub fn has_account(&self) -> bool {
        self.token.is_some()
    }

    fn req(&self, method: Method, path: &str, anonymous: bool) -> Result<RequestBuilder> {
        let mut r = self
            .http
            .request(method, &format!("{BASE}/{path}"))
            .header("X-Yandex-Music-Client", "YandexMusicAndroid/24023621")
            .header("User-Agent", "Yandex-Music-API")
            .header("Accept-Language", "ru");
        match &self.token {
            Some(t) => r = r.header("Authorization", format!("OAuth {t}")),
            None if !anonymous => return Err(YandexError::NoAccount),
            None => {}
        }
        Ok(r)
    }

    async fn result(&self, r: RequestBuilder) -> Result<Value> {
        let v: Value = self
            .http
            .send(r)
            .await?
            .json()
            .await
            .map_err(HttpError::from)?;
        if let Some(e) = v.get("error") {
            return Err(YandexError::Api(
                st(e, "message")
                    .or_else(|| st(e, "name"))
                    .unwrap_or_else(|| "error".into()),
            ));
        }
        v.get("result")
            .cloned()
            .ok_or_else(|| YandexError::Api("empty result".into()))
    }

    async fn get(&self, path: &str, query: &[(&str, &str)], anonymous: bool) -> Result<Value> {
        let r = self.req(Method::GET, path, anonymous)?.query(query);
        self.result(r).await
    }

    async fn post_form(&self, path: &str, form: &[(&str, &str)], anonymous: bool) -> Result<Value> {
        let r = self.req(Method::POST, path, anonymous)?.form(form);
        self.result(r).await
    }

    async fn uid(&self) -> Result<String> {
        let mut u = self.uid.lock().await;
        if let Some(x) = u.as_ref() {
            return Ok(x.clone());
        }
        let me = self.account().await?;
        *u = Some(me.id.clone());
        Ok(me.id)
    }

    pub async fn account(&self) -> Result<User> {
        let v = self.get("account/status", &[], false).await?;
        let a = v
            .get("account")
            .ok_or_else(|| YandexError::Api("no account in status".into()))?;
        let id = st(a, "uid")
            .ok_or_else(|| YandexError::Api("token is not bound to an account".into()))?;
        Ok(User {
            id,
            name: st(a, "displayName")
                .or_else(|| st(a, "fullName"))
                .or_else(|| st(a, "login")),
            images: vec![],
        })
    }

    /// Full tracks for raw ids (optionally `id:albumId`), in chunks of 200, order preserved.
    pub async fn tracks(&self, ids: &[String]) -> Result<Vec<Track>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(TRACKS_CHUNK) {
            let joined = chunk.join(",");
            let v = self
                .post_form(
                    "tracks",
                    &[("track-ids", &joined), ("with-positions", "true")],
                    true,
                )
                .await?;
            out.extend(
                v.as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|t| track(t, None)),
            );
        }
        Ok(out)
    }

    async fn liked_refs(&self) -> Result<Vec<TrackRef>> {
        let uid = self.uid().await?;
        let v = self
            .get(
                &format!("users/{uid}/likes/tracks"),
                &[("if-modified-since-revision", "0")],
                false,
            )
            .await?;
        Ok(v.get("library")
            .map(|l| arr(l, "tracks"))
            .unwrap_or(&[])
            .iter()
            .filter_map(|o| {
                Some(TrackRef {
                    id: st(o, "id")?,
                    album_id: st(o, "albumId"),
                })
            })
            .collect())
    }

    /// A page of liked tracks, newest first.
    pub async fn liked_songs(&self, offset: u32, limit: u32) -> Result<Page<Track>> {
        let refs = self.liked_refs().await?;
        let page: Vec<&TrackRef> = refs
            .iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        let ids: Vec<String> = page.iter().map(|r| r.request_id()).collect();
        let fetched = self.tracks(&ids).await?;
        let by_id: std::collections::HashMap<&str, &Track> =
            fetched.iter().map(|t| (t.id.as_str(), t)).collect();
        let items = page
            .iter()
            .filter_map(|r| {
                by_id
                    .get(format!("{PREFIX}{}", r.id).as_str())
                    .map(|t| (*t).clone())
            })
            .collect();
        Ok(Page {
            items,
            total: refs.len() as u32,
            offset,
        })
    }

    pub async fn my_playlists(&self) -> Result<Vec<Playlist>> {
        let uid = self.uid().await?;
        let v = self
            .get(&format!("users/{uid}/playlists/list"), &[], false)
            .await?;
        Ok(v.as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(playlist)
            .collect())
    }

    pub async fn playlist_tracks(
        &self,
        playlist_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Page<Track>> {
        let (owner, kind) = parse_playlist_id(playlist_id)
            .ok_or_else(|| YandexError::Api("bad playlist id".into()))?;
        let v = self
            .get(
                &format!("users/{owner}/playlists/{kind}"),
                &[("rich-tracks", "true")],
                true,
            )
            .await?;
        let refs = arr(&v, "tracks");
        let page: Vec<&Value> = refs
            .iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        let missing: Vec<String> = page
            .iter()
            .filter(|r| r.get("track").is_none())
            .filter_map(|r| {
                Some(match st(r, "albumId") {
                    Some(a) => format!("{}:{a}", st(r, "id")?),
                    None => st(r, "id")?,
                })
            })
            .collect();
        let fetched = if missing.is_empty() {
            Vec::new()
        } else {
            self.tracks(&missing).await?
        };
        let items = page
            .iter()
            .filter_map(|r| match r.get("track") {
                Some(t) => track(t, None),
                None => st(r, "id").and_then(|id| {
                    fetched
                        .iter()
                        .find(|t| t.id == format!("{PREFIX}{id}"))
                        .cloned()
                }),
            })
            .collect();
        Ok(Page {
            items,
            total: refs.len() as u32,
            offset,
        })
    }

    pub async fn album(&self, album_id: &str) -> Result<Album> {
        let v = self
            .get(&format!("albums/{}/with-tracks", raw(album_id)), &[], true)
            .await?;
        let (aref, artists, release, kind, tc) =
            simple_album(&v).ok_or_else(|| YandexError::Api("bad album".into()))?;
        let tracks: Vec<Track> = arr(&v, "volumes")
            .iter()
            .flat_map(|vol| vol.as_array().cloned().unwrap_or_default())
            .filter_map(|t| {
                track(&t, Some(&aref)).map(|mut tr| {
                    tr.album = Some(aref.clone());
                    tr
                })
            })
            .collect();
        let total = if tc > 0 { tc } else { tracks.len() as u32 };
        Ok(Album {
            id: aref.id.clone(),
            name: aref.name.clone(),
            artists,
            images: aref.images.clone(),
            release_date: release,
            album_type: Some(kind),
            tracks,
            total_tracks: total,
        })
    }

    /// Artist name, picture and popular tracks (`artists/<id>/brief-info`, anonymous, as YandexMusic.kt).
    pub async fn artist(&self, artist_id: &str) -> Result<ArtistPage> {
        let v = self
            .get(&format!("artists/{}/brief-info", raw(artist_id)), &[], true)
            .await?;
        let a = v
            .get("artist")
            .filter(|x| x.is_object())
            .ok_or_else(|| YandexError::Api("bad artist".into()))?;
        Ok(ArtistPage {
            id: format!("{PREFIX}{}", raw(artist_id)),
            name: st(a, "name").unwrap_or_default(),
            images: cover_images(
                a.get("cover")
                    .and_then(|c| st(c, "uri"))
                    .or_else(|| st(a, "ogImage")),
            ),
            top_tracks: arr(&v, "popularTracks")
                .iter()
                .filter_map(|t| track(t, None))
                .collect(),
            releases: {
                let mut r: Vec<Release> = arr(&v, "albums")
                    .iter()
                    .filter_map(simple_album)
                    .map(|(a, _, date, kind, _)| Release {
                        id: a.id,
                        name: a.name,
                        images: a.images,
                        date,
                        kind,
                    })
                    .collect();
                r.sort_by(|a, b| b.date.cmp(&a.date));
                r
            },
            related: Vec::new(),
        })
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<Track>> {
        let n = limit.to_string();
        let v = self
            .get(
                "search",
                &[
                    ("text", query),
                    ("type", "track"),
                    ("page", "0"),
                    ("page-size", &n),
                ],
                true,
            )
            .await?;
        Ok(v.get("tracks")
            .map(|t| arr(t, "results"))
            .unwrap_or(&[])
            .iter()
            .filter_map(|t| track(t, None))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_and_covers() {
        assert_eq!(parse_playlist_id("ym_12345_1003"), Some(("12345", "1003")));
        assert_eq!(parse_playlist_id("ym_liked"), None);
        assert_eq!(
            cover_url("avatars.yandex.net/get-music-content/1/%%", 400),
            "https://avatars.yandex.net/get-music-content/1/400x400"
        );
    }

    #[test]
    fn track_with_version_and_numeric_ids() {
        let v = json!({"id": 123, "title": "Song", "version": "Remix", "durationMs": 200000,
            "artists": [{"id": 7, "name": "A"}], "albums": [{"id": 9, "title": "Al", "coverUri": "x/%%"}]});
        let t = track(&v, None).unwrap();
        assert_eq!(t.id, "ym_123");
        assert_eq!(t.title, "Song (Remix)");
        assert_eq!(t.artists[0].id, "ym_7");
        assert_eq!(t.album.unwrap().images[1].url, "https://x/400x400");
    }

    /// Real account check with the token from an imported profile. DRX_TEST_PROFILE; counts only.
    #[test]
    fn real_session_works() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let profile = drx_core::paths::Profile::at(dir);
        let vault = drx_core::secrets::Vault::load(&profile).unwrap();
        let token = vault
            .pref(drx_core::keys::yandex::ACCESS_TOKEN)
            .map(str::to_string);
        assert!(token.is_some(), "no Yandex session in the profile");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let y = Yandex::new(Http::new().unwrap(), token, None);
            let me = y.account().await.unwrap();
            eprintln!("yandex uid_len={}", me.id.len());
            let liked = y.liked_songs(0, 5).await.unwrap();
            eprintln!(
                "yandex liked total={} page={}",
                liked.total,
                liked.items.len()
            );
            let pls = y.my_playlists().await.unwrap();
            eprintln!("yandex playlists={}", pls.len());
            if let Some(p) = pls.first() {
                let t = y.playlist_tracks(&p.id, 0, 10).await.unwrap();
                eprintln!(
                    "yandex first playlist total={} page={}",
                    t.total,
                    t.items.len()
                );
            }
            if let Some(a) = liked.items.first().and_then(|t| t.album.as_ref()) {
                let al = y.album(&a.id).await.unwrap();
                eprintln!("yandex album tracks={}", al.tracks.len());
            }
            let found = y.search("Sewerslvt goodbye", 5).await.unwrap();
            eprintln!("yandex search results={}", found.len());
        });
    }
}
