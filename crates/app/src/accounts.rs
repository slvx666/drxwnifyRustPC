//! Accounts and catalogs in the facade: Spotify, Yandex, VK clients from the vault, the main account
//! (PORTING-2.3.0 §3), account tabs in the library, and loading remote lists.

use crate::lists::{ListHeader, ListSpec};
use crate::playback::Playable;
use crate::{
    App, AppError, AppEvent, AppResult, LibraryEntry, LibraryGroup, LibraryKind, ListHandle,
};
use drx_catalog::model::{Source, Track};
use drx_core::keys;
use std::sync::Arc;

/// Setting keys (PreferenceKeys.kt): main account and per-account library tabs.
pub const MAIN_ACCOUNT_KEY: &str = "primaryMetadataSource";

fn tab_key(s: Source) -> String {
    format!("accountTab_{}", source_name(s))
}

pub fn source_name(s: Source) -> &'static str {
    match s {
        Source::Spotify => "SPOTIFY",
        Source::Yandex => "YANDEX",
        Source::Vk => "VK",
    }
}

fn source_label(s: Source) -> &'static str {
    match s {
        Source::Spotify => "Spotify",
        Source::Yandex => "Яндекс",
        Source::Vk => "ВК",
    }
}

#[derive(Default)]
pub(crate) struct Clients {
    pub spotify: std::sync::OnceLock<Option<Arc<drx_catalog::spotify::Spotify>>>,
    pub yandex: std::sync::OnceLock<Option<Arc<drx_catalog::yandex::Yandex>>>,
    pub vk: std::sync::OnceLock<Option<Arc<drx_catalog::vk::Vk>>>,
    /// Playlists of the accounts, for the library panel (filled in the background).
    pub remote_entries: std::sync::Mutex<Vec<LibraryEntry>>,
}

/// One catalog account as the settings page shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountState {
    pub source: Source,
    pub label: &'static str,
    pub linked: bool,
    pub main: bool,
    /// Its liked songs and playlists are in the library (always for the main account).
    pub tab: bool,
}

fn err(e: impl std::fmt::Display) -> AppError {
    AppError::Other(e.to_string())
}

impl App {
    fn prefs_now(&self) -> drx_core::prefs::Prefs {
        std::fs::read(self.inner.profile.prefs_pb())
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .unwrap_or_default()
    }

    fn vault_now(&self) -> Option<drx_core::secrets::Vault> {
        drx_core::secrets::Vault::load(&self.inner.profile).ok()
    }

    pub(crate) fn spotify(&self) -> Option<Arc<drx_catalog::spotify::Spotify>> {
        self.inner
            .clients
            .spotify
            .get_or_init(|| {
                let v = self.vault_now()?;
                let dc = v.pref(keys::spotify::SP_DC)?.to_string();
                let key = v.pref(keys::spotify::SP_KEY).map(str::to_string);
                Some(Arc::new(drx_catalog::spotify::Spotify::new(
                    self.inner.http.clone(),
                    Some(dc),
                    key,
                )))
            })
            .clone()
    }

    pub(crate) fn yandex(&self) -> Option<Arc<drx_catalog::yandex::Yandex>> {
        self.inner
            .clients
            .yandex
            .get_or_init(|| {
                let token = self
                    .vault_now()?
                    .pref(keys::yandex::ACCESS_TOKEN)?
                    .to_string();
                let uid = self
                    .prefs_now()
                    .get_str(keys::yandex::UID)
                    .map(str::to_string);
                Some(Arc::new(drx_catalog::yandex::Yandex::new(
                    self.inner.http.clone(),
                    Some(token),
                    uid,
                )))
            })
            .clone()
    }

    pub(crate) fn vk(&self) -> Option<Arc<drx_catalog::vk::Vk>> {
        self.inner
            .clients
            .vk
            .get_or_init(|| {
                let token = self.vault_now()?.pref(keys::vk::ACCESS_TOKEN)?.to_string();
                let uid = self
                    .prefs_now()
                    .get_str(keys::vk::USER_ID)
                    .and_then(|u| u.parse().ok());
                Some(Arc::new(drx_catalog::vk::Vk::new(
                    self.inner.http.clone(),
                    Some(token),
                    uid,
                )))
            })
            .clone()
    }

