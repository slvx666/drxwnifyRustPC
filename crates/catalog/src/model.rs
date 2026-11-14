//! Catalog models shared by Spotify, Yandex and VK (PORTING-2.3.0 §1.4: same models for all catalogs).
//! Ids follow the Android routing rules (§2): `ym_<raw>` Yandex, `src:VK:<owner>_<audio>` VK tracks,
//! `vk_<owner>_<id>` VK playlists/albums, `vkar_<id>` VK artists, `vk_liked`, anything else = Spotify.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Spotify,
    Yandex,
    Vk,
}

impl Source {
    /// Which catalog an id belongs to (PORTING-2.3.0 §2).
    pub fn of_id(id: &str) -> Source {
        if id.starts_with("ym_") {
            Source::Yandex
        } else if id.starts_with("src:VK:") || id.starts_with("vk_") || id.starts_with("vkar_") {
            Source::Vk
        } else {
            Source::Spotify
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub url: String,
    pub width: Option<u32>,
}

/// Picks the image closest to `want` pixels wide (covers come in 64/300/640).
pub fn best_image(images: &[Image], want: u32) -> Option<&Image> {
    images
        .iter()
        .min_by_key(|i| (i.width.unwrap_or(300) as i64 - want as i64).abs())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumRef {
    pub id: String,
    pub name: String,
    pub images: Vec<Image>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    pub album: Option<AlbumRef>,
    pub duration_ms: u64,
    pub isrc: Option<String>,
    pub explicit: bool,
}

impl Track {
    pub fn artist_names(&self) -> String {
        self.artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub images: Vec<Image>,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artists: Vec<ArtistRef>,
    pub images: Vec<Image>,
    pub release_date: Option<String>,
    pub album_type: Option<String>,
    pub tracks: Vec<Track>,
    pub total_tracks: u32,
}

/// An artist page: name, picture and popular tracks (Catalog.artist + artistTopTracks on Android).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub images: Vec<Image>,
    pub top_tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: Option<String>,
    pub images: Vec<Image>,
}

/// One page of a longer list.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u32,
    pub offset: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_ids_like_android() {
        assert_eq!(Source::of_id("ym_123"), Source::Yandex);
        assert_eq!(Source::of_id("src:VK:1_2_k"), Source::Vk);
        assert_eq!(Source::of_id("vk_1_2"), Source::Vk);
        assert_eq!(Source::of_id("vkar_5"), Source::Vk);
        assert_eq!(Source::of_id("vk_liked"), Source::Vk);
        assert_eq!(Source::of_id("4uLU6hMCjMI75M1A2tKUQC"), Source::Spotify);
    }
}
