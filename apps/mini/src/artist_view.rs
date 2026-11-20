//! An artist page in Mini: popular tracks, albums, singles and EPs, «Участие», similar
//! artists, and the full Spotify-style Discography ("Все треки") grouped by year and album.

use crate::lazy_songs::LazySongs;
use crate::{DiscographyAlbum, HomeCard, MainWindow, SongItem};
use drx_app::{App, ListHeader, SongRow};
use drx_catalog::model::Image;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const CARD: u32 = 144;
const ALBUM_COVER: u32 = 112;
/// Popular tracks shown before «Показать все»; all of them at most this many.
const SHORT: usize = 5;
const LONG: usize = 100;

#[derive(Default)]
struct Sections {
    /// Per strip (albums, singles, appears, related): images of each card and whether it is round.
    images: Vec<(Vec<Vec<Image>>, bool)>,
    models: Vec<Rc<VecModel<HomeCard>>>,
}

#[derive(Clone)]
pub struct DiscographyAlbumData {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub cover_url: Option<String>,
    pub _cover_images: Vec<Image>,
    pub saved: bool,
    pub tracks: Vec<SongRow>,
}

#[derive(Default)]
struct DiscographyState {
    artist_id: String,
    artist_title: String,
    releases: Vec<drx_catalog::model::Release>,
    albums: Vec<DiscographyAlbumData>,
    loaded: bool,
}

thread_local! {
    static SECTIONS: RefCell<Sections> = RefCell::default();
    static EXPANDED: Cell<bool> = const { Cell::new(false) };
    static DISCOGRAPHY: RefCell<DiscographyState> = RefCell::default();
}

fn card_cover(app: &App, images: &[Image], round: bool) -> Option<slint::Image> {
    let url = drx_catalog::model::best_image(images, 300)?.url.clone();
    let r = if round {
        CARD as f32 / 2.0
    } else {
        crate::theme_r(100.0, CARD, CARD as f32)
    };
    crate::cover_r(app, &url, CARD, r)
}

fn year_kind(date: &Option<String>, kind: &str) -> String {
    let k = match kind {
        "single" => "Сингл",
        "ep" => "EP",
        "compilation" => "Сборник",
        _ => "Альбом",
    };
    match date.as_deref().and_then(|d| d.get(..4)) {
        Some(y) => format!("{y} · {k}"),
        None => k.to_string(),
    }
}

/// A new page: nothing to show yet.
pub fn clear(ui: &MainWindow) {
    EXPANDED.with(|e| e.set(false));
    SECTIONS.with(|s| *s.borrow_mut() = Sections::default());
    DISCOGRAPHY.with(|d| *d.borrow_mut() = DiscographyState::default());
    let empty = || ModelRc::new(VecModel::<HomeCard>::default());
    ui.set_artist_albums(empty());
    ui.set_artist_singles(empty());
    ui.set_artist_appears(empty());
    ui.set_artist_related(empty());
    ui.set_artist_tracks(ModelRc::new(VecModel::<SongItem>::default()));
    ui.set_artist_total(0);
    ui.set_artist_expanded(false);
    ui.set_artist_discography(ModelRc::default());
    ui.set_artist_discography_mode(false);
    ui.set_artist_discography_loading(false);
}

