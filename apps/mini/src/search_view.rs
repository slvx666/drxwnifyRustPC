//! The search page in Mini: suggestions under the field, then results by section (search.rs in the core).
//! Results are kept per query and place, so «back» returns to them without asking again.

use crate::{HomeCard, MainWindow, SongItem, SourceChip, Suggestion};
use drx_app::{App, SearchOutcome, SearchSource};
use drx_catalog::model::Image;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Search cards are decoded at their drawn size.
const CARD: u32 = 144;
const TOP: u32 = 104;

struct Shown {
    query: String,
    source: SearchSource,
    outcome: SearchOutcome,
    expanded: bool,
}

thread_local! {
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    static CACHE: RefCell<HashMap<(String, String), SearchOutcome>> = RefCell::new(HashMap::new());
    static SOURCES: RefCell<Vec<SearchSource>> = const { RefCell::new(Vec::new()) };
    static TYPED: Cell<u64> = const { Cell::new(0) };
    /// Card models (covers arrive later and fill their card in place).
    static MODELS: RefCell<Vec<Rc<VecModel<HomeCard>>>> = const { RefCell::new(Vec::new()) };
}

/// The page was left: its results and cards are let go (a few recent searches stay in the cache for «back»).
pub fn release(ui: &MainWindow) {
    if SHOWN.with(|s| s.borrow_mut().take()).is_some() {
        MODELS.with(|m| m.borrow_mut().clear());
        ui.set_search_tracks(ModelRc::default());
        ui.set_search_artists(ModelRc::default());
        ui.set_search_albums(ModelRc::default());
        ui.set_search_playlists(ModelRc::default());
        ui.set_search_top_cover(slint::Image::default());
    }
}

fn key(q: &str, s: SearchSource) -> (String, String) {
    (q.trim().to_lowercase(), format!("{s:?}"))
}

/// A cover drawn at `size` logical px; `round` = a circle (artists), else the theme's corners (rows: small ones).
fn cover(app: &App, images: &[Image], size: u32, round: bool) -> Option<slint::Image> {
    let url = drx_catalog::model::best_image(images, 300)?.url.clone();
    let (decoded, shown, max) = if size == crate::ROW_COVER {
        (size, 44.0, 8.0)
    } else {
        (size, size as f32, 100.0)
    };
    let r = if round {
        decoded as f32 / 2.0
    } else {
        crate::theme_r(max, decoded, shown)
    };
    crate::cover_r(app, &url, decoded, r)
}

/// While typing: the history (field empty) or matching history + suggestions, a little after the last key.
pub fn typing(ui: &MainWindow, app: &App, text: String) {
    let n = TYPED.with(|t| {
        t.set(t.get() + 1);
        t.get()
    });
    let (ui_w, app2) = (ui.as_weak(), app.clone());
    slint::Timer::single_shot(std::time::Duration::from_millis(220), move || {
        if TYPED.with(|t| t.get()) != n {
            return;
        }
        let (app3, ui_w2) = (app2.clone(), ui_w.clone());
        app2.runtime().spawn(async move {
            let (history, online) = app3.search_suggestions(&text).await;
            let empty = text.trim().is_empty();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_w2.upgrade() else { return };
                if TYPED.with(|t| t.get()) != n {
                    return;
                }
                let items: Vec<Suggestion> = history
                    .into_iter()
                    .map(|t| Suggestion {
                        text: t.into(),
                        history: true,
                    })
                    .chain(online.into_iter().map(|t| Suggestion {
                        text: t.into(),
                        history: false,
                    }))
                    .collect();
                ui.set_suggest_history_only(empty);
                ui.set_suggest_open(!items.is_empty());
                ui.set_suggestions(ModelRc::new(VecModel::from(items)));
            });
        });
    });
}

/// Hides suggestions (a search started).
pub fn close_suggestions(ui: &MainWindow) {
    TYPED.with(|t| t.set(t.get() + 1));
    ui.set_suggest_open(false);
}

