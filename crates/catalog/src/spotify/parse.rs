//! GraphQL answer → catalog models (port of the parsers in Spotify.kt). Every field is optional: Spotify moves
//! fields around between web-player releases.

use crate::js::{arr, i, obj, path, s};
use crate::model::{
    Album, AlbumRef, AlbumSummary, ArtistPage, ArtistRef, ArtistSummary, Image, Page, Playlist,
    Release, SearchResults, Track, User,
};
use serde_json::Value;

fn id_of(uri: &str) -> String {
    uri.rsplit(':').next().unwrap_or(uri).to_string()
}

pub fn images(sources: &[Value]) -> Vec<Image> {
    sources
        .iter()
        .filter_map(|x| {
            Some(Image {
                url: s(x, "url")?.to_string(),
                width: i(x, "width").map(|w| w as u32),
            })
        })
        .collect()
}

fn artist(a: &Value) -> Option<ArtistRef> {
    let uri = s(a, "uri")?;
    Some(ArtistRef {
        id: id_of(uri),
        name: path(a, &["profile", "name"])
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn duration_ms(t: &Value) -> u64 {
    if let Some(ms) = path(t, &["duration", "totalMilliseconds"])
        .and_then(Value::as_i64)
        .filter(|&m| m > 0)
    {
        return ms as u64;
    }
    for k in ["durationMs", "duration_ms"] {
        if let Some(ms) = i(t, k).filter(|&m| m > 0) {
            return ms as u64;
        }
    }
    i(t, "duration")
        .filter(|&s| s > 0)
        .map(|s| s as u64 * 1000)
        .unwrap_or(0)
}

/// The track structure shared by fetchPlaylist, fetchLibraryTracks, getAlbum, userTopContent (parseGqlTrack).
pub fn track(
    t: &Value,
    uri_override: Option<&str>,
    album_override: Option<&AlbumRef>,
) -> Option<Track> {
    let uri = uri_override
        .or_else(|| s(t, "uri"))
        .or_else(|| s(t, "_uri"))?;
    let artists = obj(t, "artists")
        .map(|a| arr(a, "items").iter().filter_map(artist).collect())
        .unwrap_or_default();
    let album = album_override.cloned().or_else(|| {
        let a = obj(t, "albumOfTrack")?;
        Some(AlbumRef {
            id: id_of(s(a, "uri").unwrap_or("")),
            name: s(a, "name").unwrap_or("").to_string(),
            images: obj(a, "coverArt")
                .map(|c| images(arr(c, "sources")))
                .unwrap_or_default(),
        })
    });
    Some(Track {
        id: id_of(uri),
        title: s(t, "name").unwrap_or("").to_string(),
        artists,
        album,
        duration_ms: duration_ms(t),
        isrc: None,
        plays: s(t, "playcount")
            .or_else(|| s(t, "playCount"))
            .and_then(|x| x.parse().ok()),
        explicit: path(t, &["contentRating", "label"]).and_then(Value::as_str) == Some("EXPLICIT"),
    })
}

pub fn user(v: &Value) -> Option<User> {
    let p = path(v, &["data", "me", "profile"])?;
    Some(User {
        id: id_of(s(p, "uri").unwrap_or("")),
        name: s(p, "name").map(str::to_string),
        images: obj(p, "avatar")
            .map(|a| images(arr(a, "sources")))
            .unwrap_or_default(),
    })
}

pub fn liked(v: &Value, offset: u32) -> Option<Page<Track>> {
    let t = path(v, &["data", "me", "library", "tracks"])?;
    let items = arr(t, "items")
        .iter()
        .filter_map(|e| {
            let w = obj(e, "track")?;
            let data = obj(w, "data")?;
            track(data, s(w, "_uri").or_else(|| s(w, "uri")), None)
        })
        .collect();
    Some(Page {
        items,
        total: i(t, "totalCount").unwrap_or(0) as u32,
        offset,
    })
}

/// Ids of one kind ("album", "artist") in a `libraryV3` page, with the page's total.
pub fn library_ids(v: &Value, kind: &str) -> Option<(Vec<String>, u32)> {
    let lib = path(v, &["data", "me", "libraryV3"])?;
    let prefix = format!("spotify:{kind}:");
    let ids = arr(lib, "items")
        .iter()
        .filter_map(|e| {
            let uri = s(obj(e, "item")?, "_uri")?;
            uri.strip_prefix(prefix.as_str()).map(str::to_string)
        })
        .collect();
    Some((ids, i(lib, "totalCount").unwrap_or(0) as u32))
}

pub fn library_playlists(v: &Value, offset: u32) -> Option<Page<Playlist>> {
    let lib = path(v, &["data", "me", "libraryV3"])?;
    let items = arr(lib, "items")
        .iter()
        .filter_map(|e| {
            let w = obj(e, "item")?;
            let tn = s(w, "__typename").unwrap_or("");
            if !tn.contains("Playlist") {
                return None;
            }
            let data = obj(w, "data")?;
            if s(data, "__typename") != Some("Playlist") {
                return None;
            }
            let uri = s(w, "_uri")?;
            let imgs = obj(data, "images")
                .map(|im| {
                    arr(im, "items")
                        .iter()
                        .flat_map(|g| images(arr(g, "sources")))
                        .collect()
                })
                .unwrap_or_default();
            Some(Playlist {
                id: id_of(uri),
                name: s(data, "name").unwrap_or("").to_string(),
                description: s(data, "description").map(str::to_string),
                owner: path(data, &["ownerV2", "data", "name"])
                    .and_then(Value::as_str)
                    .map(str::to_string),
                images: imgs,
                total: 0,
            })
        })
        .collect();
    Some(Page {
        items,
        total: i(lib, "totalCount").unwrap_or(0) as u32,
        offset,
    })
}

pub fn playlist_tracks(v: &Value, offset: u32) -> Option<Page<Track>> {
    let c = path(v, &["data", "playlistV2", "content"])?;
    let items = arr(c, "items")
        .iter()
        .filter_map(|e| {
            let w = obj(e, "itemV2")?;
            let data = obj(w, "data")?;
            track(data, s(w, "_uri").or_else(|| s(w, "uri")), None)
        })
        .collect();
    Some(Page {
        items,
        total: i(c, "totalCount").unwrap_or(0) as u32,
        offset,
    })
}

pub fn album(v: &Value, album_id: &str) -> Option<Album> {
    let a = path(v, &["data", "albumUnion"])?;
    let artists: Vec<ArtistRef> = obj(a, "artists")
        .map(|x| arr(x, "items").iter().filter_map(artist).collect())
        .unwrap_or_default();
    let imgs = obj(a, "coverArt")
        .map(|c| images(arr(c, "sources")))
        .unwrap_or_default();
    let aref = AlbumRef {
        id: album_id.to_string(),
        name: s(a, "name").unwrap_or("").to_string(),
        images: imgs.clone(),
    };
    let tv2 = obj(a, "tracksV2");
    let tracks = tv2
        .map(|t| {
            arr(t, "items")
                .iter()
                .filter_map(|e| track(obj(e, "track")?, None, Some(&aref)))
                .collect()
        })
        .unwrap_or_default();
    Some(Album {
        id: album_id.to_string(),
        name: aref.name.clone(),
        artists,
        images: imgs,
        release_date: path(a, &["date", "isoString"])
            .and_then(Value::as_str)
            .map(str::to_string),
        album_type: s(a, "type").map(str::to_lowercase),
        tracks,
        total_tracks: tv2.and_then(|t| i(t, "totalCount")).unwrap_or(0) as u32,
    })
}

/// queryArtistOverview → name, avatar, popular tracks (artistUnion; Spotify.artist + artistTopTracks).
pub fn artist_page(v: &Value, artist_id: &str) -> Option<ArtistPage> {
    let a = path(v, &["data", "artistUnion"])?;
    let images = path(a, &["visuals", "avatarImage"])
        .map(|x| images(arr(x, "sources")))
        .unwrap_or_default();
    let top_tracks = path(a, &["discography", "topTracks"])
        .map(|t| {
            arr(t, "items")
                .iter()
                .filter_map(|e| track(obj(e, "track")?, None, None))
                .collect()
        })
        .unwrap_or_default();
    Some(ArtistPage {
        id: artist_id.to_string(),
        name: path(a, &["profile", "name"])
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        images,
        top_tracks,
        releases: releases(a),
        related: path(a, &["relatedContent", "relatedArtists"])
            .map(|r| arr(r, "items").iter().filter_map(artist_summary).collect())
            .unwrap_or_default(),
        appears_on: appears_on(a),
        monthly_listeners: path(a, &["stats", "monthlyListeners"]).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|x| x.parse().ok()))
        }),
        genres: Vec::new(),
    })
}

