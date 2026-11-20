//! VK as a full catalog (PORTING-2.3.0 §5) and the API client the VK audio source also uses.
//! Port of the API parts of `resolver/providers/VkAudioProvider.kt` and `catalog/VkCatalog.kt`.
//!
//! The token belongs to the official Android client (client id 2274003), so every call sends its User-Agent and
//! API version 5.116. Calls are spaced 350 ms apart; error 6 (too many requests) waits a second and retries once.
//! Ids: track `src:VK:<owner>_<audio>[_<key>]` (same id as a VK track found by the resolver), playlist/album
//! `vk_<owner>_<id>[_<key>]`, artist `vkar_<id>`, «Моя музыка» `vk_liked`.

use crate::model::{
    AlbumRef, AlbumSummary, ArtistPage, ArtistRef, ArtistSummary, Image, Playlist, Release,
    SearchResults, Track, User,
};
use drx_core::http::{Http, HttpError, Method};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub const API_VERSION: &str = "5.116";
pub const CLIENT_USER_AGENT: &str = "VKAndroidApp/5.52-4543 (Android 5.1.1; SDK 22; x86_64; unknown Android SDK built for x86_64; en; 320x240)";
pub const LIKED_ID: &str = "vk_liked";
const MIN_CALL_SPACING: Duration = Duration::from_millis(350);
const TRACKS_PAGE: u32 = 500;
const MAX_MY_TRACKS: usize = 5000;
const MAX_PLAYLIST_TRACKS: u32 = 1000;
const PLAYLIST_PAGE: u32 = 100;

const AUTH_FAILED: i64 = 5;
const TOO_MANY_REQUESTS: i64 = 6;
const FLOOD_CONTROL: i64 = 9;

#[derive(Debug, thiserror::Error)]
pub enum VkError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("VK error {code}: {message}")]
    Api { code: i64, message: String },
    #[error("not signed in to VK")]
    NoAccount,
}

impl VkError {
    pub fn token_revoked(&self) -> bool {
        matches!(
            self,
            VkError::Api {
                code: AUTH_FAILED,
                ..
            }
        )
    }
    pub fn search_flooded(&self) -> bool {
        matches!(
            self,
            VkError::Api {
                code: FLOOD_CONTROL,
                ..
            }
        )
    }
}

pub type Result<T> = std::result::Result<T, VkError>;

/// A VK track with its stream URL when VK gave one (HLS `index.m3u8` or MP3).
#[derive(Debug, Clone, PartialEq)]
pub struct VkAudio {
    pub track: Track,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VkPlaylist {
    pub playlist: Playlist,
    pub owner_id: i64,
    pub playlist_id: i64,
    pub access_key: Option<String>,
    pub is_album: bool,
    pub year: Option<i32>,
    pub artist: String,
}

/// `vk_<owner>_<id>[_<key>]` → (owner, id, key). Owners of community albums are negative.
pub fn parse_playlist_id(id: &str) -> Option<(i64, i64, Option<&str>)> {
    let rest = id.strip_prefix("vk_")?;
    let mut parts = rest.splitn(3, '_');
    let owner = parts.next()?.parse().ok()?;
    let pid = parts.next()?.parse().ok()?;
    Some((owner, pid, parts.next()))
}

/// VK hands out HLS playlists; the same file usually exists as MP3 next to it (VkAudioProvider.toMp3Url).
pub fn to_mp3_url(url: &str) -> String {
    // /<hex>(/audios)?/<hex>/index.m3u8 → /<hex1>/<hex2>.mp3 keeping the first group as the prefix
    let Some(pos) = url.find("/index.m3u8") else {
        return url.to_string();
    };
    let before = &url[..pos];
    let mut segs: Vec<&str> = before.rsplitn(4, '/').collect();
    // segs: [hex2, maybe "audios", hex1, rest...] (reversed)
    if segs.len() < 3 {
        return url.to_string();
    }
    let hex2 = segs.remove(0);
    let is_hex = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit());
    if !is_hex(hex2) {
        return url.to_string();
    }
    let (prefix, had_audios) = if segs[0] == "audios" {
        (
            segs[1..]
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("/"),
            true,
        )
    } else {
        (
            segs.iter().rev().cloned().collect::<Vec<_>>().join("/"),
            false,
        )
    };
    // prefix ends with "/<hex1>" — drop it: the Kotlin regex replaces "/hex1(/audios)?/hex2/index.m3u8" with
    // "<group1>/<hex2>.mp3", where group1 is "/audios" or empty, so the hex1 segment disappears.
    let Some(cut) = prefix.rfind('/') else {
        return url.to_string();
    };
    let hex1 = &prefix[cut + 1..];
    if !is_hex(hex1) {
        return url.to_string();
    }
    let base = &prefix[..cut];
    let tail = &url[pos + "/index.m3u8".len()..];
    format!(
        "{base}{}/{hex2}.mp3{tail}",
        if had_audios { "/audios" } else { "" }
    )
}