fn chips(app: &App, current: SearchSource) -> Vec<SourceChip> {
    let sources = app.search_sources();
    SOURCES.with(|s| *s.borrow_mut() = sources.iter().map(|(s, _)| *s).collect());
    sources
        .into_iter()
        .map(|(s, label)| SourceChip {
            label: label.into(),
            selected: s == current,
        })
        .collect()
}

/// Shows results for `query` in `source`: from the cache, or searches (the page says «Ищу…» meanwhile).
pub fn show(ui: &MainWindow, app: &App, query: &str, source: SearchSource) {
    close_suggestions(ui);
    ui.set_view_title("Поиск".into());
    ui.set_view_subtitle(format!("«{}»", query.trim()).into());
    ui.set_search_chips(ModelRc::new(VecModel::from(chips(app, source))));
    if let Some(o) = CACHE.with(|c| c.borrow().get(&key(query, source)).cloned()) {
        fill(ui, app, query, source, o);
        return;
    }
    ui.set_search_state("loading".into());
    let (app2, ui_w, q) = (app.clone(), ui.as_weak(), query.to_string());
    app.runtime().spawn(async move {
        let res = app2.search_page(&q, source).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            match res {
                Ok(o) => {
                    CACHE.with(|c| {
                        let mut c = c.borrow_mut();
                        if c.len() > 8 {
                            c.clear();
                        }
                        c.insert(key(&q, source), o.clone());
                    });
                    // Still on this search?
                    let current =
                        SHOWN.with(|s| s.borrow().as_ref().map(|x| (x.query.clone(), x.source)));
                    if ui.get_view_search()
                        && current
                            .as_ref()
                            .is_none_or(|(cq, cs)| *cq == q && *cs == source)
                    {
                        fill(&ui, &app2, &q, source, o);
                    }
                }
                Err(e) => ui.set_search_state(format!("Поиск не удался: {e}").into()),
            }
        });
    });
    SHOWN.with(|s| {
        *s.borrow_mut() = Some(Shown {
            query: query.to_string(),
            source,
            outcome: SearchOutcome::default(),
            expanded: false,
        })
    });
}

fn fill(ui: &MainWindow, app: &App, query: &str, source: SearchSource, o: SearchOutcome) {
    let expanded = SHOWN.with(|s| {
        s.borrow()
            .as_ref()
            .filter(|x| x.query == query && x.source == source)
            .is_some_and(|x| x.expanded)
    });
    SHOWN.with(|s| {
        *s.borrow_mut() = Some(Shown {
            query: query.to_string(),
            source,
            outcome: o,
            expanded,
        })
    });
    render(ui, app);
}

