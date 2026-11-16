//! The home page in Mini: the core's home feed as rows of cards. The saved feed shows at once; a stale one is
//! rebuilt behind the page and replaces it when ready.

use crate::{HomeCard, HomeSection, MainWindow};
use drx_app::{App, HomeFeed, HomeRowKind};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

/// Home cards are decoded at their drawn size (Mini memory budget: ~120 covers × 81 KB).
pub const CARD_COVER: u32 = 144;

thread_local! {
    static FEED: RefCell<Option<HomeFeed>> = const { RefCell::new(None) };
}

fn card_cover(app: &App, images: &[drx_catalog::model::Image]) -> Option<slint::Image> {
    let url = drx_catalog::model::best_image(images, 300)?.url.clone();
    crate::cover_r(
        app,
        &url,
        CARD_COVER,
        crate::theme_r(100.0, CARD_COVER, 144.0),
    )
}

/// The cards of every row: built when the feed changes; covers that arrive later update their card in place,
/// so a row the listener scrolled sideways keeps its position.
type Rows = (Rc<VecModel<HomeSection>>, Vec<Rc<VecModel<HomeCard>>>);

thread_local! {
    static MODEL: RefCell<Option<Rows>> = const { RefCell::new(None) };
}

fn cards_of(app: &App, row: &drx_app::HomeRow) -> Vec<HomeCard> {
    match row.kind {
        HomeRowKind::Tracks => row
            .tracks
            .iter()
            .map(|t| {
                let cover = t.album.as_ref().and_then(|a| card_cover(app, &a.images));
                HomeCard {
                    id: t.id.clone().into(),
                    title: t.title.clone().into(),
                    subtitle: t.artist_names().into(),
                    has_cover: cover.is_some(),
                    cover: cover.unwrap_or_default(),
                    album: false,
                }
            })
            .collect(),
        HomeRowKind::Albums => row
            .albums
            .iter()
            .map(|a| {
                let cover = card_cover(app, &a.images);
                let year = a.date.as_deref().and_then(|d| d.get(..4));
                HomeCard {
                    id: a.id.clone().into(),
                    title: a.name.clone().into(),
                    subtitle: match year {
                        Some(y) => format!("{} · {y}", a.artist),
                        None => a.artist.clone(),
                    }
                    .into(),
                    has_cover: cover.is_some(),
                    cover: cover.unwrap_or_default(),
                    album: true,
                }
            })
            .collect(),
    }
}

/// Builds the rows anew (the feed changed).
fn rebuild(ui: &MainWindow, app: &App) {
    let rows: Vec<Rc<VecModel<HomeCard>>> = FEED.with(|f| {
        f.borrow()
            .as_ref()
            .map(|feed| {
                feed.rows
                    .iter()
                    .map(|r| Rc::new(VecModel::from(cards_of(app, r))))
                    .collect()
            })
            .unwrap_or_default()
    });
    let sections: Vec<HomeSection> = FEED.with(|f| {
        f.borrow()
            .as_ref()
            .map(|feed| {
                feed.rows
                    .iter()
                    .zip(&rows)
                    .map(|(r, cards)| HomeSection {
                        title: r.title.clone().into(),
                        artist_id: r.artist_id.clone().unwrap_or_default().into(),
                        label: r.label.clone().unwrap_or_default().into(),
                        cards: ModelRc::from(cards.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    let model = Rc::new(VecModel::from(sections));
    ui.set_home_sections(ModelRc::from(model.clone()));
    MODEL.with(|m| *m.borrow_mut() = Some((model, rows)));
}

/// Covers arrived: fills in the cards still waiting for theirs.
pub fn refresh(_ui: &MainWindow, app: &App) {
    MODEL.with(|m| {
        let m = m.borrow();
        let Some((_, rows)) = m.as_ref() else { return };
        FEED.with(|f| {
            let f = f.borrow();
            let Some(feed) = f.as_ref() else { return };
            for (row, cards) in feed.rows.iter().zip(rows) {
                let fresh = cards_of(app, row);
                for (i, card) in fresh.into_iter().enumerate() {
                    if card.has_cover && cards.row_data(i).is_some_and(|c| !c.has_cover) {
                        cards.set_row_data(i, card);
                    }
                }
            }
        });
    });
}

fn set_feed(ui: &MainWindow, app: &App, feed: HomeFeed) {
    FEED.with(|f| *f.borrow_mut() = Some(feed));
    rebuild(ui, app);
}

/// Shows the saved feed and rebuilds it in the background when it is old (or always with `force`).
pub fn show(ui: &MainWindow, app: &App, force: bool) {
    let have = FEED.with(|f| f.borrow().is_some());
    if !have && let Some(saved) = app.home_cached() {
        set_feed(ui, app, saved);
    }
    let empty = FEED.with(|f| f.borrow().as_ref().is_none_or(|x| x.rows.is_empty()));
    ui.set_view_subtitle(
        if empty {
            "Собираю подборки…"
        } else {
            "Подборки по вашим лайкам и прослушиваниям"
        }
        .into(),
    );
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let res = app2.home(force).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            match res {
                Ok(feed) => {
                    let same = FEED.with(|f| f.borrow().as_ref() == Some(&feed));
                    if !same {
                        set_feed(&ui, &app2, feed);
                    }
                    if ui.get_view_home() {
                        ui.set_view_subtitle("Подборки по вашим лайкам и прослушиваниям".into());
                    }
                }
                Err(e) => {
                    if ui.get_view_home() {
                        ui.set_view_subtitle(format!("Не удалось собрать подборки: {e}").into());
                    }
                }
            }
        });
    });
}

/// A track card was clicked: its row plays from that card.
/// «Случайное» (Android HomeViewModel.getRandomItem): a random track of the home rows plays, the rest of its row
/// after it. False when the feed is empty.
pub fn play_random(app: &App) -> bool {
    let rows: Vec<Vec<drx_catalog::model::Track>> =
        FEED.with(|f| f.borrow().as_ref().map(|feed| feed.rows.iter().map(|r| r.tracks.clone()).collect()).unwrap_or_default());
    let all: Vec<(usize, usize)> =
        rows.iter().enumerate().flat_map(|(s, r)| (0..r.len()).map(move |c| (s, c))).collect();
    if all.is_empty() {
        return false;
    }
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(7);
    let (s, c) = all[seed as usize % all.len()];
    app.play_tracks(rows[s].clone(), c);
    true
}

pub fn play(app: &App, section: usize, card: usize) {
    let tracks = FEED.with(|f| {
        f.borrow()
            .as_ref()
            .and_then(|feed| feed.rows.get(section))
            .map(|r| r.tracks.clone())
    });
    if let Some(t) = tracks
        && card < t.len()
    {
        app.play_tracks(t, card);
    }
}
