//! A Slint list model over a core list handle. Rows are fetched in pages of 64 when the view needs them,
//! so a 10 000-track playlist costs only the rows on screen (Mini memory budget).

use crate::SongItem;
use drx_app::{App, ListHandle, ListSpec, SongRow};
use slint::{Model, ModelNotify, ModelTracker};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

const PAGE: usize = 64;

pub struct LazySongs {
    app: App,
    handle: Cell<ListHandle>,
    len: Cell<usize>,
    pages: RefCell<HashMap<usize, Vec<SongRow>>>,
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
            notify: ModelNotify::default(),
        }
    }

    pub fn handle(&self) -> ListHandle {
        self.handle.get()
    }

    pub fn len(&self) -> usize {
        self.len.get()
    }

    /// Switches to another list (library item clicked).
    pub fn reopen(&self, spec: ListSpec) {
        self.app.close_list(self.handle.get());
        self.handle.set(self.app.open_list(spec));
        self.invalidate();
    }

    /// Drops cached pages and re-counts; the view asks for visible rows again.
    pub fn invalidate(&self) {
        let h = self.handle.get();
        // After a backup import the old handle is gone; reopening keeps the same spec semantics.
        let len = self
            .app
            .runtime()
            .block_on(self.app.list_len(h))
            .unwrap_or(0);
        self.len.set(len);
        self.pages.borrow_mut().clear();
        self.notify.reset();
    }

    /// Covers arrived: redraw the visible rows (cached pages are kept).
    pub fn refresh_covers(&self) {
        self.notify.reset();
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
            let cover = r
                .thumbnail
                .as_deref()
                .and_then(|u| crate::cover(&self.app, u, crate::ROW_COVER));
            SongItem {
                id: r.id.into(),
                title: r.title.into(),
                artists: r.artists.into(),
                duration: crate::fmt_time(r.duration_s as f64).into(),
                liked: r.liked,
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