fn image_of(thumb: Option<&Value>) -> Vec<Image> {
    let Some(t) = thumb else { return Vec::new() };
    ["photo_600", "photo_300"]
        .iter()
        .filter_map(|k| t.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .map(|u| Image {
            url: u.to_string(),
            width: None,
        })
        .take(1)
        .collect()
}

/// The biggest artist photo up to ~1000 px (VK sends `photo` or `photos[0].photo` as size lists).
fn artist_photo(o: &Value) -> Vec<Image> {
    let lists = [o.get("photo"), o.pointer("/photos/0/photo")];
    for list in lists.into_iter().flatten() {
        let Some(a) = list.as_array() else { continue };
        let sizes: Vec<(String, u32)> = a
            .iter()
            .filter_map(|x| {
                let url = x
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|u| !u.is_empty())?;
                Some((
                    url.to_string(),
                    x.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
                ))
            })
            .collect();
        let best = sizes
            .iter()
            .filter(|s| s.1 <= 1000)
            .max_by_key(|s| s.1)
            .or_else(|| sizes.first());
        if let Some((url, w)) = best {
            return vec![Image {
                url: url.clone(),
                width: Some(*w).filter(|&w| w > 0),
            }];
        }
    }
    // Older shape: an object with photo_600 / photo_300.
    o.get("photo")
        .filter(|p| p.is_object())
        .and_then(|p| nonempty(p, "photo_600").or_else(|| nonempty(p, "photo_300")))
        .map(|url| vec![Image { url, width: None }])
        .unwrap_or_default()
}

