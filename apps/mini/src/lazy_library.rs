//! The library panel as a lazy model: covers are requested only for rows the view actually shows
//! (a VK account alone brings hundreds of playlists).

use crate::LibItem;
use drx_app::{App, LibraryEntry, LibraryKind};
use slint::{Model, ModelNotify, ModelTracker, SharedString};
use std::cell::RefCell;

pub struct LazyLibrary {
    app: App,
    entries: RefCell<Vec<LibraryEntry>>,
    notify: ModelNotify,
}

impl LazyLibrary {
    pub fn new(app: App) -> Self {
        Self {
            app,
            entries: RefCell::default(),
            notify: ModelNotify::default(),
        }
    }

    pub fn set(&self, entries: Vec<LibraryEntry>) {
        *self.entries.borrow_mut() = entries;
        self.notify.reset();
    }

    pub fn refresh_covers(&self) {
        self.notify.reset();
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
                crate::theme_r(10.0, crate::ROW_COVER, 44.0),
            )
        });
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
    }
}
