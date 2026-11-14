//! The history page: listenings grouped by day («Сегодня», «Вчера», «На этой неделе»… then months), newest first;
//! a listening can be removed. Rows are a lazy model: covers load only for what is on screen.

use crate::{HistoryItem, MainWindow, SongItem};
use drx_app::{App, HistoryEntry};
use slint::{Model, ModelNotify, ModelRc, ModelTracker};
use std::cell::RefCell;
use std::rc::Rc;

enum Row {
    Header(String),
    Entry(usize),
}

pub struct HistoryModel {
    app: App,
    entries: RefCell<Vec<HistoryEntry>>,
    rows: RefCell<Vec<Row>>,
    offset_s: i64,
    notify: ModelNotify,
}

thread_local! {
    static MODEL: RefCell<Option<Rc<HistoryModel>>> = const { RefCell::new(None) };
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// «14:32» today/yesterday, «Пн 14:32» this or last week, «12 окт.» earlier.
fn when(at_ms: i64, group: &str, offset_s: i64) -> String {
    let local = at_ms / 1000 + offset_s;
    let hm = format!(
        "{:02}:{:02}",
        local.rem_euclid(86_400) / 3600,
        local.rem_euclid(3600) / 60
    );
    match group {
        "Сегодня" | "Вчера" => hm,
        "На этой неделе" | "На прошлой неделе" => {
            const DAYS: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];
            let wd = (local.div_euclid(86_400) + 3).rem_euclid(7) as usize;
            format!("{} {hm}", DAYS[wd])
        }
        _ => {
            const MONTHS: [&str; 12] = [
                "янв.",
                "февр.",
                "мар.",
                "апр.",
                "мая",
                "июн.",
                "июл.",
                "авг.",
                "сент.",
                "окт.",
                "нояб.",
                "дек.",
            ];
            let days = local.div_euclid(86_400);
            // Civil date from days (same algorithm as the core's).
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            format!("{d} {}", MONTHS[(m - 1) as usize])
        }
    }
}

impl HistoryModel {
    fn new(app: App, entries: Vec<HistoryEntry>) -> Self {
        let m = HistoryModel {
            app,
            entries: RefCell::new(entries),
            rows: RefCell::default(),
            offset_s: drx_platform::utc_offset_seconds(),
            notify: ModelNotify::default(),
        };
        m.rebuild();
        m
    }

    fn rebuild(&self) {
        let now = now_ms();
        let mut rows = Vec::new();
        let mut last = String::new();
        for (i, e) in self.entries.borrow().iter().enumerate() {
            let g = drx_app::history_group(e.at_ms, now, self.offset_s);
            if g != last {
                rows.push(Row::Header(g.clone()));
                last = g;
            }
            rows.push(Row::Entry(i));
        }
        *self.rows.borrow_mut() = rows;
        self.notify.reset();
    }

    pub fn remove(&self, event_id: i64) {
        self.entries.borrow_mut().retain(|e| e.event_id != event_id);
        self.rebuild();
    }

    pub fn songs(&self) -> Vec<drx_app::SongRow> {
        self.entries
            .borrow()
            .iter()
            .map(|e| e.song.clone())
            .collect()
    }

    pub fn refresh_covers(&self) {
        self.notify.reset();
    }
}

impl Model for HistoryModel {
    type Data = HistoryItem;

    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<HistoryItem> {
        let rows = self.rows.borrow();
        match rows.get(row)? {
            Row::Header(h) => Some(HistoryItem {
                header: h.clone().into(),
                song: SongItem::default(),
                event: 0,
                index: -1,
            }),
            Row::Entry(i) => {
                let entries = self.entries.borrow();
                let e = entries.get(*i)?;
                // The group this row is under, for its time label.
                let group = rows[..row]
                    .iter()
                    .rev()
                    .find_map(|r| match r {
                        Row::Header(h) => Some(h.as_str()),
                        Row::Entry(_) => None,
                    })
                    .unwrap_or("");
                let s = &e.song;
                let cover = s.thumbnail.as_deref().and_then(|u| {
                    crate::cover_r(
                        &self.app,
                        u,
                        crate::ROW_COVER,
                        crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 44.0),
                    )
                });
                Some(HistoryItem {
                    header: "".into(),
                    song: SongItem {
                        id: s.id.clone().into(),
                        title: s.title.clone().into(),
                        artists: s.artists.clone().into(),
                        duration: when(e.at_ms, group, self.offset_s).into(),
                        liked: s.liked,
                        index: (*i + 1) as i32,
                        has_cover: cover.is_some(),
                        cover: cover.unwrap_or_default(),
                        artist_id: s.artist_id.clone().unwrap_or_default().into(),
                        album_id: s.album_id.clone().unwrap_or_default().into(),
                        plays: Default::default(),
                    },
                    event: e.event_id as i32,
                    index: *i as i32,
                })
            }
        }
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

/// Opens the history (the whole of it: thousands of rows are cheap, covers load lazily).
pub fn show(ui: &MainWindow, app: &App) {
    ui.set_view_subtitle("Загрузка…".into());
    let entries = app
        .runtime()
        .block_on(app.history(0, 100_000))
        .unwrap_or_default();
    let n = entries.len();
    let model = Rc::new(HistoryModel::new(app.clone(), entries));
    ui.set_history_items(ModelRc::from(model.clone()));
    MODEL.with(|m| *m.borrow_mut() = Some(model));
    ui.set_view_subtitle(
        if n == 0 {
            "Здесь появится то, что вы слушали".to_string()
        } else {
            format!("{} · {}", crate::nav::plays_word(n), "в этом приложении")
        }
        .into(),
    );
}

/// Plays the history from a song (the rest of the history follows, newest to oldest).
pub fn play(app: &App, index: usize) -> Option<String> {
    MODEL.with(|m| {
        let m = m.borrow();
        let model = m.as_ref()?;
        let songs = model.songs();
        let id = songs.get(index)?.id.clone();
        app.play_rows(songs, index);
        Some(id)
    })
}

pub fn remove(ui: &MainWindow, app: &App, event_id: i64) {
    let app2 = app.clone();
    app.runtime().spawn(async move {
        let _ = app2.remove_history_event(event_id).await;
    });
    MODEL.with(|m| {
        if let Some(model) = m.borrow().as_ref() {
            model.remove(event_id);
            ui.set_view_subtitle(
                format!(
                    "{} · в этом приложении",
                    crate::nav::plays_word(model.songs().len())
                )
                .into(),
            );
        }
    });
}

pub fn refresh_covers(ui: &MainWindow) {
    if ui.get_view_history() {
        MODEL.with(|m| {
            if let Some(model) = m.borrow().as_ref() {
                model.refresh_covers();
            }
        });
    }
}