/// relatedContent.appearsOn: releases of others with this artist (Spotify.artistAppearsOn), with their artists.
fn appears_on(a: &Value) -> Vec<AlbumSummary> {
    let mut out: Vec<AlbumSummary> = path(a, &["relatedContent", "appearsOn"])
        .map(|g| {
            arr(g, "items")
                .iter()
                .filter_map(|e| {
                    let r = obj(e, "releases")
                        .and_then(|x| arr(x, "items").first())
                        .unwrap_or(e);
                    Some(AlbumSummary {
                        id: id_of(s(r, "uri")?),
                        name: s(r, "name")?.to_string(),
                        artists: obj(r, "artists")
                            .map(|x| arr(x, "items").iter().filter_map(artist).collect())
                            .unwrap_or_default(),
                        images: obj(r, "coverArt")
                            .map(|c| images(arr(c, "sources")))
                            .unwrap_or_default(),
                        date: obj(r, "date").and_then(release_date),
                        kind: s(r, "type")
                            .map(str::to_lowercase)
                            .unwrap_or_else(|| "album".into()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    out.retain(|x| seen.insert(x.id.clone()));
    out
}

/// `{uri, profile.name, visuals.avatarImage}` → artist summary (related artists, search).
fn artist_summary(a: &Value) -> Option<ArtistSummary> {
    let id = id_of(s(a, "uri")?);
    let name = path(a, &["profile", "name"]).and_then(Value::as_str)?;
    (!id.is_empty()).then(|| ArtistSummary {
        id,
        name: name.to_string(),
        images: path(a, &["visuals", "avatarImage"])
            .map(|x| images(arr(x, "sources")))
            .unwrap_or_default(),
    })
}

/// Artists from a searchDesktop answer.
pub fn search_artists(v: &Value) -> Vec<ArtistSummary> {
    path(v, &["data", "searchV2", "artists"])
        .map(|t| {
            arr(t, "items")
                .iter()
                .filter_map(|e| {
                    if s(e, "__typename") != Some("ArtistResponseWrapper") {
                        return None;
                    }
                    let d = obj(e, "data")?;
                    (s(d, "__typename") == Some("Artist"))
                        .then(|| artist_summary(d))
                        .flatten()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A release date as precise as Spotify knows it (gqlReleaseDate): "2025-03-14" when the day is known, else the year.
fn release_date(d: &Value) -> Option<String> {
    let Some(year) = i(d, "year") else {
        return s(d, "isoString").map(|x| x.chars().take(4).collect());
    };
    let precision = s(d, "precision");
    if precision.is_none() || precision == Some("DAY") {
        if let Some(iso) = s(d, "isoString")
            && iso.len() >= 10
            && iso.starts_with(&year.to_string())
        {
            return Some(iso[..10].to_string());
        }
        if let (Some(m), Some(day)) = (i(d, "month"), i(d, "day")) {
            return Some(format!("{year:04}-{m:02}-{day:02}"));
        }
    }
    Some(year.to_string())
}

/// discography.{latest, albums, singles, compilations} → releases, newest first (artistDiscography).
fn releases(a: &Value) -> Vec<Release> {
    let Some(d) = obj(a, "discography") else {
        return Vec::new();
    };
    let one = |r: &Value, kind: &str| -> Option<Release> {
        let uri = s(r, "uri")?;
        let id = id_of(uri);
        if id.is_empty() {
            return None;
        }
        let date = obj(r, "date").and_then(release_date);
        Some(Release {
            id,
            name: s(r, "name")?.to_string(),
            images: obj(r, "coverArt")
                .map(|c| images(arr(c, "sources")))
                .unwrap_or_default(),
            date,
            kind: s(r, "type")
                .map(str::to_lowercase)
                .unwrap_or_else(|| kind.to_string()),
        })
    };
    let mut out: Vec<Release> = Vec::new();
    if let Some(l) = obj(d, "latest") {
        out.extend(one(l, "album"));
    }
    for (key, kind) in [
        ("albums", "album"),
        ("singles", "single"),
        ("compilations", "compilation"),
    ] {
        if let Some(g) = obj(d, key) {
            for e in arr(g, "items") {
                // Each item wraps releases.items[]; the first is the main release.
                let r = obj(e, "releases")
                    .and_then(|x| arr(x, "items").first())
                    .unwrap_or(e);
                if let Some(rel) = one(r, kind) {
                    out.push(rel);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(r.id.clone()));
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

pub fn search_tracks(v: &Value) -> Vec<Track> {
    path(v, &["data", "searchV2", "tracksV2"])
        .map(|t| {
            arr(t, "items")
                .iter()
                .filter_map(|e| {
                    let w = obj(e, "item")?;
                    if s(w, "__typename") != Some("TrackResponseWrapper") {
                        return None;
                    }
                    let d = obj(w, "data")?;
                    if s(d, "__typename") != Some("Track") {
                        return None;
                    }
                    track(d, s(w, "_uri").or_else(|| s(w, "uri")), None)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn top_tracks(v: &Value) -> Vec<Track> {
    path(v, &["data", "me", "profile", "topTracks"])
        .map(|p| {
            arr(p, "items")
                .iter()
                .filter_map(|e| {
                    let d = obj(e, "data")?;
                    (s(d, "__typename") == Some("Track"))
                        .then(|| track(d, None, None))
                        .flatten()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every section of a searchDesktop answer: tracks, artists, albums, playlists.
pub fn search_all(v: &Value) -> SearchResults {
    let Some(sv) = path(v, &["data", "searchV2"]) else {
        return SearchResults::default();
    };
    let items = |key: &str, wrapper: &str, kind: &str| -> Vec<&Value> {
        obj(sv, key)
            .map(|g| {
                arr(g, "items")
                    .iter()
                    .filter(|e| s(e, "__typename") == Some(wrapper))
                    .filter_map(|e| obj(e, "data"))
                    .filter(|d| s(d, "__typename") == Some(kind))
                    .collect()
            })
            .unwrap_or_default()
    };
    let albums = items("albumsV2", "AlbumResponseWrapper", "Album")
        .into_iter()
        .filter_map(|d| {
            Some(AlbumSummary {
                id: id_of(s(d, "uri")?),
                name: s(d, "name")?.to_string(),
                artists: obj(d, "artists")
                    .map(|a| arr(a, "items").iter().filter_map(artist).collect())
                    .unwrap_or_default(),
                images: obj(d, "coverArt")
                    .map(|c| images(arr(c, "sources")))
                    .unwrap_or_default(),
                date: obj(d, "date").and_then(release_date),
                kind: s(d, "type")
                    .map(str::to_lowercase)
                    .unwrap_or_else(|| "album".into()),
            })
        })
        .collect();
    let playlists = items("playlists", "PlaylistResponseWrapper", "Playlist")
        .into_iter()
        .filter_map(|d| {
            Some(Playlist {
                id: id_of(s(d, "uri")?),
                name: s(d, "name")?.to_string(),
                description: s(d, "description").map(str::to_string),
                owner: path(d, &["ownerV2", "data", "name"])
                    .and_then(Value::as_str)
                    .map(str::to_string),
                images: obj(d, "images")
                    .map(|im| {
                        arr(im, "items")
                            .iter()
                            .flat_map(|g| images(arr(g, "sources")))
                            .collect()
                    })
                    .unwrap_or_default(),
                total: 0,
            })
        })
        .collect();
    SearchResults {
        tracks: search_tracks(v),
        artists: search_artists(v),
        albums,
        playlists,
    }
}

/// The playlist itself from a fetchPlaylist answer: name, description, owner, cover, track count.
pub fn playlist_info(v: &Value, playlist_id: &str) -> Option<Playlist> {
    let p = path(v, &["data", "playlistV2"])?;
    Some(Playlist {
        id: playlist_id.to_string(),
        name: s(p, "name")?.to_string(),
        description: s(p, "description").map(str::to_string),
        owner: path(p, &["ownerV2", "data", "name"])
            .and_then(Value::as_str)
            .map(str::to_string),
        images: obj(p, "images")
            .map(|im| {
                arr(im, "items")
                    .iter()
                    .flat_map(|g| images(arr(g, "sources")))
                    .collect()
            })
            .unwrap_or_default(),
        total: path(p, &["content", "totalCount"])
            .and_then(Value::as_i64)
            .unwrap_or(0) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_liked_page() {
        let v = json!({"data":{"me":{"library":{"tracks":{"totalCount":2403,"items":[
            {"track":{"_uri":"spotify:track:abc","data":{"name":"goodbye","duration":{"totalMilliseconds":1019000},
              "artists":{"items":[{"uri":"spotify:artist:a1","profile":{"name":"Sewerslvt"}}]},
              "albumOfTrack":{"uri":"spotify:album:al","name":"we had good times","coverArt":{"sources":[{"url":"https://i/640","width":640},{"url":"https://i/64","width":64}]}}}}}
        ]}}}}});
        let p = liked(&v, 0).unwrap();
        assert_eq!(p.total, 2403);
        let t = &p.items[0];
        assert_eq!(
            (t.id.as_str(), t.title.as_str(), t.duration_ms),
            ("abc", "goodbye", 1_019_000)
        );
        assert_eq!(t.artist_names(), "Sewerslvt");
        assert_eq!(
            crate::model::best_image(&t.album.as_ref().unwrap().images, 60)
                .unwrap()
                .url,
            "https://i/64"
        );
    }
}
