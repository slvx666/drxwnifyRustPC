//! Page navigation in Mini: every list the centre shows (library item, search, album, artist) is a `Page`.
//! Pages go through `go`, which keeps a back/forward history like a browser (the arrows in the top bar).

use crate::MainWindow;
use crate::lazy_songs::LazySongs;
use drx_app::{App, ListHeader, ListSpec};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct Page {
    pub spec: ListSpec,
    pub title: String,
    /// Shown before the track count («Альбом · Prado»); empty for plain lists.
    pub subtitle: String,
    /// Library row to highlight ("" when the page is not in the library).
    pub selected: String,
}

#[derive(Default)]
struct History {
    pages: Vec<Page>,
    at: usize,
}

thread_local! {
    static HISTORY: RefCell<History> = RefCell::default();
    /// The centre list model (UI thread only; async loads find it here when they finish).
    static SONGS: RefCell<Option<Rc<LazySongs>>> = const { RefCell::new(None) };
}

pub fn init(songs: &Rc<LazySongs>) {
    SONGS.with(|s| *s.borrow_mut() = Some(songs.clone()));
}

fn current_handle() -> Option<drx_app::ListHandle> {
    SONGS.with(|s| s.borrow().as_ref().map(|m| m.handle()))
}

/// Opens a page and remembers it (drops the forward part of the history, like a browser).
pub fn go(ui: &MainWindow, app: &App, songs: &Rc<LazySongs>, page: Page) {
    HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        if !h.pages.is_empty() {
            let at = h.at;
            h.pages.truncate(at + 1);
        }
        h.pages.push(page.clone());
        h.at = h.pages.len() - 1;
        // A long session should not grow without bound.
        if h.pages.len() > 50 {
            h.pages.remove(0);
            h.at -= 1;
        }
    });
    show(ui, app, songs, page);
    update_arrows(ui);
}

/// Back (-1) or forward (+1) in the history.
pub fn step(ui: &MainWindow, app: &App, songs: &Rc<LazySongs>, delta: i32) {
    let page = HISTORY.with(|h| {
        let mut h = h.borrow_mut();
        let to = h.at as i32 + delta;
        if to < 0 || to as usize >= h.pages.len() {
            return None;
        }
        h.at = to as usize;
        Some(h.pages[h.at].clone())
    });
    if let Some(p) = page {
        show(ui, app, songs, p);
    }
    update_arrows(ui);
}

fn update_arrows(ui: &MainWindow) {
    let (back, fwd) = HISTORY.with(|h| {
        let h = h.borrow();
        (h.at > 0, h.at + 1 < h.pages.len())
    });
    ui.set_can_back(back);
    ui.set_can_forward(fwd);
}

fn count_line(prefix: &str, n: usize) -> String {
    if prefix.is_empty() {
        format!("{n} треков")
    } else {
        format!("{prefix} · {n} треков")
    }
}

fn set_header_image(ui: &MainWindow, app: &App, url: Option<String>) {
    let url = url.unwrap_or_default();
    ui.set_view_cover_url(url.clone().into());
    match (!url.is_empty())
        .then(|| crate::cover(app, &url, crate::HEADER_COVER))
        .flatten()
    {
        Some(c) => {
            ui.set_view_cover(c);
            ui.set_has_view_cover(true);
        }
        None => ui.set_has_view_cover(false),
    }
}

fn apply_header(ui: &MainWindow, app: &App, h: &ListHeader, n: usize) {
    if !h.title.is_empty() {
        ui.set_view_title(h.title.clone().into());
    }
    ui.set_view_subtitle(count_line(&h.subtitle, n).into());
    set_header_image(ui, app, h.image.clone());
}

fn show(ui: &MainWindow, app: &App, songs: &Rc<LazySongs>, page: Page) {
    songs.reopen(page.spec.clone());
    ui.set_selected_id(page.selected.clone().into());
    ui.set_view_title(page.title.clone().into());
    ui.set_current_index(-1);
    set_header_image(ui, app, None);
    let has_page_header = matches!(
        page.spec,
        ListSpec::Album { .. }
            | ListSpec::Artist { .. }
            | ListSpec::CatalogAlbum { .. }
            | ListSpec::CatalogArtist { .. }
    );
    ui.set_view_is_page(has_page_header);
    if !page.spec.is_remote() {
        match app.runtime().block_on(app.local_header(&page.spec)) {
            Some(h) => apply_header(ui, app, &h, songs.len()),
            None => ui.set_view_subtitle(count_line(&page.subtitle, songs.len()).into()),
        }
        return;
    }
    ui.set_view_subtitle(
        if matches!(page.spec, ListSpec::Search { .. }) {
            "Поиск…"
        } else {
            "Загрузка…"
        }
        .into(),
    );
    let (app2, h, ui_w) = (app.clone(), songs.handle(), ui.as_weak());
    app.runtime().spawn(async move {
        let res = app2.load_remote(h).await;
        let header = app2.list_header(h);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            // Another page was opened meanwhile.
            if current_handle() != Some(h) {
                return;
            }
            match (res, header) {
                (Ok(n), Some(head)) => apply_header(&ui, &app2, &head, n),
                (Ok(n), None) => ui.set_view_subtitle(count_line(&page.subtitle, n).into()),
                (Err(e), _) => ui.set_view_subtitle(format!("Не удалось загрузить: {e}").into()),
            }
        });
    });
}
