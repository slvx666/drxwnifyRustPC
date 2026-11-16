//! The library panel as a lazy model: covers are requested only for rows the view actually shows
//! (a VK account alone brings hundreds of playlists).

use crate::LibItem;
use drx_app::{App, LibraryEntry, LibraryKind};
use slint::{Model, ModelNotify, ModelTracker, SharedString};
use std::cell::RefCell;
use std::collections::HashSet;

pub struct LazyLibrary {
    app: App,
    entries: RefCell<Vec<LibraryEntry>>,
    /// Rows drawn without their cover yet; only these redraw when covers arrive (no flicker of the others).
    missing: RefCell<HashSet<usize>>,
    notify: ModelNotify,
}

impl LazyLibrary {
    pub fn new(app: App) -> Self {
        Self {
            app,
            entries: RefCell::default(),
            missing: RefCell::default(),
            notify: ModelNotify::default(),
        }
    }

    pub fn set(&self, entries: Vec<LibraryEntry>) {
        *self.entries.borrow_mut() = entries;
        self.missing.borrow_mut().clear();
        self.notify.reset();
    }

    /// Covers arrived: redraw the rows still waiting for theirs.
    pub fn refresh_covers(&self) {
        let waiting: Vec<usize> = self.missing.borrow().iter().copied().collect();
        for row in waiting {
            let ready = self
                .entries
                .borrow()
                .get(row)
                .and_then(|e| e.thumbnail.clone())
                .is_some_and(|u| self.app.image(&u, crate::ROW_COVER).is_some());
            if ready {
                self.missing.borrow_mut().remove(&row);
                self.notify.row_changed(row);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }
}

impl Model for LazyLibrary {
    type Data = LibItem;

    fn row_count(&self) -> usize {
        self.entries.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<LibItem> {
        let e = self.entries.borrow().get(row).cloned()?;
        let cover = e.thumbnail.as_deref().and_then(|u| {
            crate::cover_r(
                &self.app,
                u,
                crate::ROW_COVER,
                crate::theme_r(crate::ROW_COVER_R, crate::ROW_COVER, 44.0),
            )
        });
        if cover.is_none() && e.thumbnail.is_some() {
            self.missing.borrow_mut().insert(row);
        }
        Some(LibItem {
            title: e.title.into(),
            subtitle: e.subtitle.into(),
            kind: SharedString::from(kind_name(e.kind)),
            thumb: e.thumbnail.clone().unwrap_or_default().into(),
            id: e.id.into(),
            has_cover: cover.is_some(),
            cover: cover.unwrap_or_default(),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

/// The `kind` string the UI uses for a library entry.
pub fn kind_name(k: LibraryKind) -> &'static str {
    match k {
        LibraryKind::Liked => "liked",
        LibraryKind::Playlist => "playlist",
        LibraryKind::Album => "album",
        LibraryKind::Artist => "artist",
        LibraryKind::Remote => "remote",
        LibraryKind::Top => "top",
    }
}