/// Draws the page from the shown outcome (also when covers arrive).
pub fn render(ui: &MainWindow, app: &App) {
    SHOWN.with(|s| {
        let s = s.borrow();
        let Some(x) = s.as_ref() else { return };
        let r = &x.outcome.results;
        let nothing = r.tracks.is_empty()
            && r.artists.is_empty()
            && r.albums.is_empty()
            && r.playlists.is_empty();
        ui.set_search_state(if nothing { "empty" } else { "" }.into());
        ui.set_view_subtitle(
            if x.outcome.from_link {
                "По ссылке".to_string()
            } else {
                format!("«{}»", x.query.trim())
            }
            .into(),
        );

        // Best result: an artist whose name is the query, else the first track (from a link: what it names).
        let q = drx_app::fold(&x.query);
        let artist_hit = r.artists.first().filter(|a| {
            drx_app::fold(&a.name) == q || (x.outcome.from_link && r.tracks.is_empty())
        });
        if let Some(a) = artist_hit {
            ui.set_search_top_kind("artist".into());
            ui.set_search_top_title(a.name.clone().into());
            ui.set_search_top_sub("Исполнитель".into());
            set_top_cover(ui, app, &a.images, true);
        } else if let Some(t) = r.tracks.first() {
            ui.set_search_top_kind("track".into());
            ui.set_search_top_title(t.title.clone().into());
            ui.set_search_top_sub(format!("Трек · {}", t.artist_names()).into());
            set_top_cover(
                ui,
                app,
                t.album.as_ref().map(|a| a.images.as_slice()).unwrap_or(&[]),
                false,
            );
        } else {
            ui.set_search_top_kind("".into());
        }

        let shown = if x.expanded {
            r.tracks.len()
        } else {
            r.tracks.len().min(4)
        };
        let tracks: Vec<SongItem> = r
            .tracks
            .iter()
            .take(shown)
            .enumerate()
            .map(|(i, t)| {
                let c = t
                    .album
                    .as_ref()
                    .and_then(|a| cover(app, &a.images, crate::ROW_COVER, false));
                SongItem {
                    id: t.id.clone().into(),
                    title: t.title.clone().into(),
                    artists: t.artist_names().into(),
                    duration: crate::fmt_time(t.duration_ms as f64 / 1000.0).into(),
                    liked: false,
                    index: i as i32 + 1,
                    has_cover: c.is_some(),
                    cover: c.unwrap_or_default(),
                    plays: t.plays.map(crate::group_digits).unwrap_or_default().into(),
                    artist_id: t
                        .artists
                        .first()
                        .map(|a| a.id.clone())
                        .unwrap_or_default()
                        .into(),
                    album_id: t
                        .album
                        .as_ref()
                        .map(|a| a.id.clone())
                        .unwrap_or_default()
                        .into(),
                }
            })
            .collect();
        ui.set_search_tracks(ModelRc::new(VecModel::from(tracks)));
        ui.set_search_tracks_total(r.tracks.len() as i32);
        ui.set_search_expanded(x.expanded);

        let card = |id: &str, title: &str, sub: String, images: &[Image], round: bool| {
            let c = cover(app, images, CARD, round);
            HomeCard {
                id: id.into(),
                title: title.into(),
                subtitle: sub.into(),
                has_cover: c.is_some(),
                cover: c.unwrap_or_default(),
                album: true,
            }
        };
        let artists: Vec<HomeCard> = r
            .artists
            .iter()
            .map(|a| card(&a.id, &a.name, "Исполнитель".into(), &a.images, true))
            .collect();
        let albums: Vec<HomeCard> = r
            .albums
            .iter()
            .map(|a| {
                let by = a
                    .artists
                    .iter()
                    .map(|x| x.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let year = a.date.as_deref().and_then(|d| d.get(..4));
                let sub = match year {
                    Some(y) if !by.is_empty() => format!("{y} · {by}"),
                    Some(y) => y.to_string(),
                    None => by,
                };
                card(&a.id, &a.name, sub, &a.images, false)
            })
            .collect();
        let playlists: Vec<HomeCard> = r
            .playlists
            .iter()
            .map(|p| {
                let sub = match (&p.owner, p.total) {
                    (Some(o), _) => o.clone(),
                    (None, n) if n > 0 => crate::nav::tracks_word(n as usize),
                    _ => "Плейлист".into(),
                };
                card(&p.id, &p.name, sub, &p.images, false)
            })
            .collect();
        let models: Vec<Rc<VecModel<HomeCard>>> = vec![
            Rc::new(VecModel::from(artists)),
            Rc::new(VecModel::from(albums)),
            Rc::new(VecModel::from(playlists)),
        ];
        ui.set_search_artists(ModelRc::from(models[0].clone()));
        ui.set_search_albums(ModelRc::from(models[1].clone()));
        ui.set_search_playlists(ModelRc::from(models[2].clone()));
        MODELS.with(|m| *m.borrow_mut() = models);
    });
}

fn set_top_cover(ui: &MainWindow, app: &App, images: &[Image], round: bool) {
    let url = drx_catalog::model::best_image(images, 300)
        .map(|i| i.url.clone())
        .unwrap_or_default();
    ui.set_search_top_cover_url(url.into());
    match cover(app, images, TOP, round) {
        Some(c) => {
            ui.set_search_top_cover(c);
            ui.set_search_top_has_cover(true);
        }
        None => ui.set_search_top_has_cover(false),
    }
}

/// Covers arrived: fill the cards still waiting (strips keep their scroll position), the top card and rows.
pub fn refresh_covers(ui: &MainWindow, app: &App) {
    if !ui.get_view_search() {
        return;
    }
    SHOWN.with(|s| {
        let s = s.borrow();
        let Some(x) = s.as_ref() else { return };
        let r = &x.outcome.results;
        let rounds = [true, false, false];
        let lists: [Vec<&[Image]>; 3] = [
            r.artists.iter().map(|a| a.images.as_slice()).collect(),
            r.albums.iter().map(|a| a.images.as_slice()).collect(),
            r.playlists.iter().map(|p| p.images.as_slice()).collect(),
        ];
        MODELS.with(|m| {
            for ((model, images), round) in m.borrow().iter().zip(lists.iter()).zip(rounds) {
                for (i, imgs) in images.iter().enumerate() {
                    if model.row_data(i).is_some_and(|c| !c.has_cover)
                        && let Some(c) = cover(app, imgs, CARD, round)
                        && let Some(mut card) = model.row_data(i)
                    {
                        card.cover = c;
                        card.has_cover = true;
                        model.set_row_data(i, card);
                    }
                }
            }
        });
    });
    if !ui.get_search_top_has_cover() {
        let url = ui.get_search_top_cover_url().to_string();
        let round = ui.get_search_top_kind() == "artist";
        let r = if round {
            TOP as f32 / 2.0
        } else {
            crate::theme_r(100.0, TOP, TOP as f32)
        };
        if !url.is_empty()
            && let Some(c) = crate::cover_r(app, &url, TOP, r)
        {
            ui.set_search_top_cover(c);
            ui.set_search_top_has_cover(true);
        }
    }
    // Track rows are few: redraw them.
    let need = SHOWN.with(|s| s.borrow().is_some());
    if need {
        let rows = ui.get_search_tracks();
        if (0..rows.row_count()).any(|i| rows.row_data(i).is_some_and(|t| !t.has_cover)) {
            render(ui, app);
        }
    }
}

/// Which place a chip stands for.
pub fn source_at(i: usize) -> Option<SearchSource> {
    SOURCES.with(|s| s.borrow().get(i).copied())
}

pub fn current() -> Option<(String, SearchSource)> {
    SHOWN.with(|s| s.borrow().as_ref().map(|x| (x.query.clone(), x.source)))
}

pub fn toggle_expanded(ui: &MainWindow, app: &App) {
    SHOWN.with(|s| {
        if let Some(x) = s.borrow_mut().as_mut() {
            x.expanded = !x.expanded;
        }
    });
    render(ui, app);
}

/// Plays the found tracks from `i` (VK ones with their URLs).
pub fn play(app: &App, i: usize) -> Option<String> {
    SHOWN.with(|s| {
        let s = s.borrow();
        let x = s.as_ref()?;
        let id = x.outcome.playable.get(i)?.track.id.clone();
        app.play_found(x.outcome.playable.clone(), i);
        Some(id)
    })
}

/// What the best-result card does: an artist opens, a track plays.
pub enum Top {
    Artist(String),
    Track,
}

pub fn top(ui: &MainWindow) -> Option<Top> {
    match ui.get_search_top_kind().as_str() {
        "artist" => SHOWN.with(|s| {
            s.borrow()
                .as_ref()?
                .outcome
                .results
                .artists
                .first()
                .map(|a| Top::Artist(a.id.clone()))
        }),
        "track" => Some(Top::Track),
        _ => None,
    }
}

pub fn artist_id(i: usize) -> Option<String> {
    SHOWN.with(|s| {
        s.borrow()
            .as_ref()?
            .outcome
            .results
            .artists
            .get(i)
            .map(|a| a.id.clone())
    })
}

pub fn album_id(i: usize) -> Option<String> {
    SHOWN.with(|s| {
        s.borrow()
            .as_ref()?
            .outcome
            .results
            .albums
            .get(i)
            .map(|a| a.id.clone())
    })
}

/// A playlist result: its id, title, cover URL and whether it is the library's.
pub fn playlist(i: usize) -> Option<(String, String, Option<String>, bool)> {
    SHOWN.with(|s| {
        let s = s.borrow();
        let x = s.as_ref()?;
        let p = x.outcome.results.playlists.get(i)?;
        Some((
            p.id.clone(),
            p.name.clone(),
            drx_catalog::model::best_image(&p.images, 300).map(|i| i.url.clone()),
            x.outcome.library,
        ))
    })
}
