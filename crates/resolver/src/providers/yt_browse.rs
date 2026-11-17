//! YouTube Music pages without an account (InnerTube `browse`, WEB_REMIX), as the phone shows them
//! (innertube YouTube.kt: explore, moodAndGenres, browse, newReleaseAlbums, artist, artistItems,
//! playlist, album). One reader for all of them: a page is a heading and sections; a section is a title, its items
//! (songs, videos, albums, playlists, artists, mood buttons) and where «more» leads.

use serde_json::Value;

/// Well-known pages.
pub const MOODS: &str = "FEmusic_moods_and_genres";
pub const NEW_RELEASES: &str = "FEmusic_new_releases_albums";
pub const EXPLORE: &str = "FEmusic_explore";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtKind {
    Song,
    Video,
    Album,
    Playlist,
    Artist,
    /// A podcast or an episode list (opens like a playlist).
    Podcast,
    /// A mood/genre button or another link to a browse page (`id` + `params`).
    Link,
}

#[derive(Debug, Clone, PartialEq)]
pub struct YtItem {
    pub kind: YtKind,
    /// videoId for songs and videos, browseId otherwise (albums MPREb…, playlists VL…, artists UC…).
    pub id: String,
    pub params: Option<String>,
    pub title: String,
    pub subtitle: String,
    /// (name, browseId) of the artists, when the page links them.
    pub artists: Vec<(String, Option<String>)>,
    pub album: Option<(String, String)>,
    pub thumbnail: Option<String>,
    pub duration_s: Option<i64>,
    /// Mood buttons: their stripe colour (ARGB).
    pub color: Option<u32>,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct YtSection {
    pub title: String,
    pub items: Vec<YtItem>,
    /// Where the section's «more» leads (browseId, params).
    pub more: Option<(String, Option<String>)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct YtPage {
    pub title: String,
    pub subtitle: String,
    pub description: String,
    pub thumbnail: Option<String>,
    /// The album's or playlist's artists from the heading (album rows carry none of their own).
    pub artists: Vec<(String, Option<String>)>,
    pub sections: Vec<YtSection>,
    /// Next part of a long list (playlists, grids).
    pub continuation: Option<String>,
}

impl YtPage {
    /// Every song and video of the page in order (a playlist's or album's tracks).
    pub fn tracks(&self) -> Vec<&YtItem> {
        self.sections
            .iter()
            .flat_map(|s| s.items.iter())
            .filter(|i| matches!(i.kind, YtKind::Song | YtKind::Video))
            .collect()
    }
}

fn text(v: Option<&Value>) -> String {
    v.and_then(|t| t.get("runs"))
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(|x| x.get("text").and_then(Value::as_str)).collect::<String>())
        .or_else(|| v.and_then(|t| t.get("simpleText")).and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

fn runs(v: Option<&Value>) -> &[Value] {
    v.and_then(|t| t.get("runs")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn thumb(v: &Value) -> Option<String> {
    let list = v
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .or_else(|| v.pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails"))
        .or_else(|| v.pointer("/thumbnail/croppedSquareThumbnailRenderer/thumbnail/thumbnails"))
        .or_else(|| v.pointer("/foregroundThumbnail/musicThumbnailRenderer/thumbnail/thumbnails"))
        .or_else(|| v.pointer("/thumbnails"))
        .and_then(Value::as_array)?;
    list.last().and_then(|t| t.get("url")).and_then(Value::as_str).map(str::to_string)
}

fn page_type(endpoint: Option<&Value>) -> &str {
    endpoint
        .and_then(|e| {
            e.pointer("/browseEndpoint/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType")
        })
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn browse_of(endpoint: Option<&Value>) -> Option<(String, Option<String>)> {
    let b = endpoint?.get("browseEndpoint")?;
    let id = b.get("browseId")?.as_str()?.to_string();
    let params = b.get("params").and_then(Value::as_str).map(str::to_string);
    Some((id, params))
}

fn explicit(v: &Value) -> bool {
    v.get("subtitleBadges")
        .or_else(|| v.get("badges"))
        .and_then(Value::as_array)
        .is_some_and(|b| {
            b.iter().any(|x| {
                x.pointer("/musicInlineBadgeRenderer/icon/iconType").and_then(Value::as_str)
                    == Some("MUSIC_EXPLICIT_BADGE")
            })
        })
}

fn linked_artists(rs: &[Value]) -> Vec<(String, Option<String>)> {
    rs
        .iter()
        .filter(|r| {
            matches!(
                page_type(r.get("navigationEndpoint")),
                "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL"
            )
        })
        .filter_map(|r| {
            let name = r.get("text")?.as_str()?.to_string();
            Some((name, browse_of(r.get("navigationEndpoint")).map(|b| b.0)))
        })
        .collect()
}

/// Counts and kinds YouTube puts where artists would be («6,9 тыс. прослушиваний», «Сингл», «2025»).
fn not_a_name(t: &str) -> bool {
    let l = t.trim().to_lowercase();
    ["прослушив", "просмотр", " views", " plays"].iter().any(|w| l.contains(w))
        || ["сингл", "single", "альбом", "album", "ep", "песня", "song", "видео", "video"].contains(&l.as_str())
        || l.chars().all(|c| c.is_ascii_digit())
}

fn artists_of(rs: &[Value]) -> Vec<(String, Option<String>)> {
    let linked = linked_artists(rs);
    if !linked.is_empty() {
        return linked;
    }
    // Without links the first group (before " • ") names them.
    let texts: Vec<&str> = rs.iter().filter_map(|r| r.get("text").and_then(Value::as_str)).collect();
    texts
        .split(|t| t.trim() == "•")
        .next()
        .unwrap_or(&[])
        .iter()
        .filter(|t| !matches!(t.trim(), "," | "&" | "" | "и") && !not_a_name(t))
        .map(|t| (t.trim().to_string(), None))
        .collect()
}

/// "3:21" / "1:02:03" → seconds.
fn clock(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit())) {
        return None;
    }
    Some(parts.iter().fold(0i64, |a, p| a * 60 + p.parse::<i64>().unwrap_or(0)))
}

/// A card (album, playlist, artist, video, mood) — `musicTwoRowItemRenderer`.
fn two_row(r: &Value) -> Option<YtItem> {
    let title = text(r.get("title"));
    let sub_runs = runs(r.get("subtitle"));
    let subtitle = text(r.get("subtitle"));
    let nav = r.get("navigationEndpoint");
    let thumbnail = thumb(r);
    if let Some(video) = nav.and_then(|n| n.pointer("/watchEndpoint/videoId")).and_then(Value::as_str) {
        return Some(YtItem {
            kind: YtKind::Video,
            id: video.into(),
            params: None,
            title,
            artists: linked_artists(sub_runs),
            subtitle,
            album: None,
            thumbnail,
            duration_s: None,
            color: None,
            explicit: explicit(r),
        });
    }
    let (id, params) = browse_of(nav)?;
    let kind = match page_type(nav) {
        "MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_AUDIOBOOK" => YtKind::Album,
        "MUSIC_PAGE_TYPE_PLAYLIST" => YtKind::Playlist,
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => YtKind::Artist,
        "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE" => YtKind::Podcast,
        _ if id.starts_with("MPRE") => YtKind::Album,
        _ if id.starts_with("VL") => YtKind::Playlist,
        _ if id.starts_with("UC") => YtKind::Artist,
        _ => YtKind::Link,
    };
    Some(YtItem {
        kind,
        id,
        params,
        title,
        artists: if kind == YtKind::Album { linked_artists(sub_runs) } else { Vec::new() },
        subtitle,
        album: None,
        thumbnail,
        duration_s: None,
        color: None,
        explicit: explicit(r),
    })
}

/// A row (song, video, or sometimes an album/artist in a list) — `musicResponsiveListItemRenderer`.
fn list_item(r: &Value) -> Option<YtItem> {
    let cols = r.get("flexColumns").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let col = |i: usize| cols.get(i).and_then(|c| c.pointer("/musicResponsiveListItemFlexColumnRenderer/text"));
    let title = text(col(0));
    let mut meta: Vec<Value> = Vec::new();
    for i in 1..cols.len() {
        if !meta.is_empty() {
            meta.push(serde_json::json!({ "text": " • " }));
        }
        meta.extend(runs(col(i)).iter().cloned());
    }
    let video = r
        .pointer("/playlistItemData/videoId")
        .or_else(|| {
            r.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId")
        })
        .or_else(|| col(0).and_then(|c| c.pointer("/runs/0/navigationEndpoint/watchEndpoint/videoId")))
        .and_then(Value::as_str);
    let fixed = r
        .pointer("/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text")
        .map(|t| text(Some(t)))
        .unwrap_or_default();
    let texts: Vec<&str> = meta.iter().filter_map(|x| x.get("text").and_then(Value::as_str)).collect();
    let duration_s = clock(&fixed).or_else(|| texts.iter().rev().find_map(|t| clock(t)));
    let subtitle = texts
        .concat()
        .split(" • ")
        .filter(|p| clock(p).is_none() && !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" • ");
    match video {
        Some(v) => {
            let album = meta.iter().find_map(|x| {
                (page_type(x.get("navigationEndpoint")) == "MUSIC_PAGE_TYPE_ALBUM")
                    .then(|| Some((x.get("text")?.as_str()?.to_string(), browse_of(x.get("navigationEndpoint"))?.0)))
                    .flatten()
            });
            let video_type = r
                .pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/watchEndpointMusicSupportedConfigs/watchEndpointMusicConfig/musicVideoType")
                .and_then(Value::as_str)
                .unwrap_or("");
            let kind = if album.is_none() && matches!(video_type, "MUSIC_VIDEO_TYPE_UGC" | "MUSIC_VIDEO_TYPE_OMV") {
                YtKind::Video
            } else {
                YtKind::Song
            };
            Some(YtItem {
                kind,
                id: v.into(),
                params: None,
                title,
                subtitle,
                artists: artists_of(&meta),
                album,
                thumbnail: thumb(r),
                duration_s,
                color: None,
                explicit: explicit(r),
            })
        }
        None => {
            let nav = r.get("navigationEndpoint");
            let (id, params) = browse_of(nav)?;
            let kind = match page_type(nav) {
                "MUSIC_PAGE_TYPE_ALBUM" => YtKind::Album,
                "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => YtKind::Artist,
                "MUSIC_PAGE_TYPE_PLAYLIST" => YtKind::Playlist,
                "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE" => YtKind::Podcast,
                _ => YtKind::Link,
            };
            Some(YtItem {
                kind,
                id,
                params,
                title,
                subtitle,
                artists: Vec::new(),
                album: None,
                thumbnail: thumb(r),
                duration_s: None,
                color: None,
                explicit: explicit(r),
            })
        }
    }
}

/// A mood/genre button — `musicNavigationButtonRenderer`.
fn nav_button(r: &Value) -> Option<YtItem> {
    let (id, params) = browse_of(r.get("clickCommand"))?;
    Some(YtItem {
        kind: YtKind::Link,
        id,
        params,
        title: text(r.get("buttonText")),
        subtitle: String::new(),
        artists: Vec::new(),
        album: None,
        thumbnail: None,
        duration_s: None,
        color: r.pointer("/solid/leftStripeColor").and_then(Value::as_u64).map(|c| c as u32),
        explicit: false,
    })
}

fn item(v: &Value) -> Option<YtItem> {
    if let Some(r) = v.get("musicTwoRowItemRenderer") {
        two_row(r)
    } else if let Some(r) = v.get("musicResponsiveListItemRenderer") {
        list_item(r)
    } else if let Some(r) = v.get("musicNavigationButtonRenderer") {
        nav_button(r)
    } else if let Some(r) = v.get("musicMultiRowListItemRenderer") {
        // Podcast episodes.
        let video = r.pointer("/onTap/watchEndpoint/videoId").and_then(Value::as_str)?;
        Some(YtItem {
            kind: YtKind::Video,
            id: video.into(),
            params: None,
            title: text(r.get("title")),
            subtitle: text(r.get("subtitle")),
            artists: Vec::new(),
            album: None,
            thumbnail: thumb(r),
            duration_s: None,
            color: None,
            explicit: false,
        })
    } else {
        None
    }
}

fn continuation_of(v: &Value) -> Option<String> {
    v.pointer("/continuations/0/nextContinuationData/continuation")
        .or_else(|| {
            v.get("contents")
                .and_then(Value::as_array)
                .and_then(|c| c.last())
                .and_then(|l| l.pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token"))
        })
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn section(c: &Value) -> Option<(YtSection, Option<String>)> {
    if let Some(g) = c.get("gridRenderer") {
        let title = text(g.pointer("/header/gridHeaderRenderer/title"));
        let items = g.get("items").and_then(Value::as_array)?.iter().filter_map(item).collect();
        return Some((YtSection { title, items, more: None }, continuation_of(g)));
    }
    if let Some(s) = c.get("musicCarouselShelfRenderer") {
        let h = s.pointer("/header/musicCarouselShelfBasicHeaderRenderer");
        let title = text(h.and_then(|h| h.get("title")));
        let more = h
            .and_then(|h| h.pointer("/moreContentButton/buttonRenderer/navigationEndpoint"))
            .and_then(|e| browse_of(Some(e)))
            .or_else(|| h.and_then(|h| h.pointer("/title/runs/0/navigationEndpoint")).and_then(|e| browse_of(Some(e))));
        let items = s.get("contents").and_then(Value::as_array)?.iter().filter_map(item).collect();
        return Some((YtSection { title, items, more }, None));
    }
    for key in ["musicShelfRenderer", "musicPlaylistShelfRenderer"] {
        if let Some(s) = c.get(key) {
            let title = text(s.get("title"));
            let more = s
                .pointer("/bottomEndpoint")
                .and_then(|e| browse_of(Some(e)))
                .or_else(|| s.pointer("/title/runs/0/navigationEndpoint").and_then(|e| browse_of(Some(e))));
            let items = s.get("contents").and_then(Value::as_array)?.iter().filter_map(item).collect();
            return Some((YtSection { title, items, more }, continuation_of(s)));
        }
    }
    None
}

/// Reads any browse answer.
pub fn parse_page(v: &Value) -> YtPage {
    let mut page = YtPage::default();
    // Headings: artists (immersive / visual), playlists and albums (responsive), plain pages.
    if let Some(h) = v.pointer("/header/musicImmersiveHeaderRenderer").or_else(|| v.pointer("/header/musicVisualHeaderRenderer")) {
        page.title = text(h.get("title"));
        page.subtitle = text(h.get("subscriptionButton").and_then(|b| b.pointer("/subscribeButtonRenderer/subscriberCountText")));
        page.description = text(h.get("description"));
        page.thumbnail = thumb(h);
    } else if let Some(h) = v.pointer("/header/musicHeaderRenderer") {
        page.title = text(h.get("title"));
    }
    let single = v.pointer("/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer");
    let two = v.pointer("/contents/twoColumnBrowseResultsRenderer");
    let mut lists: Vec<&Value> = Vec::new();
    if let Some(s) = single {
        lists.push(s);
    }
    if let Some(t) = two {
        if let Some(first) = t.pointer("/tabs/0/tabRenderer/content/sectionListRenderer/contents/0") {
            let h = first
                .get("musicResponsiveHeaderRenderer")
                .or_else(|| first.pointer("/musicEditablePlaylistDetailHeaderRenderer/header/musicResponsiveHeaderRenderer"));
            if let Some(h) = h {
                page.title = text(h.get("title"));
                page.artists = linked_artists(runs(h.get("straplineTextOne")));
                let strap = text(h.get("straplineTextOne"));
                let sub = text(h.get("subtitle"));
                let second = text(h.get("secondSubtitle"));
                page.subtitle = [strap, sub, second].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                page.description = text(h.pointer("/description/musicDescriptionShelfRenderer/description"));
                page.thumbnail = thumb(h);
            }
        }
        if let Some(s) = t.pointer("/secondaryContents/sectionListRenderer") {
            lists.push(s);
        }
    }
    for list in lists {
        for c in list.get("contents").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(d) = c.get("musicDescriptionShelfRenderer") {
                if page.description.is_empty() {
                    page.description = text(d.get("description"));
                }
                continue;
            }
            if let Some((s, cont)) = section(c)
                && !s.items.is_empty()
            {
                if cont.is_some() {
                    page.continuation = cont;
                }
                page.sections.push(s);
            }
        }
        if page.continuation.is_none() {
            page.continuation = continuation_of(list);
        }
    }
    page
}

/// Reads the next part of a long list (`continuation` answers): its items and the next token.
pub fn parse_continuation(v: &Value) -> (Vec<YtItem>, Option<String>) {
    if let Some(c) = v.get("continuationContents") {
        for key in ["musicPlaylistShelfContinuation", "musicShelfContinuation", "gridContinuation"] {
            if let Some(s) = c.get(key) {
                let items = s
                    .get("contents")
                    .or_else(|| s.get("items"))
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(item).collect())
                    .unwrap_or_default();
                return (items, continuation_of(s));
            }
        }
    }
    let actions = v
        .pointer("/onResponseReceivedActions/0/appendContinuationItemsAction/continuationItems")
        .and_then(Value::as_array);
    if let Some(a) = actions {
        let items = a.iter().filter_map(item).collect();
        let next = a
            .last()
            .and_then(|l| l.pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token"))
            .and_then(Value::as_str)
            .map(str::to_string);
        return (items, next);
    }
    (Vec::new(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_a_grid_of_moods_and_a_carousel() {
        let v = json!({ "contents": { "singleColumnBrowseResultsRenderer": { "tabs": [{ "tabRenderer": { "content": { "sectionListRenderer": { "contents": [
            { "gridRenderer": { "header": { "gridHeaderRenderer": { "title": { "runs": [{ "text": "Настроение" }] } } }, "items": [
                { "musicNavigationButtonRenderer": { "buttonText": { "runs": [{ "text": "Для сна" }] }, "solid": { "leftStripeColor": 4278190335u64 },
                  "clickCommand": { "browseEndpoint": { "browseId": "FEmusic_moods_and_genres_category", "params": "ggMPOg1uX1Rh" } } } }
            ] } },
            { "musicCarouselShelfRenderer": { "header": { "musicCarouselShelfBasicHeaderRenderer": { "title": { "runs": [{ "text": "Плейлисты" }] } } }, "contents": [
                { "musicTwoRowItemRenderer": { "title": { "runs": [{ "text": "Chill" }] }, "subtitle": { "runs": [{ "text": "YouTube Music" }] },
                  "navigationEndpoint": { "browseEndpoint": { "browseId": "VLRDCLAK5uy", "browseEndpointContextSupportedConfigs": { "browseEndpointContextMusicConfig": { "pageType": "MUSIC_PAGE_TYPE_PLAYLIST" } } } },
                  "thumbnailRenderer": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [{ "url": "a" }, { "url": "b" }] } } } } }
            ] } }
        ] } } } }] } } });
        let p = parse_page(&v);
        assert_eq!(p.sections.len(), 2);
        assert_eq!(p.sections[0].title, "Настроение");
        let mood = &p.sections[0].items[0];
        assert_eq!((mood.kind, mood.title.as_str(), mood.params.as_deref()), (YtKind::Link, "Для сна", Some("ggMPOg1uX1Rh")));
        assert_eq!(mood.color, Some(4278190335));
        let pl = &p.sections[1].items[0];
        assert_eq!((pl.kind, pl.id.as_str(), pl.thumbnail.as_deref()), (YtKind::Playlist, "VLRDCLAK5uy", Some("b")));
    }

    #[test]
    fn reads_songs_of_a_shelf() {
        let v = json!({ "contents": { "singleColumnBrowseResultsRenderer": { "tabs": [{ "tabRenderer": { "content": { "sectionListRenderer": { "contents": [
            { "musicShelfRenderer": { "title": { "runs": [{ "text": "Песни" }] }, "contents": [
                { "musicResponsiveListItemRenderer": {
                    "playlistItemData": { "videoId": "abc" },
                    "flexColumns": [
                        { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": "Song" }] } } },
                        { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                            { "text": "Artist", "navigationEndpoint": { "browseEndpoint": { "browseId": "UC1", "browseEndpointContextSupportedConfigs": { "browseEndpointContextMusicConfig": { "pageType": "MUSIC_PAGE_TYPE_ARTIST" } } } } },
                            { "text": " • " },
                            { "text": "Album", "navigationEndpoint": { "browseEndpoint": { "browseId": "MPREb1", "browseEndpointContextSupportedConfigs": { "browseEndpointContextMusicConfig": { "pageType": "MUSIC_PAGE_TYPE_ALBUM" } } } } }
                        ] } } }
                    ],
                    "fixedColumns": [{ "musicResponsiveListItemFixedColumnRenderer": { "text": { "runs": [{ "text": "3:05" }] } } }]
                } }
            ] } }
        ] } } } }] } } });
        let p = parse_page(&v);
        let s = &p.tracks()[0];
        assert_eq!((s.kind, s.id.as_str(), s.title.as_str()), (YtKind::Song, "abc", "Song"));
        assert_eq!(s.artists, vec![("Artist".to_string(), Some("UC1".to_string()))]);
        assert_eq!(s.album, Some(("Album".into(), "MPREb1".into())));
        assert_eq!(s.duration_s, Some(185));
    }
}

/// Live check against YouTube Music (no account): `DRX_LIVE_YT=1 cargo test -p drx-resolver yt_browse_live -- --nocapture`.
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn yt_browse_live() {
        if std::env::var_os("DRX_LIVE_YT").is_none() {
            return;
        }
        let yt = crate::providers::youtube::YouTube::new(drx_core::http::Http::new().unwrap());
        let show = |name: &str, p: &YtPage| {
            println!("== {name}: «{}» «{}» {} sections, cont {}", p.title, p.subtitle, p.sections.len(), p.continuation.is_some());
            for s in &p.sections {
                let first = s.items.first().map(|i| format!("{:?} {} [{}] {:?}", i.kind, i.title, i.id, i.artists.first())).unwrap_or_default();
                println!("   {} — {} items, more {:?}: {first}", s.title, s.items.len(), s.more.as_ref().map(|m| &m.0));
            }
        };
        let moods = yt.browse(MOODS, None).await.unwrap();
        show("moods", &moods);
        let first = moods.sections[0].items[0].clone();
        let mood = yt.browse(&first.id, first.params.as_deref()).await.unwrap();
        show(&first.title, &mood);
        let new = yt.browse(NEW_RELEASES, None).await.unwrap();
        show("new releases", &new);
        let album = new.sections[0].items[0].clone();
        let a = yt.browse(&album.id, None).await.unwrap();
        show(&album.title, &a);
        assert!(!a.tracks().is_empty(), "album tracks");
        let pl = mood.sections.iter().flat_map(|s| s.items.iter()).find(|i| i.kind == YtKind::Playlist).cloned().unwrap();
        let p = yt.browse(&pl.id, None).await.unwrap();
        show(&pl.title, &p);
        assert!(!p.tracks().is_empty(), "playlist tracks");
        let artist_id = a.tracks()[0].artists.iter().find_map(|x| x.1.clone()).unwrap();
        let ar = yt.browse(&artist_id, None).await.unwrap();
        show("artist", &ar);
        if let Some((id, params)) = ar.sections.iter().find_map(|s| s.more.clone()) {
            let more = yt.browse(&id, params.as_deref()).await.unwrap();
            show("artist more", &more);
        }
    }
}
