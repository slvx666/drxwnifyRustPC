//! The statistics page: period chips, totals (time, listenings, since when), top tracks, artists and albums.
//! Counts this app's listening only (the 2.3.0 note says so on the page).

use crate::nav::{length_words, plays_word};
use crate::{HomeCard, MainWindow, SongItem, SourceChip, StatTile};
use drx_app::{App, StatPeriod, Stats};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const CARD: u32 = 144;
const SHORT: usize = 10;

/// A strip's model, the cover URL of each card, and whether covers are round.
type CardStrip = (Rc<VecModel<HomeCard>>, Vec<Option<String>>, bool);

thread_local! {
    static PERIOD: Cell<usize> = const { Cell::new(1) };
    static EXPANDED: Cell<bool> = const { Cell::new(false) };
    static DATA: RefCell<Option<Stats>> = const { RefCell::new(None) };
    static CARDS: RefCell<Vec<CardStrip>> = const { RefCell::new(Vec::new()) };
}

/// «398 ч 12 мин», «45 мин».
fn time_words(ms: i64) -> String {
    length_words(ms / 1000)
}

fn date_words(ms: i64) -> String {
    const MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    let days = (ms / 1000 + drx_platform::utc_offset_seconds()).div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

fn card_cover(app: &App, url: &Option<String>, round: bool) -> Option<slint::Image> {
    let url = url.as_deref()?;
    let r = if round {
        CARD as f32 / 2.0
    } else {
        crate::theme_r(100.0, CARD, CARD as f32)
    };
    crate::cover_r(app, url, CARD, r)
}

/// Wrapped's line about the year's minutes (WrappedRepository, in Russian; the same for the same minutes).
fn wrapped_line(minutes: i64) -> String {
    let lines: &[&str] = match minutes {
        0..=999 => &[
            "Только разогреваетесь? {} минут.",
            "{} минут — короткое погружение в музыку.",
        ],
        1000..=4999 => &[
            "Похоже, вы нашли Drxwnify недавно — и отдали музыке {} минут.",
            "Жизнь не только в музыке: {} минут — здоровый баланс.",
        ],
        5000..=14999 => &[
            "Музыка — определённо ваше: {} минут саундтрека за год.",
            "Стабильно, надёжно, ритмично: {} минут.",
        ],
        15000..=39999 => &[
            "Вы вообще снимаете наушники? {} минут — музыка вам как воздух.",
            "Энергия главного героя: ваша жизнь была фильмом {} минут.",
        ],
        _ => &[
            "У вас всё хорошо? Вы буквально жили здесь {} минут.",
            "Поведение топ-1 %: {} минут — легенда.",
        ],
    };
    lines[(minutes as usize) % lines.len()].replace("{}", &crate::group_digits(minutes as u64))
}

pub fn wire_wrapped(ui: &MainWindow, app: &App) {
    use slint::ComponentHandle;
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.global::<crate::StatsUi>().on_act(move || {
        let Some(ui) = ui_w.upgrade() else { return };
        match app2.runtime().block_on(app2.save_wrapped_playlist()) {
            Ok((_, n)) => ui.set_toast(
                format!(
                    "Плейлист «{}» готов: {}",
                    drx_app::wrapped_playlist(),
                    crate::nav::tracks_word(n)
                )
                .into(),
            ),
            Err(e) => ui.set_toast(format!("Не получилось: {e}").into()),
        }
    });
}

/// The page was left: its numbers and cards are let go.
pub fn release(ui: &MainWindow) {
    if DATA.with(|d| d.borrow_mut().take()).is_some() {
        CARDS.with(|c| c.borrow_mut().clear());
        ui.set_stats_tracks(ModelRc::default());
        ui.set_stats_artists(ModelRc::default());
        ui.set_stats_albums(ModelRc::default());
        ui.set_stats_tiles(ModelRc::default());
    }
}

pub fn show(ui: &MainWindow, app: &App) {
    let period = StatPeriod::ALL[PERIOD.with(|p| p.get())];
    let chips: Vec<SourceChip> = StatPeriod::ALL
        .iter()
        .map(|p| SourceChip {
            label: p.label().into(),
            selected: *p == period,
        })
        .collect();
    ui.set_stats_periods(ModelRc::new(VecModel::from(chips)));
    ui.set_view_subtitle("Прослушивания в этом приложении".into());
    ui.set_stats_note("".into());
    crate::nav::set_loading(ui, true);
    let generation = crate::nav::generation();
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    app.runtime().spawn(async move {
        let stats = app2.stats(period).await.unwrap_or_default();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            // Another page, or another period chosen meanwhile.
            if crate::nav::generation() == generation
                && StatPeriod::ALL[PERIOD.with(|p| p.get())] == period
            {
                crate::nav::set_loading(&ui, false);
                fill(&ui, &app2, period, stats);
            }
        });
    });
}

