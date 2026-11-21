//! YouTube Music pages in Mini (the phone's Explore without charts — the owner does not want them: moods and
//! genres, a mood or genre, new releases, a
//! YouTube artist or channel with «Показать все»). The page loads in the background behind the skeleton; mood
//! buttons come four to a row, the rest as card strips or track rows. Albums, playlists and podcasts open as track
//! lists (`ListSpec::YtList`), songs and videos play from their row.

use crate::nav::Page;
use crate::{HomeCard, MainWindow, SongItem, YtChip, YtRow, YtUi};
use drx_app::App;
use drx_app::ListSpec;
use drx_app::yt::{YtItem, YtKind, YtPage};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const CARD: u32 = 160 * 2;
const CHIPS_PER_ROW: usize = 4;
/// Sections with more cards than this become a grid.
const GRID_FROM: usize = 16;

/// Cards that fit across the centre (176 px each) beside the library and the right panel.
fn grid_columns(ui: &MainWindow) -> usize {
    let w = ui
        .window()
        .size()
        .to_logical(ui.window().scale_factor())
        .width;
    let g = ui.global::<crate::Grid>();
    let lib = g.get_lib_width().max(300.0);
    let side = g.get_side_width().max(0.0);
    (((w - lib - side - 48.0) / 176.0).floor() as usize).clamp(2, 10)
}

struct Shown {
    page: YtPage,
    /// For every UI row: (section, first item of the section in that row).
    rows: Vec<(usize, usize)>,
    /// Card models by UI row (covers fill in later).
    cards: Vec<Option<Rc<VecModel<HomeCard>>>>,
    tracks: Vec<Option<Rc<VecModel<SongItem>>>>,
}

thread_local! {
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
}

/// The pages behind the home chips.
pub fn root_page(which: &str) -> Page {
    match which {
        "moods" => Page::yt(drx_app::yt::MOODS, None, "Настроения и жанры"),
        _ => Page::yt(drx_app::yt::NEW_RELEASES, None, "Новые релизы"),
    }
}

/// The page an item opens (None: it plays instead).
pub fn page_of(item: &YtItem) -> Option<Page> {
    let thumb = item
        .thumbnail
        .as_deref()
        .map(|u| drx_app::yt::bigger_thumbnail(u, 544));
    match item.kind {
        YtKind::Song | YtKind::Video => None,
        YtKind::Album | YtKind::Playlist | YtKind::Podcast => Some(Page::list(
            ListSpec::YtList {
                id: item.id.clone(),
            },
            &item.title,
            "",
            thumb,
        )),
        YtKind::Artist | YtKind::Link => Some(if drx_app::yt::is_list_id(&item.id) {
            Page::list(
                ListSpec::YtList {
                    id: item.id.clone(),
                },
                &item.title,
                "",
                thumb,
            )
        } else {
            Page::yt(&item.id, item.params.as_deref(), &item.title)
        }),
    }
}

fn card_of(app: &App, it: &YtItem) -> HomeCard {
    let radius = if it.kind == YtKind::Artist {
        CARD as f32 / 2.0
    } else {
        crate::theme_r(100.0, CARD, 160.0)
    };
    let cover = it
        .thumbnail
        .as_deref()
        .and_then(|u| crate::cover_r(app, &drx_app::yt::bigger_thumbnail(u, 320), CARD, radius));
    let subtitle = match it.kind {
        YtKind::Album if !it.subtitle.is_empty() => it.subtitle.clone(),
        _ if !it.subtitle.is_empty() => it.subtitle.clone(),
        _ => it
            .artists
            .iter()
            .map(|a| a.0.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    };
    HomeCard {
        id: it.id.clone().into(),
        title: it.title.clone().into(),
        subtitle: subtitle.into(),
        has_cover: cover.is_some(),
        cover: cover.unwrap_or_default(),
        album: true,
    }
}

fn song_of(app: &App, it: &YtItem, page: &YtPage, i: usize) -> SongItem {
    let t = drx_app::yt::track_of(it, Some(page));
    let url = t
        .album
        .as_ref()
        .and_then(|a| a.images.first())
        .map(|im| im.url.clone());
    let cover = url.and_then(|u| {
        crate::cover_r(
            app,
            &u,
            crate::ROW_COVER,
            crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 42.0),
        )
    });
    SongItem {
        id: t.id.clone().into(),
        title: t.title.clone().into(),
        artists: t.artist_names().into(),
        duration: if t.duration_ms > 0 {
            crate::fmt_time(t.duration_ms as f64 / 1000.0).into()
        } else {
            "".into()
        },
        liked: false,
        downloaded: app.is_downloaded(&t.id),
        index: i as i32 + 1,
        has_cover: cover.is_some(),
        cover: cover.unwrap_or_default(),
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
        plays: "".into(),
    }
}

