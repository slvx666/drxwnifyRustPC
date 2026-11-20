//! A friend's portrait tab (Android PortraitUi.PortraitSection): week / month / year or one month of the last
//! year, the numbers, who they listen to most, their favourite tracks and the genre bar; the genre tags under the
//! friend's name. A friend on an older version sends the old weekly numbers instead (FriendsScreens, «За 7 дней»).

use crate::{
    GenreItem, GenreRow, MainWindow, PortraitArtistItem, PortraitData, SongItem, SourceChip,
    StatTile,
};
use drx_app::App;
use drx_social::friends::FriendData;
use drx_social::portrait::{Portrait, hues_for};
use slint::{Color, ComponentHandle, ModelRc, VecModel};
use std::cell::Cell;

thread_local! {
    /// The period picked: 0 week, 1 month, 2 year, 100 + i the i-th month chip; None = the default.
    static PERIOD: Cell<Option<i32>> = const { Cell::new(None) };
}

const MONTHS: [&str; 12] = [
    "Янв", "Фев", "Мар", "Апр", "Май", "Июн", "Июл", "Авг", "Сен", "Окт", "Ноя", "Дек",
];

/// Another friend's page: the default period again.
pub fn reset() {
    PERIOD.with(|p| p.set(None));
}

pub fn wire(ui: &MainWindow, app: &App) {
    let (app2, ui_w) = (app.clone(), ui.as_weak());
    ui.on_friend_portrait_period(move |i| {
        PERIOD.with(|p| p.set(Some(i)));
        if let Some(ui) = ui_w.upgrade() {
            crate::friends_view::refresh(&ui, &app2);
        }
    });
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

/// HSV (h in degrees, s and v 0..1) as a colour, as Compose's Color.hsv.
fn hsv(h: f32, s: f32, v: f32) -> Color {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let u = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::from_rgb_u8(u(r), u(g), u(b))
}

/// «3,5 ч» from an hour on, «45 мин» below (PortraitUi.durationLabel).
fn duration_label(minutes: i64) -> String {
    if minutes >= 60 {
        let v = format!("{:.1}", minutes as f64 / 60.0).replace('.', ",");
        format!("{} ч", v.strip_suffix(",0").unwrap_or(&v))
    } else {
        format!("{minutes} мин")
    }
}

/// «5 раз», «3 раза» (portrait_plays).
fn plays_label(n: i64) -> String {
    let (d10, d100) = (n % 10, n % 100);
    let word = if (2..=4).contains(&d10) && !(12..=14).contains(&d100) {
        "раза"
    } else {
        "раз"
    };
    format!("{n} {word}")
}

/// «2026-10» → «Окт», with the year when it is not this one.
fn month_label(key: &str, this_year: &str) -> String {
    let (y, m) = key.split_once('-').unwrap_or((key, ""));
    let name = m
        .parse::<usize>()
        .ok()
        .and_then(|m| MONTHS.get(m.wrapping_sub(1)))
        .copied()
        .unwrap_or(m);
    if y == this_year {
        name.to_string()
    } else {
        format!("{name} {y}")
    }
}

/// The genre tags under a friend's name (GenreTag: tinted background, text and dot of the genre's hue).
pub fn tags(ui: &MainWindow, d: Option<&FriendData>) {
    let dark = !ui.get_day_theme();
    let tags = d
        .and_then(|d| d.portrait.as_ref())
        .map(|p| p.tags.clone())
        .unwrap_or_default();
    let items = hues_for(&tags)
        .into_iter()
        .take(3)
        .map(|(name, h)| GenreItem {
            name: name.into(),
            pct: 0,
            color: if dark {
                hsv(h, 0.25, 0.95)
            } else {
                hsv(h, 0.75, 0.38)
            },
            bg: if dark {
                hsv(h, 0.45, 0.32)
            } else {
                hsv(h, 0.18, 0.97)
            },
            dot: hsv(h, 0.6, if dark { 0.9 } else { 0.65 }),
        })
        .collect();
    ui.set_friend_tags(model(items));
}

/// Fills the tab; returns the tracks of «Любимые треки» (a click plays them as a list).
pub fn render(
    ui: &MainWindow,
    app: &App,
    d: Option<&FriendData>,
) -> Vec<drx_catalog::model::Track> {
    let dark = !ui.get_day_theme();
    let mut data = PortraitData {
        footer: "".into(),
        ..Default::default()
    };
    let Some(portrait) = d.and_then(|d| d.portrait.as_ref()) else {
        // An older version: the weekly numbers; nothing at all: say so.
        match d.and_then(|d| d.stats.as_ref()) {
            Some(s) => {
                data.note = "За 7 дней (у друга версия без портрета)".into();
                data.tiles = model(vec![
                    StatTile {
                        value: duration_label(s.week_minutes).into(),
                        label: "Слушал".into(),
                    },
                    StatTile {
                        value: s.week_plays.to_string().into(),
                        label: "Прослушиваний".into(),
                    },
                    StatTile {
                        value: s.liked.to_string().into(),
                        label: "Лайков".into(),
                    },
                ]);
                data.artists_title = "Топ исполнителей".into();
                data.artists = model(
                    s.top
                        .iter()
                        .map(|(n, c)| PortraitArtistItem {
                            name: n.into(),
                            info: plays_label(*c).into(),
                            frac: -1.0,
                            cover: Default::default(),
                            has_cover: false,
                        })
                        .collect(),
                );
            }
            None => {
                data.note =
                    "Портрета пока нет: друг его не прислал или скрыл статистику в приватности."
                        .into()
            }
        }
        ui.set_friend_portrait(data);
        return Vec::new();
    };

    let mut months: Vec<(&String, &Portrait)> = portrait.months.iter().collect();
    months.reverse(); // newest first
    let default = if portrait.month.is_some() {
        1
    } else if portrait.week.is_some() {
        0
    } else {
        2
    };
    let mut period = PERIOD.with(|p| p.get()).unwrap_or(default);
    if period >= 100 && (period - 100) as usize >= months.len() {
        period = default;
    }
    let shown: Option<&Portrait> = match period {
        0 => portrait.week.as_ref(),
        1 => portrait.month.as_ref(),
        2 => portrait.year.as_ref(),
        i => months.get((i - 100) as usize).map(|(_, p)| *p),
    };
    data.periods = model(
        ["Неделя", "Месяц", "Год"]
            .iter()
            .enumerate()
            .map(|(i, l)| SourceChip {
                label: (*l).into(),
                selected: period == i as i32,
            })
            .collect(),
    );
    let this_year = months
        .first()
        .map(|(k, _)| k.split('-').next().unwrap_or("").to_string())
        .unwrap_or_default();
    data.months = model(
        months
            .iter()
            .enumerate()
            .map(|(i, (k, _))| SourceChip {
                label: month_label(k, &this_year).into(),
                selected: period == 100 + i as i32,
            })
            .collect(),
    );
    data.footer = "По прослушиваниям в Drxwnify и лайкам. Фоновое радио весит меньше, пропущенные треки не считаются. \
                   Обновляется раз в день."
        .into();
    let Some(p) = shown else {
        data.note = "За этот период пока мало прослушиваний".into();
        data.footer = "".into();
        ui.set_friend_portrait(data);
        return Vec::new();
    };
    if p.by_likes {
        data.note = "В приложении слушал мало — портрет по лайкам".into();
    }
    let mut tiles = Vec::new();
    if !p.by_likes {
        tiles.push(StatTile {
            value: duration_label(p.minutes).into(),
            label: "Слушал".into(),
        });
        tiles.push(StatTile {
            value: p.tracks.to_string().into(),
            label: "Треков".into(),
        });
    }
    tiles.push(StatTile {
        value: p.new_likes.to_string().into(),
        label: "Новых лайков".into(),
    });
    data.tiles = model(tiles);

    data.artists_title = if p.by_likes {
        "Лайкал больше всего"
    } else {
        "Слушает больше всего"
    }
    .into();
    let max_minutes = p
        .artists
        .iter()
        .map(|a| a.minutes)
        .max()
        .unwrap_or(1)
        .max(1);
    data.artists = model(
        p.artists
            .iter()
            .map(|a| {
                let cover = a
                    .cover
                    .as_deref()
                    .and_then(|u| crate::cover_r(app, u, 88, 44.0));
                let mut parts = Vec::new();
                if a.minutes > 0 && !p.by_likes {
                    parts.push(duration_label(a.minutes));
                }
                if a.likes > 0 {
                    parts.push(format!("♥ {}", a.likes));
                }
                PortraitArtistItem {
                    name: a.name.clone().into(),
                    info: parts.join(" · ").into(),
                    frac: if p.by_likes {
                        -1.0
                    } else {
                        a.minutes as f32 / max_minutes as f32
                    },
                    has_cover: cover.is_some(),
                    cover: cover.unwrap_or_default(),
                }
            })
            .collect(),
    );

    data.tracks_title = if p.by_likes {
        "Новые лайки"
    } else {
        "Любимые треки"
    }
    .into();
    let tracks: Vec<drx_catalog::model::Track> = p
        .top_tracks
        .iter()
        .map(|t| drx_app::social::track_of(&t.track))
        .collect();
    app.remember_tracks(&tracks);
    data.tracks = model(
        tracks
            .iter()
            .zip(&p.top_tracks)
            .enumerate()
            .map(|(i, (tr, t))| {
                let cover = t.track.cover.as_deref().and_then(|u| {
                    crate::cover_r(
                        app,
                        u,
                        crate::ROW_COVER,
                        crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 44.0),
                    )
                });
                let artists = if t.plays > 0 {
                    format!("{} · {}", t.track.artist, plays_label(t.plays))
                } else {
                    t.track.artist.clone()
                };
                SongItem {
                    id: tr.id.clone().into(),
                    title: tr.title.clone().into(),
                    artists: artists.into(),
                    duration: "".into(),
                    liked: t.liked,
                    index: i as i32 + 1,
                    has_cover: cover.is_some(),
                    cover: cover.unwrap_or_default(),
                    artist_id: "".into(),
                    album_id: "".into(),
                    plays: "".into(),
                }
            })
            .collect(),
    );

    let names: Vec<String> = p.genres.iter().map(|(g, _)| g.clone()).collect();
    let hues = hues_for(&names);
    let other = if dark {
        Color::from_rgb_u8(0x5a, 0x5a, 0x62)
    } else {
        Color::from_rgb_u8(0xc8, 0xc8, 0xcd)
    };
    let genres: Vec<GenreItem> = p
        .genres
        .iter()
        .filter(|(_, pct)| *pct > 0)
        .map(|(g, pct)| {
            let color = hues
                .iter()
                .find(|(n, _)| n == g)
                .map_or(other, |(_, h)| hsv(*h, 0.6, if dark { 0.85 } else { 0.72 }));
            GenreItem {
                name: if g.is_empty() {
                    "Другое".into()
                } else {
                    g.clone().into()
                },
                pct: *pct as i32,
                color,
                bg: color,
                dot: color,
            }
        })
        .collect();
    data.legend = model(
        genres
            .chunks(6)
            .map(|c| GenreRow {
                items: model(c.to_vec()),
            })
            .collect(),
    );
    data.genres = model(genres);
    ui.set_friend_portrait(data);
    tracks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(duration_label(45), "45 мин");
        assert_eq!(duration_label(60), "1 ч");
        assert_eq!(duration_label(210), "3,5 ч");
        assert_eq!(plays_label(1), "1 раз");
        assert_eq!(plays_label(3), "3 раза");
        assert_eq!(plays_label(12), "12 раз");
        assert_eq!(plays_label(22), "22 раза");
        assert_eq!(month_label("2026-10", "2026"), "Окт");
        assert_eq!(month_label("2025-12", "2026"), "Дек 2025");
        let red = hsv(0.0, 1.0, 1.0);
        assert_eq!((red.red(), red.green(), red.blue()), (255, 0, 0));
    }
}