fn fill(ui: &MainWindow, app: &App, period: StatPeriod, stats: drx_app::Stats) {
    // Wrapped: the year's line and its playlist (the phone's last pages).
    {
        use slint::ComponentHandle;
        let g = ui.global::<crate::StatsUi>();
        if period == StatPeriod::Wrapped && stats.plays > 0 {
            g.set_headline(wrapped_line(stats.total_ms / 60_000).into());
            g.set_action(format!("Сохранить плейлист «{}»", drx_app::wrapped_playlist()).into());
        } else {
            g.set_headline("".into());
            g.set_action("".into());
        }
    }
    DATA.with(|d| *d.borrow_mut() = Some(stats));
    render(ui, app);
}

fn render(ui: &MainWindow, app: &App) {
    DATA.with(|d| {
        let d = d.borrow();
        let Some(st) = d.as_ref() else { return };
        let mut tiles = vec![
            StatTile {
                value: if st.total_ms > 0 {
                    time_words(st.total_ms)
                } else {
                    "0 мин".into()
                }
                .into(),
                label: "слушали за период".into(),
            },
            StatTile {
                value: crate::group_digits(st.plays as u64).into(),
                label: plays_word(st.plays as usize)
                    .split_once(' ')
                    .map(|(_, w)| w.to_string())
                    .unwrap_or_default()
                    .into(),
            },
        ];
        // The year's summary counts different songs and artists (Wrapped's totals) instead of the first day.
        if StatPeriod::ALL[PERIOD.with(|p| p.get())] == StatPeriod::Wrapped {
            tiles.push(StatTile {
                value: crate::group_digits(st.unique_songs as u64).into(),
                label: "разных треков".into(),
            });
            tiles.push(StatTile {
                value: crate::group_digits(st.unique_artists as u64).into(),
                label: "разных исполнителей".into(),
            });
        } else if let Some(since) = st.since_ms {
            tiles.push(StatTile {
                value: date_words(since).into(),
                label: "первое прослушивание".into(),
            });
        }
        ui.set_stats_tiles(ModelRc::new(VecModel::from(tiles)));
        if st.plays == 0 {
            ui.set_stats_note("За этот период прослушиваний нет".into());
        }
        let expanded = EXPANDED.with(|e| e.get());
        let n = if expanded {
            st.songs.len()
        } else {
            st.songs.len().min(SHORT)
        };
        let tracks: Vec<SongItem> = st
            .songs
            .iter()
            .take(n)
            .enumerate()
            .map(|(i, s)| {
                let r = &s.song;
                let cover = r.thumbnail.as_deref().and_then(|u| {
                    crate::cover_r(
                        app,
                        u,
                        crate::ROW_COVER,
                        crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 44.0),
                    )
                });
                SongItem {
                    id: r.id.clone().into(),
                    title: r.title.clone().into(),
                    artists: r.artists.clone().into(),
                    // Compact: how many times in the duration column, the time in the wider one.
                    duration: format!("×{}", s.plays).into(),
                    liked: r.liked,
                    downloaded: app.is_downloaded(&r.id),
                    index: i as i32 + 1,
                    has_cover: cover.is_some(),
                    cover: cover.unwrap_or_default(),
                    artist_id: r.artist_id.clone().unwrap_or_default().into(),
                    album_id: r.album_id.clone().unwrap_or_default().into(),
                    plays: time_words(s.ms).into(),
                }
            })
            .collect();
        ui.set_stats_tracks(ModelRc::new(VecModel::from(tracks)));
        ui.set_stats_total(st.songs.len() as i32);
        ui.set_stats_expanded(expanded);
        let artists: Vec<(HomeCard, Option<String>)> = st
            .artists
            .iter()
            .map(|a| {
                let c = card_cover(app, &a.thumbnail, true);
                (
                    HomeCard {
                        id: a.id.clone().into(),
                        title: a.title.clone().into(),
                        subtitle: time_words(a.ms).into(),
                        has_cover: c.is_some(),
                        cover: c.unwrap_or_default(),
                        album: true,
                    },
                    a.thumbnail.clone(),
                )
            })
            .collect();
        let albums: Vec<(HomeCard, Option<String>)> = st
            .albums
            .iter()
            .map(|a| {
                let c = card_cover(app, &a.thumbnail, false);
                (
                    HomeCard {
                        id: a.id.clone().into(),
                        title: a.title.clone().into(),
                        subtitle: if a.subtitle.is_empty() {
                            time_words(a.ms)
                        } else {
                            format!("{} · {}", a.subtitle, time_words(a.ms))
                        }
                        .into(),
                        has_cover: c.is_some(),
                        cover: c.unwrap_or_default(),
                        album: true,
                    },
                    a.thumbnail.clone(),
                )
            })
            .collect();
        let mut cards = Vec::new();
        for (list, round) in [(artists, true), (albums, false)] {
            let urls: Vec<Option<String>> = list.iter().map(|(_, u)| u.clone()).collect();
            let model = Rc::new(VecModel::from(
                list.into_iter().map(|(c, _)| c).collect::<Vec<_>>(),
            ));
            cards.push((model, urls, round));
        }
        ui.set_stats_artists(ModelRc::from(cards[0].0.clone()));
        ui.set_stats_albums(ModelRc::from(cards[1].0.clone()));
        CARDS.with(|c| *c.borrow_mut() = cards);
    });
}