    pub fn has_account(&self, s: Source) -> bool {
        match s {
            Source::Spotify => self.spotify().is_some(),
            Source::Yandex => self.yandex().is_some(),
            Source::Vk => self.vk().is_some(),
        }
    }

    /// The main account (setting, else Spotify when linked, else the first linked one) — §3.
    pub fn main_account(&self) -> Option<Source> {
        let set = self
            .prefs_now()
            .get_str(MAIN_ACCOUNT_KEY)
            .and_then(|v| match v {
                "SPOTIFY" => Some(Source::Spotify),
                "YANDEX" => Some(Source::Yandex),
                "VK" => Some(Source::Vk),
                _ => None,
            });
        set.filter(|s| self.has_account(*s)).or_else(|| {
            [Source::Spotify, Source::Yandex, Source::Vk]
                .into_iter()
                .find(|s| self.has_account(*s))
        })
    }

    /// Every catalog account for the settings page: linked or not, main, shown in the library.
    pub fn accounts_state(&self) -> Vec<AccountState> {
        let prefs = self.prefs_now();
        let main = self.main_account();
        [Source::Spotify, Source::Yandex, Source::Vk]
            .into_iter()
            .map(|s| AccountState {
                source: s,
                label: source_label(s),
                linked: self.has_account(s),
                main: Some(s) == main,
                tab: Some(s) == main || prefs.get_bool(&tab_key(s)) == Some(true),
            })
            .collect()
    }

    /// Makes `s` the main account (catalog of search, home, links). The home page is built anew.
    pub fn set_main_account(&self, s: Source) -> AppResult<()> {
        self.set_pref(
            MAIN_ACCOUNT_KEY,
            drx_core::prefs::PrefValue::String(source_name(s).into()),
        )?;
        let _ = std::fs::remove_file(self.profile().root().join("home_feed.json"));
        Ok(())
    }

    /// Shows or hides an account's liked songs and playlists in the library.
    pub fn set_account_tab(&self, s: Source, on: bool) -> AppResult<()> {
        self.set_pref(&tab_key(s), drx_core::prefs::PrefValue::Bool(on))
    }

    /// Accounts shown in the library: the main one always, the others when their tab is on (off by default).
    pub fn library_accounts(&self) -> Vec<Source> {
        let prefs = self.prefs_now();
        let main = self.main_account();
        [Source::Spotify, Source::Yandex, Source::Vk]
            .into_iter()
            .filter(|s| {
                self.has_account(*s)
                    && (Some(*s) == main || prefs.get_bool(&tab_key(*s)) == Some(true))
            })
            .collect()
    }

    /// «Liked» rows of the accounts in the library, main account first.
    pub(crate) fn account_entries(&self) -> Vec<LibraryEntry> {
        let main = self.main_account();
        let mut accs = self.library_accounts();
        accs.sort_by_key(|s| Some(*s) != main);
        accs.into_iter()
            .map(|s| {
                let spec = match s {
                    Source::Spotify => ListSpec::SpotifyLiked,
                    Source::Yandex => ListSpec::YandexLiked,
                    Source::Vk => ListSpec::VkMyMusic,
                };
                LibraryEntry {
                    kind: LibraryKind::Remote,
                    id: spec.entry_id().unwrap_or_default(),
                    title: if s == Source::Vk {
                        "ВК · Моя музыка".into()
                    } else {
                        format!("{} · Любимые треки", source_label(s))
                    },
                    subtitle: if Some(s) == main {
                        "Основной аккаунт".into()
                    } else {
                        source_label(s).into()
                    },
                    thumbnail: None,
                    pinned: true,
                    group: LibraryGroup::Liked,
                    added_ms: 0,
                    updated_ms: 0,
                }
            })
            .collect()
    }