/// Opens a YouTube page: the skeleton at once, the rows when they come.
pub fn show(ui: &MainWindow, app: &App, id: &str, params: Option<&str>) {
    let g = ui.global::<YtUi>();
    g.set_rows(ModelRc::default());
    g.set_state("".into());
    SHOWN.with(|s| *s.borrow_mut() = None);
    ui.set_view_subtitle("YouTube Music".into());
    crate::nav::set_loading(ui, true);
    let generation = crate::nav::generation();
    let (app2, ui_w, id, params) = (
        app.clone(),
        ui.as_weak(),
        id.to_string(),
        params.map(str::to_string),
    );
    app.runtime().spawn(async move {
        let res = app2.yt_page(&id, params.as_deref()).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            if crate::nav::generation() != generation {
                return;
            }
            crate::nav::set_loading(&ui, false);
            match res {
                Ok(page) => fill(&ui, &app2, page),
                Err(e) => ui
                    .global::<YtUi>()
                    .set_state(format!("Не удалось загрузить: {e}").into()),
            }
        });
    });
}

fn fill(ui: &MainWindow, app: &App, page: YtPage) {
    if !page.title.is_empty() {
        ui.set_view_title(page.title.clone().into());
    }
    let sub = [page.subtitle.as_str(), "YouTube Music"]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    ui.set_view_subtitle(sub.into());
    let mut rows: Vec<YtRow> = Vec::new();
    let mut map = Vec::new();
    let mut cards = Vec::new();
    let mut tracks = Vec::new();
    for (si, s) in page.sections.iter().enumerate() {
        let links = s
            .items
            .iter()
            .all(|i| i.kind == YtKind::Link && i.thumbnail.is_none());
        let songs = s.items.iter().all(|i| matches!(i.kind, YtKind::Song))
            || (s
                .items
                .iter()
                .all(|i| matches!(i.kind, YtKind::Song | YtKind::Video))
                && s.items
                    .iter()
                    .all(|i| i.thumbnail.is_none() || i.duration_s.is_some()));
        if links {
            for (ci, chunk) in s.items.chunks(CHIPS_PER_ROW).enumerate() {
                let mut chips: Vec<YtChip> = chunk
                    .iter()
                    .map(|i| YtChip {
                        title: i.title.clone().into(),
                        color: i
                            .color
                            .map(|c| slint::Color::from_argb_encoded(c | 0xFF00_0000))
                            .unwrap_or(slint::Color::from_argb_u8(255, 128, 128, 128)),
                    })
                    .collect();
                // Equal widths in the last row: empty places after the buttons.
                while chips.len() < CHIPS_PER_ROW {
                    chips.push(YtChip {
                        title: "".into(),
                        color: slint::Color::default(),
                    });
                }
                rows.push(YtRow {
                    kind: "chips".into(),
                    title: if ci == 0 {
                        s.title.clone().into()
                    } else {
                        "".into()
                    },
                    more: false,
                    chips: ModelRc::new(VecModel::from(chips)),
                    cards: ModelRc::default(),
                    tracks: ModelRc::default(),
                });
                map.push((si, ci * CHIPS_PER_ROW));
                cards.push(None);
                tracks.push(None);
            }
        } else if songs {
            let model = Rc::new(VecModel::from(
                s.items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| song_of(app, it, &page, i))
                    .collect::<Vec<_>>(),
            ));
            rows.push(YtRow {
                kind: "tracks".into(),
                title: s.title.clone().into(),
                more: s.more.is_some(),
                chips: ModelRc::default(),
                cards: ModelRc::default(),
                tracks: ModelRc::from(model.clone()),
            });
            map.push((si, 0));
            cards.push(None);
            tracks.push(Some(model));
        } else {
            let round = s.items.iter().all(|i| i.kind == YtKind::Artist);
            // A big section (new releases: ~100 albums) is a grid of rows, not one endless strip.
            let per_row = if s.items.len() > GRID_FROM {
                grid_columns(ui)
            } else {
                s.items.len().max(1)
            };
            for (ci, chunk) in s.items.chunks(per_row).enumerate() {
                let model = Rc::new(VecModel::from(
                    chunk.iter().map(|it| card_of(app, it)).collect::<Vec<_>>(),
                ));
                rows.push(YtRow {
                    kind: if round { "round" } else { "cards" }.into(),
                    title: if ci == 0 {
                        s.title.clone().into()
                    } else {
                        "".into()
                    },
                    more: ci == 0 && s.more.is_some(),
                    chips: ModelRc::default(),
                    cards: ModelRc::from(model.clone()),
                    tracks: ModelRc::default(),
                });
                map.push((si, ci * per_row));
                cards.push(Some(model));
                tracks.push(None);
            }
        }
    }
    if rows.is_empty() {
        ui.global::<YtUi>().set_state("Здесь пока пусто".into());
    }
    ui.global::<YtUi>()
        .set_rows(ModelRc::new(VecModel::from(rows)));
    SHOWN.with(|s| {
        *s.borrow_mut() = Some(Shown {
            page,
            rows: map,
            cards,
            tracks,
        })
    });
}