pub fn set_period(ui: &MainWindow, app: &App, i: usize) {
    PERIOD.with(|p| p.set(i.min(StatPeriod::ALL.len() - 1)));
    EXPANDED.with(|e| e.set(false));
    show(ui, app);
}

pub fn toggle_more(ui: &MainWindow, app: &App) {
    EXPANDED.with(|e| e.set(!e.get()));
    render(ui, app);
}

pub fn play(app: &App, i: usize) -> Option<String> {
    DATA.with(|d| {
        let d = d.borrow();
        let st = d.as_ref()?;
        let rows: Vec<drx_app::SongRow> = st.songs.iter().map(|s| s.song.clone()).collect();
        let id = rows.get(i)?.id.clone();
        app.play_rows(rows, i);
        Some(id)
    })
}

pub fn refresh_covers(ui: &MainWindow, app: &App) {
    if !ui.get_view_stats() {
        return;
    }
    CARDS.with(|c| {
        let mut cards = c.borrow_mut();
        for (model, urls, round) in cards.iter_mut() {
            for (i, url) in urls.iter_mut().enumerate() {
                if let Some(card) = model.row_data(i)
                    && !card.has_cover
                {
                    if (url.is_none() || url.as_deref() == Some("")) && *round {
                        if let Some(thumb) = app.artist_thumbnail(&card.id) {
                            *url = Some(thumb);
                        }
                    }
                    if let Some(img) = card_cover(app, url, *round)
                        && let Some(mut card) = model.row_data(i)
                    {
                        card.cover = img;
                        card.has_cover = true;
                        model.set_row_data(i, card);
                    }
                }
            }
        }
    });
    let rows = ui.get_stats_tracks();
    if (0..rows.row_count()).any(|i| rows.row_data(i).is_some_and(|t| !t.has_cover)) {
        render(ui, app);
    }
}