    /// Fetches the accounts' playlists (and VK albums) for the library panel; sends [`AppEvent::LibraryChanged`].
    pub async fn refresh_remote_library(&self) -> AppResult<usize> {
        let mut out: Vec<LibraryEntry> = Vec::new();
        for s in self.library_accounts() {
            let label = source_label(s);
            match s {
                Source::Spotify => {
                    if let Some(sp) = self.spotify()
                        && let Ok(page) = sp.my_playlists(0, 100).await
                    {
                        out.extend(page.items.into_iter().map(|p| {
                            LibraryEntry {
                                kind: LibraryKind::Remote,
                                id: ListSpec::SpotifyPlaylist { id: p.id }
                                    .entry_id()
                                    .unwrap_or_default(),
                                title: p.name,
                                subtitle: format!("{label} · Плейлист"),
                                thumbnail: drx_catalog::model::best_image(&p.images, 120)
                                    .map(|i| i.url.clone()),
                                pinned: false,
                                group: LibraryGroup::Playlists,
                                added_ms: 0,
                                updated_ms: 0,
                            }
                        }));
                    }
                }
                Source::Yandex => {
                    if let Some(y) = self.yandex()
                        && let Ok(list) = y.my_playlists().await
                    {
                        out.extend(list.into_iter().map(|p| {
                            LibraryEntry {
                                kind: LibraryKind::Remote,
                                id: ListSpec::YandexPlaylist { id: p.id }
                                    .entry_id()
                                    .unwrap_or_default(),
                                subtitle: format!("{label} · Плейлист · {}", p.total),
                                title: p.name,
                                thumbnail: drx_catalog::model::best_image(&p.images, 120)
                                    .map(|i| i.url.clone()),
                                pinned: false,
                                group: LibraryGroup::Playlists,
                                added_ms: 0,
                                updated_ms: 0,
                            }
                        }));
                    }
                }
                Source::Vk => {
                    if let Some(vk) = self.vk()
                        && let Ok(list) = vk.my_playlists().await
                    {
                        out.extend(list.into_iter().map(|p| {
                            LibraryEntry {
                                kind: LibraryKind::Remote,
                                id: ListSpec::VkPlaylist {
                                    id: p.playlist.id.clone(),
                                }
                                .entry_id()
                                .unwrap_or_default(),
                                subtitle: if p.is_album {
                                    format!("{label} · Альбом · {}", p.artist)
                                } else {
                                    format!("{label} · Плейлист · {}", p.playlist.total)
                                },
                                title: p.playlist.name,
                                thumbnail: drx_catalog::model::best_image(&p.playlist.images, 120)
                                    .map(|i| i.url.clone()),
                                pinned: false,
                                group: if p.is_album {
                                    LibraryGroup::Albums
                                } else {
                                    LibraryGroup::Playlists
                                },
                                added_ms: 0,
                                updated_ms: 0,
                            }
                        }));
                    }
                }
            }
        }
        let n = out.len();
        *self
            .inner
            .clients
            .remote_entries
            .lock()
            .expect("remote entries") = out;
        let _ = self.inner.events.send(AppEvent::LibraryChanged);
        Ok(n)
    }

    pub(crate) fn remote_entries(&self) -> Vec<LibraryEntry> {
        self.inner
            .clients
            .remote_entries
            .lock()
            .expect("remote entries")
            .clone()
    }

    fn set_rows(&self, handle: ListHandle, rows: Vec<Playable>, version: u64) {
        self.inner
            .remote
            .lock()
            .expect("remote")
            .insert(handle, rows);
        let _ = self
            .inner
            .events
            .send(AppEvent::ListChanged { handle, version });
    }