/// The sections from an artist page heading (catalog data).
pub fn set_header(ui: &MainWindow, app: &App, h: &ListHeader) {
    struct C {
        id: String,
        title: String,
        sub: String,
        images: Vec<Image>,
    }
    let (albums, singles): (Vec<_>, Vec<_>) = h
        .releases
        .iter()
        .map(|r| {
            (
                matches!(r.kind.as_str(), "single" | "ep"),
                C {
                    id: r.id.clone(),
                    title: r.name.clone(),
                    sub: year_kind(&r.date, &r.kind),
                    images: r.images.clone(),
                },
            )
        })
        .partition(|(single, _)| !*single);
    let albums: Vec<C> = albums.into_iter().map(|(_, c)| c).collect();
    let singles: Vec<C> = singles.into_iter().map(|(_, c)| c).collect();
    let appears: Vec<C> = h
        .appears_on
        .iter()
        .map(|a| C {
            id: a.id.clone(),
            title: a.name.clone(),
            sub: {
                let by = a
                    .artists
                    .iter()
                    .map(|x| x.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                match a.date.as_deref().and_then(|d| d.get(..4)) {
                    Some(y) if !by.is_empty() => format!("{y} · {by}"),
                    Some(y) => y.to_string(),
                    None => by,
                }
            },
            images: a.images.clone(),
        })
        .collect();
    let related: Vec<C> = h
        .related
        .iter()
        .map(|a| C {
            id: a.id.clone(),
            title: a.name.clone(),
            sub: "Исполнитель".into(),
            images: a.images.clone(),
        })
        .collect();
    let mut sections = Sections::default();
    for (list, round) in [
        (albums, false),
        (singles, false),
        (appears, false),
        (related, true),
    ] {
        let cards: Vec<HomeCard> = list
            .iter()
            .map(|c| {
                let cover = card_cover(app, &c.images, round);
                HomeCard {
                    id: c.id.clone().into(),
                    title: c.title.clone().into(),
                    subtitle: c.sub.clone().into(),
                    has_cover: cover.is_some(),
                    cover: cover.unwrap_or_default(),
                    album: true,
                }
            })
            .collect();
        sections
            .images
            .push((list.into_iter().map(|c| c.images).collect(), round));
        sections.models.push(Rc::new(VecModel::from(cards)));
    }
    ui.set_artist_albums(ModelRc::from(sections.models[0].clone()));
    ui.set_artist_singles(ModelRc::from(sections.models[1].clone()));
    ui.set_artist_appears(ModelRc::from(sections.models[2].clone()));
    ui.set_artist_related(ModelRc::from(sections.models[3].clone()));
    SECTIONS.with(|s| *s.borrow_mut() = sections);

    let artist_id = crate::nav::current()
        .and_then(|p| match p.spec {
            drx_app::ListSpec::Artist { id } | drx_app::ListSpec::CatalogArtist { id } => Some(id),
            _ => None,
        })
        .unwrap_or_default();
    DISCOGRAPHY.with(|d| {
        *d.borrow_mut() = DiscographyState {
            artist_id,
            artist_title: h.title.clone(),
            releases: h.releases.clone(),
            albums: Vec::new(),
            loaded: false,
        };
    });
}

/// Popular tracks from the page's list (its first rows).
pub fn set_tracks(ui: &MainWindow, songs: &LazySongs) {
    if !ui.get_view_artist() {
        return;
    }
    let total = songs.len();
    let expanded = EXPANDED.with(|e| e.get());
    let n = if expanded {
        total.min(LONG)
    } else {
        total.min(SHORT)
    };
    let items: Vec<SongItem> = (0..n).filter_map(|i| songs.row_data(i)).collect();
    ui.set_artist_tracks(ModelRc::new(VecModel::from(items)));
    ui.set_artist_total(total as i32);
    ui.set_artist_expanded(expanded);
}

pub fn toggle_more(ui: &MainWindow, songs: &LazySongs) {
    EXPANDED.with(|e| e.set(!e.get()));
    set_tracks(ui, songs);
}

/// Helper to render the Discography UI model in Slint
fn render_discography(ui: &MainWindow, app: &App) {
    DISCOGRAPHY.with(|d| {
        let d = d.borrow();
        let mut disc_albums = Vec::new();
        for alb in &d.albums {
            let cover = alb
                .cover_url
                .as_deref()
                .and_then(|u| crate::cover_r(app, u, ALBUM_COVER, 6.0));
            let has_cover = cover.is_some();
            let track_items: Vec<SongItem> = alb
                .tracks
                .iter()
                .enumerate()
                .map(|(ti, t)| SongItem {
                    id: t.id.clone().into(),
                    title: t.title.clone().into(),
                    artists: t.artists.clone().into(),
                    duration: crate::fmt_time(t.duration_s as f64).into(),
                    liked: t.liked,
                    index: (ti + 1) as i32,
                    cover: slint::Image::default(),
                    has_cover: false,
                    artist_id: t.artist_id.clone().unwrap_or_default().into(),
                    album_id: alb.id.clone().into(),
                    plays: t.plays.map(crate::group_digits).unwrap_or_default().into(),
                })
                .collect();
            disc_albums.push(DiscographyAlbum {
                id: alb.id.clone().into(),
                title: alb.title.clone().into(),
                subtitle: alb.subtitle.clone().into(),
                cover: cover.unwrap_or_default(),
                has_cover,
                saved: alb.saved,
                tracks: ModelRc::new(VecModel::from(track_items)),
            });
        }
        ui.set_artist_discography(ModelRc::new(VecModel::from(disc_albums)));
        ui.set_artist_discography_loading(false);
    });
}

/// Loads full discography: queries local database library tracks and fetches catalog albums.
pub fn load_discography(ui: &MainWindow, app: &App) {
    let already_loaded = DISCOGRAPHY.with(|d| d.borrow().loaded);
    if already_loaded {
        render_discography(ui, app);
        return;
    }
    ui.set_artist_discography_loading(true);

    let (artist_id, artist_title, releases) = DISCOGRAPHY.with(|d| {
        let d = d.borrow();
        (
            d.artist_id.clone(),
            d.artist_title.clone(),
            d.releases.clone(),
        )
    });

    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let mut loaded_albums: Vec<DiscographyAlbumData> = Vec::new();

        // 1. Check local library tracks for this artist
        if !artist_title.is_empty() {
            if let Ok(rows) = app2.artist_library_albums(&artist_title).await {
                let mut current_id = String::new();
                for (alb_id, alb_title, yr, thumb, mut song) in rows {
                    if alb_id != current_id || loaded_albums.is_empty() {
                        current_id = alb_id.clone();
                        let yr_str = if yr > 0 {
                            format!("{yr}")
                        } else {
                            String::new()
                        };
                        let sub = if yr_str.is_empty() {
                            "Альбом".into()
                        } else {
                            format!("Альбом • {yr_str}")
                        };
                        let is_saved = app2.is_album_saved(&alb_id).await.unwrap_or(false);
                        loaded_albums.push(DiscographyAlbumData {
                            id: alb_id,
                            title: alb_title,
                            subtitle: sub,
                            cover_url: thumb,
                            _cover_images: Vec::new(),
                            saved: is_saved,
                            tracks: Vec::new(),
                        });
                    }
                    if song.artist_id.is_none() && !artist_id.is_empty() {
                        song.artist_id = Some(artist_id.clone());
                    }
                    if let Some(last) = loaded_albums.last_mut() {
                        last.tracks.push(song);
                    }
                }
                for a in &mut loaded_albums {
                    let count = a.tracks.len();
                    a.subtitle = format!("{} • {} треков", a.subtitle, count);
                }

                if !loaded_albums.is_empty() {
                    let local_copy = loaded_albums.clone();
                    let (ui_w2, app3) = (ui_w.clone(), app2.clone());
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = ui_w2.upgrade() else { return };
                        DISCOGRAPHY.with(|d| {
                            let mut d = d.borrow_mut();
                            d.albums = local_copy;
                        });
                        render_discography(&ui, &app3);
                    });
                }
            }
        }

        // 2. Fetch catalog releases from Spotify/catalog
        if !releases.is_empty() {
            let mut catalog_albums = Vec::new();
            for r in releases.into_iter().take(20) {
                if let Some(alb) = app2.catalog_album(&r.id).await {
                    let yr = alb
                        .release_date
                        .as_deref()
                        .and_then(|d| d.get(..4))
                        .unwrap_or("");
                    let kind_label = match alb.album_type.as_deref().unwrap_or(&r.kind) {
                        "single" => "Сингл",
                        "ep" => "EP",
                        "compilation" => "Сборник",
                        _ => "Альбом",
                    };
                    let is_saved = app2.is_album_saved(&alb.id).await.unwrap_or(false);
                    let a_id_default = if !artist_id.is_empty() {
                        Some(artist_id.clone())
                    } else {
                        None
                    };
                    let tracks: Vec<SongRow> = alb
                        .tracks
                        .into_iter()
                        .map(|t| {
                            let artists = t.artist_names();
                            let a_id = t
                                .artists
                                .into_iter()
                                .next()
                                .map(|a| a.id)
                                .or_else(|| a_id_default.clone());
                            SongRow {
                                id: t.id,
                                title: t.title,
                                artists,
                                album: Some(alb.name.clone()),
                                duration_s: (t.duration_ms / 1000) as i64,
                                thumbnail: drx_catalog::model::best_image(&alb.images, 300)
                                    .map(|i| i.url.clone()),
                                liked: false,
                                artist_id: a_id,
                                album_id: Some(alb.id.clone()),
                                plays: t.plays,
                            }
                        })
                        .collect();
                    let count = tracks.len();
                    let sub = if yr.is_empty() {
                        format!("{kind_label} • {count} треков")
                    } else {
                        format!("{kind_label} • {yr} • {count} треков")
                    };
                    let cover_url =
                        drx_catalog::model::best_image(&alb.images, 300).map(|i| i.url.clone());
                    catalog_albums.push(DiscographyAlbumData {
                        id: alb.id,
                        title: alb.name,
                        subtitle: sub,
                        cover_url,
                        _cover_images: alb.images,
                        saved: is_saved,
                        tracks,
                    });
                }
            }
            if !catalog_albums.is_empty() {
                loaded_albums = catalog_albums;
            }
        }

        let albums_to_save = loaded_albums;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            DISCOGRAPHY.with(|d| {
                let mut d = d.borrow_mut();
                d.albums = albums_to_save;
                d.loaded = true;
            });
            render_discography(&ui, &app2);
        });
    });
}

