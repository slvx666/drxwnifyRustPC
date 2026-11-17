//! YouTube Music pages for both UIs, without an account (the phone's Explore, MoodAndGenres, NewRelease — no charts, owner's call —
//! YouTube artist/channel, playlist, album and podcast screens). The pages themselves come from
//! `drx_resolver::providers::yt_browse`; this file turns their rows into tracks the player knows (a YouTube Music
//! song plays by its video id, as library songs from YouTube Music do).

use crate::{App, AppError, AppResult, ListHandle, ListHeader, Playable};
use drx_catalog::model::{AlbumRef, ArtistRef, Image, Track};
pub use drx_resolver::providers::yt_browse::{MOODS, NEW_RELEASES, YtItem, YtKind, YtPage, YtSection};
use std::sync::Arc;

/// At most this many tracks of one YouTube list are loaded (long playlists come in parts of ~100).
const MAX_TRACKS: usize = 2000;

/// Bigger thumbnails: YouTube Music sends 60/120 px ones with a size in the address (`=w120-h120`).
pub fn bigger_thumbnail(url: &str, px: u32) -> String {
    match url.rfind("=w") {
        Some(i) if url[i..].contains("-h") => format!("{}=w{px}-h{px}-l90-rj", &url[..i]),
        _ => url.to_string(),
    }
}

/// A song or video row as a track (id = video id).
pub fn track_of(item: &YtItem, page: Option<&YtPage>) -> Track {
    let artists = if item.artists.is_empty() { page.map(|p| p.artists.clone()).unwrap_or_default() } else { item.artists.clone() };
    let thumb = item
        .thumbnail
        .clone()
        .or_else(|| page.and_then(|p| p.thumbnail.clone()))
        .map(|u| bigger_thumbnail(&u, 544));
    let album = match (&item.album, page) {
        (Some((name, id)), _) => Some(AlbumRef { id: id.clone(), name: name.clone(), images: thumb.iter().map(|u| Image { url: u.clone(), width: Some(544) }).collect() }),
        (None, _) => thumb.map(|u| AlbumRef { id: String::new(), name: String::new(), images: vec![Image { url: u, width: Some(544) }] }),
    };
    Track {
        id: item.id.clone(),
        title: item.title.clone(),
        artists: artists
            .into_iter()
            .map(|(name, id)| ArtistRef { id: id.unwrap_or_default(), name })
            .collect(),
        album,
        duration_ms: item.duration_s.unwrap_or(0).max(0) as u64 * 1000,
        isrc: None,
        explicit: item.explicit,
        plays: None,
    }
}

/// Whether a browse id opens as a list of tracks (playlist, album, podcast) rather than a page of sections.
pub fn is_list_id(id: &str) -> bool {
    id.starts_with("VL") || id.starts_with("MPRE") || id.starts_with("MPSP") || id.starts_with("PL") || id.starts_with("RD") || id.starts_with("OLAK")
}

/// The id a playlist link needs for `browse` (`PL…`/`RD…`/`OLAK…` get their `VL`).
fn browse_id_of(id: &str) -> String {
    if id.starts_with("PL") || id.starts_with("RD") || id.starts_with("OLAK") { format!("VL{id}") } else { id.to_string() }
}

impl App {
    pub(crate) fn yt(&self) -> Arc<drx_resolver::providers::youtube::YouTube> {
        self.inner.yt.get_or_init(|| Arc::new(drx_resolver::providers::youtube::YouTube::new(self.inner.http.clone()))).clone()
    }

    /// A YouTube Music page of sections (moods, a mood, new releases, an artist or channel, «all albums»).
    pub async fn yt_page(&self, browse_id: &str, params: Option<&str>) -> AppResult<YtPage> {
        let mut page = self.yt().browse(browse_id, params).await.map_err(|e| AppError::Other(e.0))?;
        // Content filters (the phone's hideExplicit / hideVideoSongs).
        let (no_explicit, no_video) = (self.hide_explicit(), self.hide_video());
        if no_explicit || no_video {
            for s in &mut page.sections {
                s.items.retain(|i| !(no_explicit && i.explicit) && !(no_video && i.kind == YtKind::Video));
            }
            page.sections.retain(|s| !s.items.is_empty());
        }
        let tracks: Vec<Track> = page.sections.iter().flat_map(|s| s.items.iter()).filter(|i| matches!(i.kind, YtKind::Song | YtKind::Video)).map(|i| track_of(i, Some(&page))).collect();
        self.remember(tracks.iter());
        Ok(page)
    }

    /// Plays the songs and videos of a page section, starting from `items[from]`.
    pub fn play_yt(&self, items: &[YtItem], page: Option<&YtPage>, from: usize) {
        let start = items[..from.min(items.len())].iter().filter(|i| matches!(i.kind, YtKind::Song | YtKind::Video)).count();
        let tracks: Vec<Track> =
            items.iter().filter(|i| matches!(i.kind, YtKind::Song | YtKind::Video)).map(|i| track_of(i, page)).collect();
        if start < tracks.len() {
            self.play_tracks(tracks, start);
        }
    }

    /// Loads a YouTube list page (`ListSpec::YtList`): the heading, then the tracks part by part.
    pub(crate) async fn load_yt_list(&self, handle: ListHandle, id: &str) -> AppResult<Vec<Playable>> {
        let yt = self.yt();
        let page = yt.browse(&browse_id_of(id), None).await.map_err(|e| AppError::Other(e.0))?;
        let kind = if id.starts_with("MPRE") { "Альбом" } else if id.starts_with("MPSP") { "Подкаст" } else { "Плейлист" };
        self.set_header(
            handle,
            ListHeader {
                title: page.title.clone(),
                subtitle: if page.subtitle.is_empty() { format!("{kind} · YouTube Music") } else { page.subtitle.clone() },
                image: page.thumbnail.as_deref().map(|u| bigger_thumbnail(u, 544)),
                releases: Vec::new(),
                appears_on: Vec::new(),
                related: Vec::new(),
            },
        );
        let mut rows: Vec<Playable> = page.tracks().into_iter().map(|i| Playable { track: track_of(i, Some(&page)), url: None }).collect();
        let mut next = page.continuation.clone();
        let mut version = 1;
        while let Some(token) = next.take() {
            if rows.len() >= MAX_TRACKS || self.inner.lists.spec(handle).is_none() {
                break;
            }
            self.set_rows(handle, rows.clone(), version);
            version += 1;
            let Ok((items, more)) = yt.browse_more(&token).await else { break };
            if items.is_empty() {
                break;
            }
            rows.extend(
                items
                    .iter()
                    .filter(|i| matches!(i.kind, YtKind::Song | YtKind::Video))
                    .map(|i| Playable { track: track_of(i, Some(&page)), url: None }),
            );
            next = more;
        }
        rows.truncate(MAX_TRACKS);
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn thumbnails_grow() {
        assert_eq!(
            super::bigger_thumbnail("https://lh3.googleusercontent.com/abc=w120-h120-l90-rj", 544),
            "https://lh3.googleusercontent.com/abc=w544-h544-l90-rj"
        );
        assert_eq!(super::bigger_thumbnail("https://i.ytimg.com/vi/x/hqdefault.jpg", 544), "https://i.ytimg.com/vi/x/hqdefault.jpg");
    }
}