fn nonempty(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// One audio object (VkAudioProvider.parse).
pub fn parse_audio(o: &Value) -> Option<VkAudio> {
    let id = o.get("id").and_then(Value::as_i64).filter(|&x| x != 0)?;
    let owner = o
        .get("owner_id")
        .and_then(Value::as_i64)
        .filter(|&x| x != 0)?;
    let key = nonempty(o, "access_key");
    let subtitle = nonempty(o, "subtitle");
    let raw_title = o.get("title").and_then(Value::as_str).unwrap_or("");
    let title = match subtitle {
        Some(s) if !raw_title.to_lowercase().contains(&s.to_lowercase()) => {
            format!("{raw_title} ({s})")
        }
        _ => raw_title.to_string(),
    };
    let mut artists: Vec<ArtistRef> = Vec::new();
    for k in ["main_artists", "featured_artists"] {
        for a in o
            .get(k)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let Some(name) = nonempty(a, "name") else {
                continue;
            };
            if artists
                .iter()
                .any(|x| x.name.to_lowercase() == name.to_lowercase())
            {
                continue;
            }
            let id = nonempty(a, "id")
                .filter(|i| i != "0")
                .map(|i| format!("vkar_{i}"))
                .unwrap_or_default();
            artists.push(ArtistRef { id, name });
        }
    }
    if artists.is_empty() {
        artists.push(ArtistRef {
            id: String::new(),
            name: o
                .get("artist")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    let album = o.get("album").filter(|a| a.is_object()).and_then(|a| {
        let ao = a
            .get("owner_id")
            .and_then(Value::as_i64)
            .filter(|&x| x != 0)?;
        let ai = a.get("id").and_then(Value::as_i64).filter(|&x| x != 0)?;
        let mut id = format!("vk_{ao}_{ai}");
        if let Some(k) = nonempty(a, "access_key") {
            id.push('_');
            id.push_str(&k);
        }
        Some(AlbumRef {
            id,
            name: a
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            images: image_of(a.get("thumb")),
        })
    });
    let full = match &key {
        Some(k) => format!("{owner}_{id}_{k}"),
        None => format!("{owner}_{id}"),
    };
    Some(VkAudio {
        track: Track {
            id: format!("src:VK:{full}"),
            title,
            artists,
            album,
            duration_ms: o.get("duration").and_then(Value::as_u64).unwrap_or(0) * 1000,
            isrc: None,
            plays: None,
            explicit: o
                .get("is_explicit")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        url: nonempty(o, "url"),
    })
}

/// Album or playlist (VkAudioProvider.isAlbum): people own playlists; VK's catalog (negative owner) owns
/// releases, marked by `album_type`/`type == 1`, or by artists plus a year.
fn is_album(o: &Value, owner: i64, has_artists: bool) -> bool {
    if owner > 0 {
        return false;
    }
    if let Some(t) = nonempty(o, "album_type")
        && !t.eq_ignore_ascii_case("playlist")
    {
        return true;
    }
    if o.get("type").and_then(Value::as_i64) == Some(1) {
        return true;
    }
    has_artists && o.get("year").and_then(Value::as_i64).unwrap_or(0) > 0
}

pub fn parse_playlist(o: &Value) -> Option<VkPlaylist> {
    let original = o.get("original").filter(|x| x.is_object());
    let owner = original
        .and_then(|x| x.get("owner_id"))
        .and_then(Value::as_i64)
        .filter(|&x| x != 0)
        .or_else(|| o.get("owner_id").and_then(Value::as_i64))
        .filter(|&x| x != 0)?;
    let pid = original
        .and_then(|x| x.get("playlist_id"))
        .and_then(Value::as_i64)
        .filter(|&x| x != 0)
        .or_else(|| o.get("id").and_then(Value::as_i64))
        .filter(|&x| x != 0)?;
    let key = original
        .and_then(|x| nonempty(x, "access_key"))
        .or_else(|| nonempty(o, "access_key"));
    let artists: Vec<String> = o
        .get("main_artists")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| nonempty(a, "name"))
        .collect();
    let images = {
        let p = image_of(o.get("photo"));
        if p.is_empty() {
            image_of(o.get("thumbs").and_then(|t| t.get(0)))
        } else {
            p
        }
    };
    let mut id = format!("vk_{owner}_{pid}");
    if let Some(k) = &key {
        id.push('_');
        id.push_str(k);
    }
    Some(VkPlaylist {
        playlist: Playlist {
            id,
            name: o
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            description: nonempty(o, "description"),
            owner: None,
            images,
            total: o.get("count").and_then(Value::as_u64).unwrap_or(0) as u32,
        },
        owner_id: owner,
        playlist_id: pid,
        access_key: key,
        is_album: is_album(o, owner, !artists.is_empty()),
        year: o
            .get("year")
            .and_then(Value::as_i64)
            .filter(|&y| y > 0)
            .map(|y| y as i32),
        artist: artists.join(", "),
    })
}

fn items(v: &Value) -> Vec<&Value> {
    match v.get("response") {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(o @ Value::Object(_)) => o
            .get("items")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub struct Vk {
    http: Http,
    token: Option<String>,
    user_id: Mutex<Option<i64>>,
    last_call: Mutex<Instant>,
}

impl Vk {
    pub fn new(http: Http, token: Option<String>, user_id: Option<i64>) -> Self {
        Self {
            http,
            token,
            user_id: Mutex::new(user_id),
            last_call: Mutex::new(Instant::now() - MIN_CALL_SPACING),
        }
    }

    pub fn has_account(&self) -> bool {
        self.token.is_some()
    }

    /// One API method call (POST form with access_token and v).
    pub async fn call(&self, method: &str, params: &[(&str, String)]) -> Result<Value> {
        let token = self.token.as_deref().ok_or(VkError::NoAccount)?;
        for attempt in 0..2 {
            {
                let mut last = self.last_call.lock().await;
                let wait = MIN_CALL_SPACING.saturating_sub(last.elapsed());
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
                *last = Instant::now();
            }
            let mut form: Vec<(&str, &str)> =
                params.iter().map(|(k, v)| (*k, v.as_str())).collect();
            form.push(("access_token", token));
            form.push(("v", API_VERSION));
            let req = self
                .http
                .request(Method::POST, &format!("https://api.vk.com/method/{method}"))
                .header("User-Agent", CLIENT_USER_AGENT)
                .form(&form);
            let v: Value = self
                .http
                .send(req)
                .await?
                .json()
                .await
                .map_err(HttpError::from)?;
            if let Some(err) = v.get("error") {
                let code = err.get("error_code").and_then(Value::as_i64).unwrap_or(0);
                if code == TOO_MANY_REQUESTS && attempt == 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                return Err(VkError::Api {
                    code,
                    message: err
                        .get("error_msg")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
            }
            return Ok(v);
        }
        Err(VkError::Api {
            code: TOO_MANY_REQUESTS,
            message: "too many requests".into(),
        })
    }

    pub async fn me(&self) -> Result<User> {
        let v = self
            .call("users.get", &[("fields", "photo_200".into())])
            .await?;
        let u = v
            .get("response")
            .and_then(|r| r.get(0))
            .ok_or(VkError::Api {
                code: 0,
                message: "no user".into(),
            })?;
        let id = u.get("id").and_then(Value::as_i64).unwrap_or(0);
        *self.user_id.lock().await = Some(id);
        let name = format!(
            "{} {}",
            u.get("first_name").and_then(Value::as_str).unwrap_or(""),
            u.get("last_name").and_then(Value::as_str).unwrap_or("")
        );
        Ok(User {
            id: id.to_string(),
            name: Some(name.trim().to_string()),
            images: nonempty(u, "photo_200")
                .map(|url| {
                    vec![Image {
                        url,
                        width: Some(200),
                    }]
                })
                .unwrap_or_default(),
        })
    }

    async fn owner(&self) -> Result<i64> {
        if let Some(id) = *self.user_id.lock().await {
            return Ok(id);
        }
        Ok(self.me().await?.id.parse().unwrap_or(0))
    }

    /// «Моя музыка», newest first, up to 5000 tracks.
    pub async fn my_tracks(&self) -> Result<Vec<VkAudio>> {
        let owner = self.owner().await?.to_string();
        let mut all: Vec<VkAudio> = Vec::new();
        let mut offset = 0u32;
        loop {
            let v = self
                .call(
                    "audio.get",
                    &[
                        ("owner_id", owner.clone()),
                        ("count", TRACKS_PAGE.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let page: Vec<VkAudio> = items(&v).into_iter().filter_map(parse_audio).collect();
            let total = v
                .get("response")
                .and_then(|r| r.get("count"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let empty = page.is_empty();
            all.extend(page);
            offset += TRACKS_PAGE;
            if empty || offset >= total || all.len() >= MAX_MY_TRACKS {
                break;
            }
        }
        let mut seen = std::collections::HashSet::new();
        all.retain(|a| seen.insert(a.track.id.clone()));
        Ok(all)
    }

    pub async fn my_playlists(&self) -> Result<Vec<VkPlaylist>> {
        let owner = self.owner().await?.to_string();
        let mut all: Vec<VkPlaylist> = Vec::new();
        let mut offset = 0u32;
        loop {
            let v = self
                .call(
                    "audio.getPlaylists",
                    &[
                        ("owner_id", owner.clone()),
                        ("count", PLAYLIST_PAGE.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let page: Vec<VkPlaylist> = items(&v).into_iter().filter_map(parse_playlist).collect();
            let total = v
                .get("response")
                .and_then(|r| r.get("count"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let empty = page.is_empty();
            all.extend(page);
            offset += PLAYLIST_PAGE;
            if empty || offset >= total || all.len() >= 1000 {
                break;
            }
        }
        // VK repeats an album now and then; one entry each.
        let mut seen = std::collections::HashSet::new();
        all.retain(|p| seen.insert((p.owner_id, p.playlist_id)));
        Ok(all)
    }

    /// The user's own id (owner of «Моя музыка»).
    pub async fn my_id(&self) -> Result<i64> {
        self.owner().await
    }

    /// Adds a track to «Моя музыка», as VK's «+» (VkAudioProvider.addTrack). `full_id` is
    /// `owner_audio[_key]`; returns the address (owner, id) of the user's copy.
    pub async fn add_track(&self, full_id: &str) -> Result<(i64, i64)> {
        let parts: Vec<&str> = full_id.split('_').collect();
        if parts.len() < 2 {
            return Err(VkError::Api {
                code: 0,
                message: "bad VK track id".into(),
            });
        }
        let mut params = vec![
            ("owner_id", parts[0].to_string()),
            ("audio_id", parts[1].to_string()),
        ];
        if let Some(k) = parts.get(2) {
            params.push(("access_key", k.to_string()));
        }
        let v = self.call("audio.add", &params).await?;
        let copy = v.get("response").and_then(Value::as_i64).unwrap_or(0);
        Ok((self.owner().await?, copy))
    }

    /// Removes a track from «Моя музыка» by the address of the user's copy.
    pub async fn delete_track(&self, owner: i64, audio_id: i64) -> Result<()> {
        self.call(
            "audio.delete",
            &[
                ("owner_id", owner.to_string()),
                ("audio_id", audio_id.to_string()),
            ],
        )
        .await?;
        Ok(())
    }

    /// Adds an album or playlist to the user's list (`audio.followPlaylist`); returns the copy's address.
    pub async fn follow_playlist(
        &self,
        owner: i64,
        playlist_id: i64,
        access_key: Option<&str>,
    ) -> Result<(i64, i64)> {
        let mut params = vec![
            ("owner_id", owner.to_string()),
            ("playlist_id", playlist_id.to_string()),
        ];
        if let Some(k) = access_key {
            params.push(("access_key", k.to_string()));
        }
        let v = self.call("audio.followPlaylist", &params).await?;
        let r = v.get("response");
        let o = r
            .and_then(|r| r.get("owner_id"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let p = r
            .and_then(|r| r.get("playlist_id"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        Ok((o, p))
    }

    /// Removes the user's copy of an album or playlist (`audio.deletePlaylist`).
    pub async fn delete_playlist(&self, owner: i64, playlist_id: i64) -> Result<()> {
        self.call(
            "audio.deletePlaylist",
            &[
                ("owner_id", owner.to_string()),
                ("playlist_id", playlist_id.to_string()),
            ],
        )
        .await?;
        Ok(())
    }

    /// Every track of an album or playlist.
    pub async fn playlist_tracks(
        &self,
        owner: i64,
        playlist_id: i64,
        access_key: Option<&str>,
    ) -> Result<Vec<VkAudio>> {
        let mut params = vec![
            ("owner_id", owner.to_string()),
            ("album_id", playlist_id.to_string()),
            ("count", MAX_PLAYLIST_TRACKS.to_string()),
        ];
        if let Some(k) = access_key {
            params.push(("access_key", k.to_string()));
        }
        let v = self.call("audio.get", &params).await?;
        Ok(items(&v).into_iter().filter_map(parse_audio).collect())
    }

    /// Tracks by full ids ("owner_audio[_key]"), 100 per call.
    pub async fn tracks_by_id(&self, full_ids: &[String]) -> Result<Vec<VkAudio>> {
        let mut out = Vec::new();
        for chunk in full_ids.chunks(100) {
            let v = self
                .call("audio.getById", &[("audios", chunk.join(","))])
                .await?;
            out.extend(items(&v).into_iter().filter_map(parse_audio));
        }
        Ok(out)
    }

    /// Artist name and photo (`audio.getArtistById`) and their tracks (`audio.getAudiosByArtist`, VkAudioProvider.kt).
    /// `artist_id` is `vkar_<id>`. Tracks keep their URLs (they play without a search).
    pub async fn artist(&self, artist_id: &str) -> Result<(ArtistPage, Vec<VkAudio>)> {
        let raw = artist_id
            .strip_prefix("vkar_")
            .unwrap_or(artist_id)
            .to_string();
        let info = self
            .call("audio.getArtistById", &[("artist_id", raw.clone())])
            .await?;
        let r = info.get("response").cloned().unwrap_or_default();
        let list = self
            .call(
                "audio.getAudiosByArtist",
                &[("artist_id", raw.clone()), ("count", "50".into())],
            )
            .await?;
        // Releases are optional: the page still opens without them.
        let albums = self
            .call(
                "audio.getAlbumsByArtist",
                &[("artist_id", raw.clone()), ("count", "100".into())],
            )
            .await
            .map(|v| {
                items(&v)
                    .into_iter()
                    .filter_map(parse_playlist)
                    .map(|p| Release {
                        id: p.playlist.id,
                        name: p.playlist.name,
                        images: p.playlist.images,
                        date: p.year.map(|y| y.to_string()),
                        kind: "album".into(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let audios: Vec<VkAudio> = items(&list)
            .into_iter()
            .filter_map(parse_audio)
            .filter(|a| seen.insert(a.track.id.clone()))
            .collect();
        let page = ArtistPage {
            id: format!("vkar_{raw}"),
            name: nonempty(&r, "name").unwrap_or_default(),
            images: artist_photo(&r),
            top_tracks: audios.iter().map(|a| a.track.clone()).collect(),
            releases: {
                let mut r = albums;
                r.sort_by(|a, b| b.date.cmp(&a.date));
                r
            },
            related: Vec::new(),
            appears_on: Vec::new(),
            monthly_listeners: None,
            genres: Vec::new(),
        };
        Ok((page, audios))
    }

    /// Everything at once: tracks (`audio.search`), artists, albums and playlists (`audio.searchArtists`,
    /// `searchAlbums`, `searchPlaylists`). Tracks come back with their URLs; a failing section is just empty.
    pub async fn search_all(&self, text: &str) -> Result<(SearchResults, Vec<VkAudio>)> {
        let audios = self.search(text, 40).await?;
        let q = || vec![("q", text.to_string()), ("count", "20".to_string())];
        let artists = self
            .call("audio.searchArtists", &q())
            .await
            .map(|v| {
                items(&v)
                    .into_iter()
                    .filter_map(|a| {
                        let id = nonempty(a, "id")?;
                        Some(ArtistSummary {
                            id: format!("vkar_{id}"),
                            name: nonempty(a, "name")?,
                            images: artist_photo(a),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let playlists_of = |v: Value| -> Vec<VkPlaylist> {
            items(&v).into_iter().filter_map(parse_playlist).collect()
        };
        let albums = self
            .call("audio.searchAlbums", &q())
            .await
            .map(playlists_of)
            .unwrap_or_default()
            .into_iter()
            .map(|p| AlbumSummary {
                id: p.playlist.id,
                name: p.playlist.name,
                artists: vec![ArtistRef {
                    id: String::new(),
                    name: p.artist,
                }],
                images: p.playlist.images,
                date: p.year.map(|y| y.to_string()),
                kind: "album".into(),
            })
            .collect();
        let playlists = self
            .call("audio.searchPlaylists", &q())
            .await
            .map(playlists_of)
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.playlist)
            .collect();
        let results = SearchResults {
            tracks: audios.iter().map(|a| a.track.clone()).collect(),
            artists,
            albums,
            playlists,
        };
        Ok((results, audios))
    }

    pub async fn search(&self, text: &str, count: u32) -> Result<Vec<VkAudio>> {
        let v = self
            .call(
                "audio.search",
                &[
                    ("q", text.to_string()),
                    ("count", count.to_string()),
                    ("sort", "2".into()),
                    ("auto_complete", "1".into()),
                ],
            )
            .await?;
        Ok(items(&v).into_iter().filter_map(parse_audio).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mp3_url_like_android() {
        assert_eq!(
            to_mp3_url("https://cs1.vkuseraudio.net/s/v1/ac/abc123/def456/index.m3u8?extra=1"),
            "https://cs1.vkuseraudio.net/s/v1/ac/def456.mp3?extra=1"
        );
        assert_eq!(
            to_mp3_url("https://psv4.vkuseraudio.net/s/v1/a2/aa11/audios/bb22/index.m3u8?x"),
            "https://psv4.vkuseraudio.net/s/v1/a2/audios/bb22.mp3?x"
        );
        assert_eq!(to_mp3_url("https://x/y.mp3"), "https://x/y.mp3");
    }

    #[test]
    fn parses_official_track_with_album() {
        let o = json!({"id": 456, "owner_id": -2001, "access_key": "kk", "title": "Song", "subtitle": "Remix", "duration": 200,
            "main_artists": [{"name": "A", "id": "123"}], "featured_artists": [{"name": "a", "id": "9"}, {"name": "B", "id": "0"}],
            "album": {"owner_id": -2000, "id": 77, "access_key": "ak", "title": "Al", "thumb": {"photo_600": "https://p/600"}},
            "url": "https://x/index.m3u8"});
        let a = parse_audio(&o).unwrap();
        assert_eq!(a.track.id, "src:VK:-2001_456_kk");
        assert_eq!(a.track.title, "Song (Remix)");
        assert_eq!(a.track.artists.len(), 2, "duplicate names dropped");
        assert_eq!(a.track.artists[0].id, "vkar_123");
        assert_eq!(a.track.artists[1].id, "", "artist id 0 means none");
        assert_eq!(a.track.album.as_ref().unwrap().id, "vk_-2000_77_ak");
        assert_eq!(a.track.duration_ms, 200_000);
    }

    #[test]
    fn album_or_playlist_rule() {
        let user_pl = json!({"id": 1, "owner_id": 5, "title": "mine", "album_type": "main_only"});
        assert!(
            !parse_playlist(&user_pl).unwrap().is_album,
            "people own playlists"
        );
        let rel = json!({"id": 2, "owner_id": -10, "title": "LP", "main_artists": [{"name": "A"}], "year": 2021});
        assert!(parse_playlist(&rel).unwrap().is_album);
        let editorial = json!({"id": 3, "owner_id": -10, "title": "Hits"});
        assert!(!parse_playlist(&editorial).unwrap().is_album);
        let reference = json!({"id": 9, "owner_id": 5, "original": {"owner_id": -10, "playlist_id": 2, "access_key": "z"}, "album_type": "album"});
        let p = parse_playlist(&reference).unwrap();
        assert_eq!(p.playlist.id, "vk_-10_2_z");
        assert!(p.is_album);
        assert_eq!(parse_playlist_id("vk_-10_2_z"), Some((-10, 2, Some("z"))));
    }

    /// Real account check. DRX_TEST_PROFILE; counts only.
    #[test]
    fn real_session_works() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let profile = drx_core::paths::Profile::at(dir);
        let vault = drx_core::secrets::Vault::load(&profile).unwrap();
        let token = vault
            .pref(drx_core::keys::vk::ACCESS_TOKEN)
            .map(str::to_string);
        assert!(token.is_some(), "no VK session in the profile");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let vk = Vk::new(Http::new().unwrap(), token, None);
            let me = vk.me().await.unwrap();
            eprintln!("vk id_len={}", me.id.len());
            let mine = vk.my_tracks().await.unwrap();
            eprintln!(
                "vk my tracks={} with_url={}",
                mine.len(),
                mine.iter().filter(|a| a.url.is_some()).count()
            );
            let pls = vk.my_playlists().await.unwrap();
            eprintln!(
                "vk playlists={} albums={}",
                pls.len(),
                pls.iter().filter(|p| p.is_album).count()
            );
            if let Some(p) = pls.first() {
                let t = vk
                    .playlist_tracks(p.owner_id, p.playlist_id, p.access_key.as_deref())
                    .await
                    .unwrap();
                eprintln!("vk first playlist tracks={}", t.len());
            }
        });
    }
}
