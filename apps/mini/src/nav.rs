//! Page navigation in Mini: every list the centre shows (library item, search, album, artist) is a `Page`.
//! Pages go through `go`, which keeps a back/forward history like a browser (the arrows in the top bar).

use crate::MainWindow;
use crate::lazy_songs::LazySongs;
use drx_app::{App, ListHeader, ListSpec};
use slint::{ComponentHandle, Model};
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
    /// The home page (rows of cards); `spec` is then unused.
    pub home: bool,
    /// The settings page; `spec` is then unused.
    pub settings: bool,
}

impl Page {
    pub fn home() -> Self {
        Page {
            spec: ListSpec::LikedSongs,
            title: "Главная".into(),
            subtitle: String::new(),
            selected: String::new(),
            home: true,
            settings: false,
        }
    }

    pub fn settings() -> Self {
        Page {
            spec: ListSpec::LikedSongs,
            title: "Настройки".into(),
            subtitle: String::new(),
            selected: String::new(),
            home: false,
            settings: true,
        }
    }
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
    /// Releases of the artist page on screen (their covers arrive later).
    static RELEASES: RefCell<Vec<drx_catalog::model::Release>> = const { RefCell::new(Vec::new()) };
    static RELEASE_MODEL: RefCell<Option<Rc<slint::VecModel<crate::ReleaseItem>>>> = const { RefCell::new(None) };
}

fn release_items(app: &App) -> Vec<crate::ReleaseItem> {
    RELEASES.with(|r| {
        r.borrow()
            .iter()
            .map(|rel| {
                let url = drx_catalog::model::best_image(&rel.images, 300).map(|i| i.url.clone());
                let cover = url.and_then(|u| crate::cover(app, &u, crate::RELEASE_COVER));
                let kind = match rel.kind.as_str() {
                    "single" => "Сингл",
                    "ep" => "EP",
                    "compilation" => "Сборник",
                    _ => "Альбом",
                };
                let year = rel.date.as_deref().and_then(|d| d.get(..4));
                crate::ReleaseItem {
                    id: rel.id.clone().into(),
                    title: rel.name.clone().into(),
                    subtitle: match year {
                        Some(y) => format!("{y} · {kind}"),
                        None => kind.to_string(),
                    }
                    .into(),
                    has_cover: cover.is_some(),
                    cover: cover.unwrap_or_default(),
                }
            })
            .collect()
    })
}

/// Covers arrived: fills in the release cards still waiting for theirs (the strip keeps its scroll position).
pub fn refresh_releases(_ui: &MainWindow, app: &App) {
    RELEASE_MODEL.with(|m| {
        let m = m.borrow();
        let Some(model) = m.as_ref() else { return };
        for (i, item) in release_items(app).into_iter().enumerate() {
            if item.has_cover && model.row_data(i).is_some_and(|c| !c.has_cover) {
                model.set_row_data(i, item);
            }
        }
    });
}

fn set_releases(ui: &MainWindow, app: &App, list: Vec<drx_catalog::model::Release>) {
    RELEASES.with(|r| *r.borrow_mut() = list);
    let model = Rc::new(slint::VecModel::from(release_items(app)));
    ui.set_view_releases(slint::ModelRc::from(model.clone()));
    RELEASE_MODEL.with(|m| *m.borrow_mut() = Some(model));
}

pub fn init(songs: &Rc<LazySongs>) {
    SONGS.with(|s| *s.borrow_mut() = Some(songs.clone()));
}

fn current_handle() -> Option<drx_app::ListHandle> {
    SONGS.with(|s| s.borrow().as_ref().map(|m| m.handle()))
}

fn current_len() -> usize {
    SONGS.with(|s| s.borrow().as_ref().map_or(0, |m| m.len()))
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
    set_releases(ui, app, h.releases.clone());
}

fn show(ui: &MainWindow, app: &App, songs: &Rc<LazySongs>, page: Page) {
    ui.set_view_home(page.home);
    ui.set_view_settings(page.settings);
    if page.settings {
        ui.set_selected_id("".into());
        ui.set_view_title(page.title.clone().into());
        ui.set_view_subtitle("Аккаунты и оформление".into());
        ui.set_view_is_page(false);
        set_header_image(ui, app, None);
        set_releases(ui, app, Vec::new());
        crate::settings_view::fill(ui, app);
        return;
    }
    if page.home {
        ui.set_selected_id("".into());
        ui.set_view_title(page.title.clone().into());
        ui.set_view_is_page(false);
        set_header_image(ui, app, None);
        set_releases(ui, app, Vec::new());
        crate::home_view::show(ui, app, false);
        return;
    }
    songs.reopen(page.spec.clone());
    ui.set_selected_id(page.selected.clone().into());
    ui.set_view_title(page.title.clone().into());
    ui.set_current_index(-1);
    set_header_image(ui, app, None);
    set_releases(ui, app, Vec::new());
    let has_page_header = matches!(
        page.spec,
        ListSpec::Album { .. }
            | ListSpec::Artist { .. }
            | ListSpec::CatalogAlbum { .. }
            | ListSpec::CatalogArtist { .. }
    );
    ui.set_view_is_page(has_page_header);
    if !page.spec.is_remote() {
        let local = app.runtime().block_on(app.local_header(&page.spec));
        match &local {
            Some(h) => apply_header(ui, app, h, songs.len()),
            None => ui.set_view_subtitle(count_line(&page.subtitle, songs.len()).into()),
        }
        let (name, local_image) = local.map(|h| (h.title, h.image)).unwrap_or_default();
        // A library artist who is also in a catalog: its photo and releases join the page.
        if let ListSpec::Artist { id } = &page.spec {
            let (app2, h, ui_w, id) = (app.clone(), songs.handle(), ui.as_weak(), id.clone());
            app.runtime().spawn(async move {
                let Some(mut head) = app2.library_artist_header(&id, &name).await else {
                    return;
                };
                // The library's own picture and name stay when it has them.
                head.title = name;
                if head.image.is_none() {
                    head.image = local_image;
                }
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_w.upgrade() else { return };
                    if current_handle() != Some(h) {
                        return;
                    }
                    let n = current_len();
                    apply_header(&ui, &app2, &head, n);
                });
            });
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
