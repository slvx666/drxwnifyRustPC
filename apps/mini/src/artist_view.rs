//! An artist page in Mini: popular tracks (from the page's list), albums, singles and EPs, «Участие», similar
//! artists. Cards are filled in place when their covers arrive, so the strips keep their scroll position.

use crate::lazy_songs::LazySongs;
use crate::{HomeCard, MainWindow, SongItem};
use drx_app::{App, ListHeader};
use drx_catalog::model::Image;
use slint::{Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const CARD: u32 = 144;
/// Popular tracks shown before «Показать все»; all of them at most this many.
const SHORT: usize = 5;
const LONG: usize = 100;

#[derive(Default)]
struct Sections {
    /// Per strip (albums, singles, appears, related): images of each card and whether it is round.
    images: Vec<(Vec<Vec<Image>>, bool)>,
    models: Vec<Rc<VecModel<HomeCard>>>,
}

thread_local! {
    static SECTIONS: RefCell<Sections> = RefCell::default();
    static EXPANDED: Cell<bool> = const { Cell::new(false) };
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
    let empty = || ModelRc::new(VecModel::<HomeCard>::default());
    ui.set_artist_albums(empty());
    ui.set_artist_singles(empty());
    ui.set_artist_appears(empty());
    ui.set_artist_related(empty());
    ui.set_artist_tracks(ModelRc::new(VecModel::<SongItem>::default()));
    ui.set_artist_total(0);
    ui.set_artist_expanded(false);
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

/// Covers arrived: fill the cards still waiting and redraw the track rows that had none.
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
}