/// Plays an album and all subsequent albums from the discography view down to the end.
pub fn play_album(app: &App, album_id: &str) {
    DISCOGRAPHY.with(|d| {
        let d = d.borrow();
        if let Some(pos) = d.albums.iter().position(|a| a.id == album_id) {
            let mut all_tracks = Vec::new();
            for a in &d.albums[pos..] {
                all_tracks.extend(a.tracks.iter().cloned());
            }
            if !all_tracks.is_empty() {
                app.play_rows(all_tracks, 0);
            }
        }
    });
}

/// Plays a track from the discography view (continues through current album and all subsequent albums).
pub fn play_track(app: &App, track_id: &str) {
    DISCOGRAPHY.with(|d| {
        let d = d.borrow();
        for (alb_idx, alb) in d.albums.iter().enumerate() {
            if let Some(t_idx) = alb.tracks.iter().position(|t| t.id == track_id) {
                let mut all_tracks = Vec::new();
                all_tracks.extend(alb.tracks[t_idx..].iter().cloned());
                for next_alb in &d.albums[(alb_idx + 1)..] {
                    all_tracks.extend(next_alb.tracks.iter().cloned());
                }
                if !all_tracks.is_empty() {
                    app.play_rows(all_tracks, 0);
                }
                return;
            }
        }
    });
}

