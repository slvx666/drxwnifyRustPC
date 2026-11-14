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
    /// The library row's cover (playlists of the accounts have no other heading).
    pub image: Option<String>,
    /// The search page for this query and place; `spec` is then unused.
    pub search: Option<(String, drx_app::SearchSource)>,
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
            image: None,
            search: None,
        }
    }

    pub fn search(query: &str, source: drx_app::SearchSource) -> Self {
        Page {
            spec: ListSpec::LikedSongs,
            title: "Поиск".into(),
            subtitle: String::new(),
            selected: String::new(),
            home: false,
            settings: false,
            image: None,
            search: Some((query.to_string(), source)),
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
            image: None,
            search: None,
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
                let cover = url.and_then(|u| {
                    crate::cover_r(
                        app,
                        &u,
                        crate::RELEASE_COVER,
                        crate::theme_r(100.0, crate::RELEASE_COVER, 128.0),
                    )
                });
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

/// The artist view's popular tracks from the current list (after it loads or changes).
pub fn refresh_artist_tracks(ui: &MainWindow) {
    SONGS.with(|s| {
        if let Some(m) = s.borrow().as_ref() {
            crate::artist_view::set_tracks(ui, m);
        }
    });
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

/// «1 трек», «3 трека», «12 треков».
pub fn tracks_word(n: usize) -> String {
    let w = match (n % 10, n % 100) {
        (1, x) if x != 11 => "трек",
        (2..=4, x) if !(12..=14).contains(&x) => "трека",
        _ => "треков",
    };
    format!("{n} {w}")
}

/// «47 мин», «1 ч 3 мин».
pub fn length_words(secs: i64) -> String {
    let m = (secs + 30) / 60;
    if m >= 60 {
        format!("{} ч {} мин", m / 60, m % 60)
    } else {
        format!("{} мин", m.max(1))
    }
}

/// «Альбом · Prado · 2021 · 12 треков · 47 мин».
fn count_line(prefix: &str, n: usize, secs: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !prefix.is_empty() {
        parts.push(prefix.to_string());
    }
    parts.push(tracks_word(n));
    if secs > 0 && n > 0 {
        parts.push(length_words(secs));
    }
    parts.join(" · ")
}

/// Kind caption over the title and whether the heading shows the liked heart instead of a cover.
fn kind_of(spec: &ListSpec) -> (&'static str, bool) {
    match spec {
        ListSpec::LikedSongs
        | ListSpec::SpotifyLiked
        | ListSpec::YandexLiked
        | ListSpec::VkMyMusic => ("ПЛЕЙЛИСТ", true),
        ListSpec::Playlist { .. }
        | ListSpec::SpotifyPlaylist { .. }
        | ListSpec::YandexPlaylist { .. }
        | ListSpec::VkPlaylist { .. } => ("ПЛЕЙЛИСТ", false),
        ListSpec::Album { .. } | ListSpec::CatalogAlbum { .. } => ("АЛЬБОМ", false),
        ListSpec::Artist { .. } | ListSpec::CatalogArtist { .. } => ("ИСПОЛНИТЕЛЬ", false),
        ListSpec::History | ListSpec::RecentSongs => ("ИСТОРИЯ", false),
        _ => ("", false),
    }
}

fn set_header_image(ui: &MainWindow, app: &App, url: Option<String>) {
    let url = url.unwrap_or_default();
    ui.set_view_cover_url(url.clone().into());
    match (!url.is_empty())
        .then(|| {
            crate::cover_r(
                app,
                &url,
                crate::HEADER_COVER,
                crate::theme_r(100.0, crate::HEADER_COVER, 132.0),
            )
        })
        .flatten()
    {
        Some(c) => {
            ui.set_view_cover(c);
            ui.set_has_view_cover(true);
        }
        None => ui.set_has_view_cover(false),
    }
}

fn apply_header(ui: &MainWindow, app: &App, h: &ListHeader, n: usize, secs: i64) {
    if !h.title.is_empty() {
        ui.set_view_title(h.title.clone().into());
    }
    // A release's own kind names the caption (СИНГЛ, EP, СБОРНИК instead of АЛЬБОМ).
    match h.subtitle.split(" · ").next() {
        Some("Сингл") => ui.set_view_kind("СИНГЛ".into()),
        Some("EP") => ui.set_view_kind("EP".into()),
        Some("Сборник") => ui.set_view_kind("СБОРНИК".into()),
        _ => {}
    }
    // The subtitle's own first word is the kind («Альбом · …»): the caption above says it already.
    let sub = h
        .subtitle
        .split_once(" · ")
        .filter(|(k, _)| matches!(*k, "Альбом" | "Сингл" | "EP" | "Сборник" | "Исполнитель"))
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| {
            if matches!(h.subtitle.as_str(), "Исполнитель" | "Альбом") {
                String::new()
            } else {
                h.subtitle.clone()
            }
        });
    // An artist page says listeners and genres, not a track count (Spotify, Android).
    if ui.get_view_artist() && !sub.is_empty() {
        ui.set_view_subtitle(sub.into());
    } else {
        ui.set_view_subtitle(count_line(&sub, n, secs).into());
    }
    if h.image.is_some() {
        set_header_image(ui, app, h.image.clone());
    }
    if ui.get_view_artist() {
        crate::artist_view::set_header(ui, app, h);
    }
    set_releases(ui, app, h.releases.clone());
}

fn show(ui: &MainWindow, app: &App, songs: &Rc<LazySongs>, page: Page) {
    ui.set_view_home(page.home);
    ui.set_view_settings(page.settings);
    ui.set_view_artist(
        matches!(
            page.spec,
            ListSpec::Artist { .. } | ListSpec::CatalogArtist { .. }
        ) && !page.home
            && !page.settings
            && page.search.is_none(),
    );
    crate::artist_view::clear(ui);
    ui.set_view_search(page.search.is_some());
    if let Some((q, source)) = &page.search {
        ui.set_selected_id("".into());
        ui.set_view_is_page(false);
        ui.set_view_kind("".into());
        set_header_image(ui, app, None);
        set_releases(ui, app, Vec::new());
        crate::search_view::show(ui, app, q, *source);
        return;
    }
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
    set_header_image(ui, app, page.image.clone());
    set_releases(ui, app, Vec::new());
    let (kind, liked) = kind_of(&page.spec);
    let search = matches!(page.spec, ListSpec::Search { .. });
    ui.set_view_kind(kind.into());
    ui.set_view_liked_cover(liked);
    ui.set_view_is_page(!search);
    ui.set_view_can_play(false);
    if !page.spec.is_remote() {
        let n = songs.len();
        let secs = app
            .runtime()
            .block_on(app.list_seconds(songs.handle()))
            .unwrap_or(0);
        ui.set_view_can_play(n > 0);
        crate::artist_view::set_tracks(ui, songs);
        let local = app.runtime().block_on(app.local_header(&page.spec));
        match &local {
            Some(h) => apply_header(ui, app, h, n, secs),
            None => ui.set_view_subtitle(count_line(&page.subtitle, n, secs).into()),
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
                    let secs = app2.runtime().block_on(app2.list_seconds(h)).unwrap_or(0);
                    apply_header(&ui, &app2, &head, n, secs);
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
        let secs = app2.list_seconds(h).await.unwrap_or(0);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            // Another page was opened meanwhile.
            if current_handle() != Some(h) {
                return;
            }
            if let Ok(n) = res {
                ui.set_view_can_play(n > 0 && !search);
                refresh_artist_tracks(&ui);
            }
            match (res, header) {
                (Ok(n), Some(head)) => apply_header(&ui, &app2, &head, n, secs),
                (Ok(n), None) => ui.set_view_subtitle(
                    count_line(&page.subtitle, n, if search { 0 } else { secs }).into(),
                ),
                (Err(e), _) => ui.set_view_subtitle(format!("Не удалось загрузить: {e}").into()),
            }
        });
    });
}