/// Covers arrived: cards and rows still without theirs get them.
pub fn refresh_covers(ui: &MainWindow, app: &App) {
    if !ui.get_view_yt() {
        return;
    }
    SHOWN.with(|s| {
        let s = s.borrow();
        let Some(x) = s.as_ref() else { return };
        for (ri, (si, first)) in x.rows.iter().enumerate() {
            let Some(sec) = x.page.sections.get(*si) else {
                continue;
            };
            if let Some(Some(m)) = x.cards.get(ri) {
                for (i, it) in sec
                    .items
                    .iter()
                    .skip(*first)
                    .take(m.row_count())
                    .enumerate()
                {
                    if m.row_data(i).is_some_and(|c| !c.has_cover) {
                        let c = card_of(app, it);
                        if c.has_cover {
                            m.set_row_data(i, c);
                        }
                    }
                }
            }
            if let Some(Some(m)) = x.tracks.get(ri) {
                for (i, it) in sec.items.iter().enumerate() {
                    if m.row_data(i).is_some_and(|c| !c.has_cover) {
                        let c = song_of(app, it, &x.page, i);
                        if c.has_cover {
                            m.set_row_data(i, c);
                        }
                    }
                }
            }
        }
    });
}

/// The page was left: its rows go.
pub fn release(ui: &MainWindow) {
    if SHOWN.with(|s| s.borrow_mut().take()).is_some() {
        ui.global::<YtUi>().set_rows(ModelRc::default());
    }
}

fn item_at(ri: usize, ci: usize) -> Option<(YtItem, usize)> {
    SHOWN.with(|s| {
        let s = s.borrow();
        let x = s.as_ref()?;
        let (si, first) = *x.rows.get(ri)?;
        let it = x.page.sections.get(si)?.items.get(first + ci)?.clone();
        Some((it, si))
    })
}

pub fn wire(ui: &MainWindow, app: &App) {
    let g = ui.global::<YtUi>();
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        g.on_root_page(move |which| {
            if let Some(ui) = ui_w.upgrade() {
                crate::nav::go_page(&ui, &app2, root_page(&which));
            }
        });
    }
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        g.on_open(move |ri, ci| {
            let Some(ui) = ui_w.upgrade() else { return };
            let Some((it, si)) = item_at(ri.max(0) as usize, ci.max(0) as usize) else {
                return;
            };
            match page_of(&it) {
                Some(p) => crate::nav::go_page(&ui, &app2, p),
                None => SHOWN.with(|s| {
                    let s = s.borrow();
                    if let Some(x) = s.as_ref()
                        && let Some(sec) = x.page.sections.get(si)
                    {
                        let at = sec.items.iter().position(|i| i.id == it.id).unwrap_or(0);
                        app2.play_yt(&sec.items, Some(&x.page), at);
                    }
                }),
            }
        });
    }
    {
        let app2 = app.clone();
        g.on_play(move |ri, ti| {
            SHOWN.with(|s| {
                let s = s.borrow();
                let Some(x) = s.as_ref() else { return };
                let Some((si, _)) = x.rows.get(ri.max(0) as usize) else {
                    return;
                };
                if let Some(sec) = x.page.sections.get(*si) {
                    app2.play_yt(&sec.items, Some(&x.page), ti.max(0) as usize);
                }
            });
        });
    }
    {
        let (app2, ui_w) = (app.clone(), ui.as_weak());
        g.on_more(move |ri| {
            let Some(ui) = ui_w.upgrade() else { return };
            let target = SHOWN.with(|s| {
                let s = s.borrow();
                let x = s.as_ref()?;
                let (si, _) = *x.rows.get(ri.max(0) as usize)?;
                let sec = x.page.sections.get(si)?;
                let (id, params) = sec.more.clone()?;
                Some((id, params, sec.title.clone()))
            });
            if let Some((id, params, title)) = target {
                let page = if drx_app::yt::is_list_id(&id) {
                    Page::list(ListSpec::YtList { id }, &title, "", None)
                } else {
                    Page::yt(&id, params.as_deref(), &title)
                };
                crate::nav::go_page(&ui, &app2, page);
            }
        });
    }
}
