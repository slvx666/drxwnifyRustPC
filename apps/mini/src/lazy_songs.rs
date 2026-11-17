//! A Slint list model over a core list handle. Rows are fetched in pages of 64 when the view needs them,
//! so a 10 000-track playlist costs only the rows on screen (Mini memory budget).

use crate::SongItem;
use drx_app::{App, ListHandle, ListSpec, SongRow};
use slint::{Model, ModelNotify, ModelTracker};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

const PAGE: usize = 64;

pub struct LazySongs {
    app: App,
    handle: Cell<ListHandle>,
    len: Cell<usize>,
    pages: RefCell<HashMap<usize, Vec<SongRow>>>,
    /// Rows drawn without their cover yet: only these are redrawn when covers arrive, so rows that have theirs
    /// never blink (resetting the whole model made every cover reload and the list flicker).
    missing: RefCell<HashSet<usize>>,
    notify: ModelNotify,
}

impl LazySongs {
    pub fn new(app: App, spec: ListSpec) -> Self {
        let handle = app.open_list(spec);
        let len = app.runtime().block_on(app.list_len(handle)).unwrap_or(0);
        Self {
            app,
            handle: Cell::new(handle),
            len: Cell::new(len),
            pages: RefCell::default(),
            missing: RefCell::default(),
            notify: ModelNotify::default(),
        }
    }

    pub fn handle(&self) -> ListHandle {
        self.handle.get()
    }

    pub fn len(&self) -> usize {
        self.len.get()
    }

    /// Switches to another list (library item clicked). The list is empty at once and fills when the count and
    /// the first rows come from the background (`nav::list_loaded` follows) — the UI thread never waits for the
    /// database: under load (a game, a busy disk) counting the liked songs took up to a second.
    pub fn reopen(&self, spec: ListSpec) {
        self.app.close_list(self.handle.get());
        self.handle.set(self.app.open_list(spec));
        self.len.set(0);
        self.pages.borrow_mut().clear();
        self.missing.borrow_mut().clear();
        self.notify.reset();
        self.load_async();
    }

    /// The rows changed: counted again in the background; the rows on screen stay until the new ones come.
    pub fn invalidate(&self) {
        self.load_async();
    }

    fn load_async(&self) {
        let (app, h) = (self.app.clone(), self.handle.get());
        self.app.runtime().spawn(async move {
            let len = app.list_len(h).await.unwrap_or(0);
            let first = app.list_rows(h, 0..PAGE.min(len)).await.unwrap_or_default();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(m) = crate::nav::songs() {
                    m.loaded(h, len, first);
                }
            });
        });
    }

    /// The background count and first rows of list `h` came.
    fn loaded(&self, h: ListHandle, len: usize, first: Vec<SongRow>) {
        if h != self.handle.get() {
            return;
        }
        self.len.set(len);
        {
            let mut pages = self.pages.borrow_mut();
            pages.clear();
            if !first.is_empty() {
                pages.insert(0, first);
            }
        }
        self.missing.borrow_mut().clear();
        self.notify.reset();
        crate::nav::list_loaded(h);
    }

    /// The liked flag of a cached row with this id.
    pub fn liked_of(&self, id: &str) -> Option<bool> {
        self.pages
            .borrow()
            .values()
            .flat_map(|p| p.iter())
            .find(|r| r.id == id)
            .map(|r| r.liked)
    }

    /// A heart flipped: cached rows of this track show it at once.
    pub fn set_liked(&self, id: &str, liked: bool) {
        let mut changed = Vec::new();
        for (page, rows) in self.pages.borrow_mut().iter_mut() {
            for (i, r) in rows.iter_mut().enumerate() {
                if r.id == id && r.liked != liked {
                    r.liked = liked;
                    changed.push(page * PAGE + i);
                }
            }
        }
        for row in changed {
            self.notify.row_changed(row);
        }
    }

    /// Covers arrived: redraw the rows still waiting for theirs.
    pub fn refresh_covers(&self) {
        let waiting: Vec<usize> = self.missing.borrow().iter().copied().collect();
        for row in waiting {
            let ready = self
                .row(row)
                .and_then(|r| r.thumbnail)
                .is_some_and(|u| crate::covers::ready(&self.app, &u, crate::ROW_COVER));
            if ready {
                self.missing.borrow_mut().remove(&row);
                self.notify.row_changed(row);
            }
        }
    }

    pub fn row(&self, i: usize) -> Option<SongRow> {
        if i >= self.len.get() {
            return None;
        }
        let page = i / PAGE;
        if !self.pages.borrow().contains_key(&page) {
            let rows = self
                .app
                .runtime()
                .block_on(
                    self.app
                        .list_rows(self.handle.get(), page * PAGE..(page + 1) * PAGE),
                )
                .unwrap_or_default();
            let mut pages = self.pages.borrow_mut();
            // Keep memory bounded: a handful of pages around what is on screen.
            if pages.len() > 32 {
                pages.clear();
            }
            pages.insert(page, rows);
        }
        self.pages
            .borrow()
            .get(&page)
            .and_then(|p| p.get(i % PAGE))
            .cloned()
    }
}

impl Model for LazySongs {
    type Data = SongItem;

    fn row_count(&self) -> usize {
        self.len.get()
    }

    fn row_data(&self, row: usize) -> Option<SongItem> {
        self.row(row).map(|r| {
            let cover = r.thumbnail.as_deref().and_then(|u| {
                crate::cover_r(
                    &self.app,
                    u,
                    crate::ROW_COVER,
                    crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 44.0),
                )
            });
            if cover.is_none() && r.thumbnail.is_some() {
                self.missing.borrow_mut().insert(row);
            }
            SongItem {
                id: r.id.into(),
                title: r.title.into(),
                artists: r.artists.into(),
                duration: crate::fmt_time(r.duration_s as f64).into(),
                liked: r.liked,
                plays: r.plays.map(crate::group_digits).unwrap_or_default().into(),
                artist_id: r.artist_id.unwrap_or_default().into(),
                album_id: r.album_id.unwrap_or_default().into(),
                index: row as i32 + 1,
                has_cover: cover.is_some(),
                cover: cover.unwrap_or_default(),
            }
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}