    /// Loads a remote list; long lists (Spotify liked) arrive page by page, each page announced.
    pub async fn load_remote(&self, handle: ListHandle) -> AppResult<usize> {
        let spec = self
            .inner
            .lists
            .spec(handle)
            .ok_or_else(|| AppError::Other("list is closed".into()))?;
        let plain = |t: Vec<Track>| -> Vec<Playable> {
            t.into_iter()
                .map(|track| Playable { track, url: None })
                .collect()
        };
        let rows: Vec<Playable> = match &spec {
            ListSpec::VkMyMusic | ListSpec::VkPlaylist { .. } => {
                let vk = self.vk().ok_or_else(|| err("нет входа во ВКонтакте"))?;
                let found = match &spec {
                    ListSpec::VkPlaylist { id } => {
                        let (o, p, k) = drx_catalog::vk::parse_playlist_id(id)
                            .ok_or_else(|| err("bad VK id"))?;
                        vk.playlist_tracks(o, p, k).await
                    }
                    _ => vk.my_tracks().await,
                }
                .map_err(err)?;
                found
                    .into_iter()
                    .map(|a| Playable {
                        track: a.track,
                        url: a.url,
                    })
                    .collect()
            }
            ListSpec::SpotifyLiked | ListSpec::SpotifyPlaylist { .. } => {
                let sp = self.spotify().ok_or_else(|| err("нет входа в Spotify"))?;
                let mut all: Vec<Playable> = Vec::new();
                let mut offset = 0u32;
                let mut version = 1;
                loop {
                    let page = match &spec {
                        ListSpec::SpotifyPlaylist { id } => {
                            sp.playlist_tracks(id, offset, 100).await
                        }
                        _ => sp.liked_songs(offset, 50).await,
                    }
                    .map_err(err)?;
                    let n = page.items.len() as u32;
                    all.extend(plain(page.items));
                    offset += n;
                    self.set_rows(handle, all.clone(), version);
                    version += 1;
                    if n == 0 || offset >= page.total || self.inner.lists.spec(handle).is_none() {
                        break;
                    }
                }
                all
            }
            ListSpec::YandexLiked | ListSpec::YandexPlaylist { .. } => {
                let y = self.yandex().ok_or_else(|| err("нет входа в Яндекс"))?;
                let page = match &spec {
                    ListSpec::YandexPlaylist { id } => y.playlist_tracks(id, 0, 10_000).await,
                    _ => y.liked_songs(0, 10_000).await,
                }
                .map_err(err)?;
                plain(page.items)
            }
            ListSpec::Search { query } => {
                // The main account's catalog answers; Yandex and VK when Spotify is not linked.
                let found = match self.main_account() {
                    Some(Source::Yandex) => self
                        .yandex()
                        .ok_or_else(|| err("нет Яндекса"))?
                        .search(query, 50)
                        .await
                        .map_err(err)?,
                    Some(Source::Vk) => {
                        let vk = self.vk().ok_or_else(|| err("нет ВК"))?;
                        let found = vk.search(query, 50).await.map_err(err)?;
                        self.set_rows(
                            handle,
                            found
                                .into_iter()
                                .map(|a| Playable {
                                    track: a.track,
                                    url: a.url,
                                })
                                .collect(),
                            u64::MAX,
                        );
                        return Ok(self
                            .inner
                            .remote
                            .lock()
                            .expect("remote")
                            .get(&handle)
                            .map(Vec::len)
                            .unwrap_or(0));
                    }
                    _ => match self.spotify() {
                        Some(sp) => sp.search_tracks(query, 50).await.map_err(err)?,
                        None => {
                            drx_catalog::spotify::Spotify::new(self.inner.http.clone(), None, None)
                                .search_tracks(query, 50)
                                .await
                                .map_err(err)?
                        }
                    },
                };
                plain(found)
            }
            ListSpec::CatalogAlbum { id } => {
                let album = match Source::of_id(id) {
                    Source::Vk => {
                        let vk = self.vk().ok_or_else(|| err("нет входа во ВКонтакте"))?;
                        let (o, p, k) = drx_catalog::vk::parse_playlist_id(id)
                            .ok_or_else(|| err("bad VK id"))?;
                        let audios = vk.playlist_tracks(o, p, k).await.map_err(err)?;
                        let first = audios.first().and_then(|a| a.track.album.clone());
                        let header = ListHeader {
                            title: first.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                            subtitle: "Альбом · ВКонтакте".into(),
                            image: first.as_ref().and_then(|a| {
                                drx_catalog::model::best_image(&a.images, 600)
                                    .map(|i| i.url.clone())
                            }),
                            releases: Vec::new(),
                        };
                        self.set_header(handle, header);
                        let rows: Vec<Playable> = audios
                            .into_iter()
                            .map(|a| Playable {
                                track: a.track,
                                url: a.url,
                            })
                            .collect();
                        let n = rows.len();
                        self.set_rows(handle, rows, u64::MAX);
                        return Ok(n);
                    }
                    Source::Yandex => {
                        let y = self.yandex().ok_or_else(|| err("нет входа в Яндекс"))?;
                        y.album(id).await.map_err(err)?
                    }
                    Source::Spotify => {
                        let sp = match self.spotify() {
                            Some(sp) => sp,
                            None => Arc::new(drx_catalog::spotify::Spotify::new(
                                self.inner.http.clone(),
                                None,
                                None,
                            )),
                        };
                        sp.album(id).await.map_err(err)?
                    }
                };
                self.set_header(handle, album_header(&album));
                plain(album.tracks)
            }
            ListSpec::CatalogArtist { id } => match Source::of_id(id) {
                Source::Vk => {
                    let vk = self.vk().ok_or_else(|| err("нет входа во ВКонтакте"))?;
                    let (page, audios) = vk.artist(id).await.map_err(err)?;
                    self.set_header(handle, artist_header(&page));
                    audios
                        .into_iter()
                        .map(|a| Playable {
                            track: a.track,
                            url: a.url,
                        })
                        .collect()
                }
                Source::Yandex => {
                    let y = self.yandex().ok_or_else(|| err("нет входа в Яндекс"))?;
                    let page = y.artist(id).await.map_err(err)?;
                    self.set_header(handle, artist_header(&page));
                    plain(page.top_tracks)
                }
                Source::Spotify => {
                    let sp = match self.spotify() {
                        Some(sp) => sp,
                        None => Arc::new(drx_catalog::spotify::Spotify::new(
                            self.inner.http.clone(),
                            None,
                            None,
                        )),
                    };
                    let page = sp.artist(id).await.map_err(err)?;
                    self.set_header(handle, artist_header(&page));
                    plain(page.top_tracks)
                }
            },
            _ => return Ok(0),
        };
        let n = rows.len();
        self.set_rows(handle, rows, u64::MAX);
        Ok(n)
    }
}

/// «Альбом · Исполнитель · 2021» (or «Сингл», «EP»…), with the biggest cover.
fn album_header(a: &drx_catalog::model::Album) -> ListHeader {
    let kind = match a.album_type.as_deref() {
        Some("single") => "Сингл",
        Some("ep") => "EP",
        Some("compilation") => "Сборник",
        _ => "Альбом",
    };
    let mut sub = vec![kind.to_string()];
    let artists = a
        .artists
        .iter()
        .map(|x| x.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if !artists.is_empty() {
        sub.push(artists);
    }
    if let Some(y) = a.release_date.as_deref().and_then(|d| d.get(..4)) {
        sub.push(y.to_string());
    }
    ListHeader {
        title: a.name.clone(),
        subtitle: sub.join(" · "),
        image: drx_catalog::model::best_image(&a.images, 600).map(|i| i.url.clone()),
        releases: Vec::new(),
    }
}

fn artist_header(p: &drx_catalog::model::ArtistPage) -> ListHeader {
    ListHeader {
        title: p.name.clone(),
        subtitle: "Исполнитель · популярное".into(),
        image: drx_catalog::model::best_image(&p.images, 600).map(|i| i.url.clone()),
        releases: p.releases.clone(),
    }
}