/// Toggles saving a discography album to library (+ / checkmark button).
pub fn toggle_album_save(ui: &MainWindow, app: &App, album_id: &str) {
    let album_id = album_id.to_string();
    let disc = ui.get_artist_discography();
    let count = disc.row_count();
    let mut new_state = None;

    DISCOGRAPHY.with(|d| {
        let mut d = d.borrow_mut();
        if let Some(alb) = d.albums.iter_mut().find(|a| a.id == album_id) {
            alb.saved = !alb.saved;
            new_state = Some(alb.saved);
        }
    });

    for i in 0..count {
        if let Some(mut row) = disc.row_data(i) {
            if row.id == album_id {
                let saved = new_state.unwrap_or(!row.saved);
                row.saved = saved;
                disc.set_row_data(i, row);
                break;
            }
        }
    }

    if let Some(saved) = new_state {
        let (app2, ui_w, alb_id) = (app.clone(), ui.as_weak(), album_id.clone());
        app.runtime().spawn(async move {
            let res = app2.set_album_saved(&alb_id, saved).await;
            if res.is_ok() {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_w.upgrade() {
                        let msg = if saved {
                            "Альбом добавлен в медиатеку"
                        } else {
                            "Альбом удалён из медиатеки"
                        };
                        ui.set_toast(msg.into());
                    }
                });
            }
        });
    }
}

/// Shares a discography album (...) button: copies link to clipboard and notifies user.
pub fn share_album(ui: &MainWindow, _app: &App, album_id: &str) {
    let (title, artist) = DISCOGRAPHY.with(|d| {
        let d = d.borrow();
        let title = d
            .albums
            .iter()
            .find(|a| a.id == album_id)
            .map(|a| a.title.clone())
            .unwrap_or_default();
        let artist = d.artist_title.clone();
        (title, artist)
    });
    let text = drx_app::share_text("album", album_id, &title, &artist);
    crate::track_menu::copy_share(ui, &text);
}

/// Covers arrived: fill the cards still waiting, redraw the track rows that had none,
/// and fill any discography album covers that became available.
pub fn refresh_covers(ui: &MainWindow, app: &App, songs: &LazySongs) {
    if !ui.get_view_artist() {
        return;
    }
    SECTIONS.with(|s| {
        let s = s.borrow();
        for ((images, round), model) in s.images.iter().zip(&s.models) {
            for (i, imgs) in images.iter().enumerate() {
                if model.row_data(i).is_some_and(|c| !c.has_cover)
                    && let Some(c) = card_cover(app, imgs, *round)
                    && let Some(mut card) = model.row_data(i)
                {
                    card.cover = c;
                    card.has_cover = true;
                    model.set_row_data(i, card);
                }
            }
        }
    });
    let rows = ui.get_artist_tracks();
    if (0..rows.row_count()).any(|i| rows.row_data(i).is_some_and(|t| !t.has_cover)) {
        set_tracks(ui, songs);
    }

    // Discography covers:
    let disc = ui.get_artist_discography();
    let count = disc.row_count();
    for i in 0..count {
        if let Some(mut alb) = disc.row_data(i) {
            if !alb.has_cover {
                let cover_url = DISCOGRAPHY
                    .with(|d| d.borrow().albums.get(i).and_then(|a| a.cover_url.clone()));
                if let Some(url) = cover_url {
                    if let Some(img) = crate::cover_r(app, &url, ALBUM_COVER, 6.0) {
                        alb.cover = img;
                        alb.has_cover = true;
                        disc.set_row_data(i, alb);
                    }
                }
            }
        }
    }
}
