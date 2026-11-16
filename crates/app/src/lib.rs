//! Facade for both UIs: commands, queries, list handles, state snapshots and the edition profile.
//! The UIs depend on this crate only (see docs/ARCHITECTURE.md). No business logic lives in the UIs.

mod accounts;
pub mod chats;
pub mod listen_along;
pub mod edition;
pub mod genres;
mod history;
mod home;
pub mod likes;
pub mod lists;
pub mod playback;
pub mod playlists;
pub mod radio;
pub mod rooms;
pub mod sleep;
pub mod downloads;
pub mod sponsorblock;
pub mod sound;
pub use history::{HISTORY_DURATION_KEY, PAUSE_HISTORY_KEY};
mod queue_store;
mod search;
pub mod social;
pub mod songs;
mod stats;

pub use accounts::AccountState;
pub use drx_audio::player::PlaybackClock;
use drx_core::http::Http;
use drx_core::paths::Profile;
use drx_core::store::Store;
use drx_transfer::backup::{self, Accounts, ImportOptions};
pub use edition::{Edition, EditionProfile};
pub use home::{HomeAlbum, HomeFeed, HomeRow, HomeRowKind};
pub use lists::{ListHandle, ListHeader, ListSpec, SongRow, TopPeriod};
pub use playback::{NowPlaying, Playable, Repeat};
pub use search::{SearchOutcome, SearchSource, fold, link_page, share_text};
pub use stats::{HistoryEntry, StatItem, StatPeriod, StatSong, Stats, history_group};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, watch};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] drx_core::CoreError),
    #[error(transparent)]
    Backup(#[from] backup::BackupError),
    #[error(transparent)]
    Http(#[from] drx_core::http::HttpError),
    #[error("{0}")]
    Other(String),
}

pub type AppResult<T> = std::result::Result<T, AppError>;

/// What the UI can ask the core to do. Every command returns when it is done (or failed).
#[derive(Debug, Clone)]
pub enum Command {
    /// Replace the profile with an Android (or PC) backup. A snapshot of the current profile is kept.
    ImportBackup {
        path: PathBuf,
        keep_youtube_session: bool,
    },
    /// Write the profile as a backup the phone can restore.
    CreateBackup { path: PathBuf },
}

#[derive(Debug, Clone)]
pub enum CommandOutcome {
    Done,
    Imported {
        songs: i64,
        playlists: i64,
        accounts: Accounts,
        chat_cursors_reset: usize,
        youtube_session_dropped: bool,
    },
}

/// One row of the library panel (left column).
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryEntry {
    pub kind: LibraryKind,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub thumbnail: Option<String>,
    pub pinned: bool,
    /// What the row is to the filter chips (a remote VK album is an album, a Spotify playlist a playlist).
    pub group: LibraryGroup,
    /// When it came into the library / was last changed (Unix ms; 0 = unknown, e.g. account playlists).
    pub added_ms: i64,
    pub updated_ms: i64,
}

/// Library filter groups (Android LibraryFilter: the chips).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryGroup {
    Liked,
    Playlists,
    Albums,
    Artists,
}

/// Library order (Android MixSortType).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MixSort {
    /// «Недавно добавленные»
    #[default]
    Added,
    /// «Название»
    Name,
    /// «Дата обновления»
    Updated,
}

/// How the library panel is shown: chip filter, order, a search in the panel.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LibraryView {
    pub filter: Option<LibraryGroup>,
    pub sort: MixSort,
    pub descending: bool,
    pub query: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryKind {
    Liked,
    Playlist,
    Album,
    Artist,
    /// A list of a linked account (liked, playlist, album), loaded from its catalog. `id` is the list's entry id.
    Remote,
    /// «Мой топ N» (an auto playlist of the most listened songs).
    Top,
}

/// Library numbers shown on the home page and in settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryCounts {
    pub songs: i64,
    pub liked: i64,
    pub playlists: i64,
    pub artists: i64,
    pub history_events: i64,
}

/// Whole-app state the UI renders. Sent as a new snapshot whenever something in it changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppState {
    pub library: LibraryCounts,
    pub accounts: Accounts,
}

/// Events the UI may react to.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// Rows of an open list changed; the UI asks for the visible range again.
    ListChanged { handle: ListHandle, version: u64 },
    /// The whole library was replaced (backup import): reopen lists.
    LibraryReplaced,
    /// Library panel rows changed (account playlists arrived): ask for [`App::library_entries`] again.
    LibraryChanged,
    /// An image requested with [`App::request_image`] is decoded and in the cache.
    ImageReady { url: String, size: u32 },
    /// A short word for the user (a toast), e.g. «Пропущено: заставка».
    Notice(String),
}

struct Inner {
    profile: Profile,
    edition: EditionProfile,
    store: RwLock<Store>,
    http: Http,
    state_tx: watch::Sender<AppState>,
    events: broadcast::Sender<AppEvent>,
    lists: lists::Lists,
    playback: Arc<playback::Playback>,
    /// Rows of remote lists (VK), filled when the list is opened.
    remote: Mutex<HashMap<ListHandle, Vec<Playable>>>,
    headers: Mutex<HashMap<ListHandle, ListHeader>>,
    /// One home build at a time.
    home_lock: tokio::sync::Mutex<()>,
    clients: accounts::Clients,
    images: drx_core::images::Images,
    /// One writer at a time for the Android shared-prefs files (cross_catalog, vk_saved_tracks).
    shared_lock: Mutex<()>,
    /// Tracks the UI was handed (lists, search, queue), by id: a heart on any row knows the whole track.
    known: Mutex<HashMap<String, drx_catalog::model::Track>>,
    /// Artists' genres (Last.fm / MusicBrainz), loaded from the profile on first use.
    genres: Mutex<genres::GenreCache>,
    friends: std::sync::OnceLock<Option<drx_social::friends::Friends>>,
    history_publish: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The friend followed by «слушать вместе» and the task that follows them.
    listen_along: Mutex<Option<(String, tokio::task::JoinHandle<()>)>>,
    listen_along_version: watch::Sender<u64>,
    /// The «Слушать вместе» room, while in one.
    room: Mutex<Option<rooms::RoomSession>>,
    room_version: watch::Sender<u64>,
    sleep: watch::Sender<sleep::SleepState>,
    downloads_version: watch::Sender<u64>,
    download_progress: watch::Sender<downloads::DownloadProgress>,
    download_queue: Mutex<std::collections::VecDeque<drx_catalog::model::Track>>,
    download_running: std::sync::atomic::AtomicBool,
    sleep_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    chats: std::sync::OnceLock<Option<Arc<chats::ChatStore>>>,
}

/// The running core. Cheap to clone; UIs call the async methods through [`App::runtime`].
#[derive(Clone)]
pub struct App {
    inner: Arc<Inner>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl App {
    /// Starts the core for one profile. Mini uses 2 worker threads, Full 4.
    pub fn start(profile: Profile, edition: EditionProfile) -> AppResult<Self> {
        profile.ensure_dirs().map_err(drx_core::CoreError::from)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(edition.worker_threads())
            .max_blocking_threads(4)
            .thread_keep_alive(std::time::Duration::from_secs(10))
            .thread_name("drx-core")
            .enable_all()
            .build()
            .map_err(|e| AppError::Other(e.to_string()))?;
        let store = Store::open(&profile.song_db())?;
        let (state_tx, _) = watch::channel(AppState::default());
        let (events, _) = broadcast::channel(256);
        let http = Http::new()?;
        let playback =
            playback::Playback::new(http.clone(), runtime.handle().clone(), profile.cache_dir());
        let images = drx_core::images::Images::new(
            http.clone(),
            profile.cache_dir(),
            edition.image_cache_bytes,
        );
        let inner = Arc::new(Inner {
            http,
            playback,
            remote: Mutex::default(),
            headers: Mutex::default(),
            home_lock: tokio::sync::Mutex::new(()),
            clients: accounts::Clients::default(),
            images,
            shared_lock: Mutex::new(()),
            known: Mutex::default(),
            genres: Mutex::default(),
            friends: std::sync::OnceLock::new(),
            history_publish: Mutex::default(),
            listen_along: Mutex::default(),
            listen_along_version: watch::channel(0).0,
            room: Mutex::default(),
            room_version: watch::channel(0).0,
            sleep: watch::channel(sleep::SleepState::default()).0,
            downloads_version: watch::channel(0).0,
            download_progress: watch::channel(downloads::DownloadProgress::default()).0,
            download_queue: Mutex::default(),
            download_running: std::sync::atomic::AtomicBool::new(false),
            sleep_task: Mutex::default(),
            chats: std::sync::OnceLock::new(),
            lists: lists::Lists::default(),
            profile,
            edition,
            store: RwLock::new(store),
            state_tx,
            events,
        });
        let app = Self {
            inner,
            runtime: Arc::new(runtime),
        };
        app.watch_tables();
        app.install_providers();
        app.restore_shuffle_repeat();
        app.start_history();
        app.start_social_publishing();
        let a = app.clone();
        app.runtime.spawn(async move {
            let _ = a.refresh_state().await;
        });
        Ok(app)
    }

    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.runtime
    }
    pub fn edition(&self) -> &EditionProfile {
        &self.inner.edition
    }
    pub fn http(&self) -> &Http {
        &self.inner.http
    }
    pub fn profile(&self) -> &Profile {
        &self.inner.profile
    }

    /// Changes one setting in `settings.preferences_pb` (the file the phone's backup carries).
    pub fn set_pref(&self, key: &str, value: drx_core::prefs::PrefValue) -> AppResult<()> {
        let path = self.inner.profile.prefs_pb();
        let mut prefs = std::fs::read(&path)
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .unwrap_or_default();
        prefs.set(key, value);
        drx_core::paths::write_atomic(&path, &prefs.encode())
            .map_err(|e| AppError::Other(e.to_string()))
    }
    /// A text setting from `settings.preferences_pb`.
    pub fn pref_string(&self, key: &str) -> Option<String> {
        std::fs::read(self.inner.profile.prefs_pb())
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .and_then(|p| p.get_str(key).map(str::to_string))
    }
    /// A switch from `settings.preferences_pb`.
    pub fn pref_bool(&self, key: &str) -> Option<bool> {
        std::fs::read(self.inner.profile.prefs_pb())
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .and_then(|p| p.get_bool(key))
    }
    /// «Пропускать трек, если не включается» (Android autoSkipNextOnError) applied to the player.
    pub fn apply_playback_prefs(&self) {
        let on = self.pref_bool("autoSkipNextOnError").unwrap_or(false);
        self.inner.playback.skip_on_error.store(on, std::sync::atomic::Ordering::Relaxed);
        *self.inner.playback.lyrics_sources.lock().expect("lyrics sources") =
            (self.pref_bool("enableLrclib").unwrap_or(true), self.pref_bool("enableKugou").unwrap_or(true));
        self.apply_sound();
    }
    /// How many songs «Мой топ» holds (the phone's `topSize`, a text setting; 50 by default).
    pub fn top_size(&self) -> usize {
        self.pref_string("topSize").and_then(|s| s.trim().parse().ok()).filter(|n| *n > 0).unwrap_or(50).min(500)
    }

    /// The period «Мой топ» shows (PC: `pcTopPeriod`, the phone's MyTopFilter names).
    pub fn top_period(&self) -> TopPeriod {
        self.pref_string("pcTopPeriod").map(|s| TopPeriod::from_name(&s)).unwrap_or_default()
    }

    pub fn set_top_period(&self, p: TopPeriod) {
        let _ = self.set_pref("pcTopPeriod", drx_core::prefs::PrefValue::String(p.name().into()));
    }

    /// A number setting stored as a float in `settings.preferences_pb`.
    pub fn pref_f32(&self, key: &str) -> Option<f32> {
        std::fs::read(self.inner.profile.prefs_pb())
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .and_then(|p| p.get_f32(key))
    }
    /// A number setting from `settings.preferences_pb`.
    pub fn pref_i64(&self, key: &str) -> Option<i64> {
        std::fs::read(self.inner.profile.prefs_pb())
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .and_then(|p| p.get_i64(key))
    }
    fn store(&self) -> Store {
        self.inner.store.read().expect("store lock").clone()
    }

    pub fn state(&self) -> watch::Receiver<AppState> {
        self.inner.state_tx.subscribe()
    }
    pub fn events(&self) -> broadcast::Receiver<AppEvent> {
        self.inner.events.subscribe()
    }

    /// Subscribes to table changes of the current store and forwards them to open lists and the state.
    fn watch_tables(&self) {
        let mut rx = self.store().subscribe();
        let app = self.clone();
        self.runtime.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ch) => {
                        for (handle, version) in app.inner.lists.touch(&ch.tables) {
                            let _ = app
                                .inner
                                .events
                                .send(AppEvent::ListChanged { handle, version });
                        }
                        let _ = app.refresh_state().await;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    async fn refresh_state(&self) -> AppResult<()> {
        let library = self
            .store()
            .call(|c| {
                let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
                Ok(LibraryCounts {
                    songs: n("SELECT count(*) FROM song WHERE inLibrary IS NOT NULL")?,
                    liked: n(&format!("SELECT count(*) FROM {}", lists::LIKED_UNIQUE))?,
                    playlists: n("SELECT count(*) FROM playlist")?,
                    artists: n("SELECT count(*) FROM artist WHERE bookmarkedAt IS NOT NULL")?,
                    history_events: n("SELECT count(*) FROM event")?,
                })
            })
            .await?;
        let accounts = self.accounts_now()?;
        self.inner.state_tx.send_if_modified(|s| {
            let new = AppState { library, accounts };
            let changed = *s != new;
            *s = new;
            changed
        });
        Ok(())
    }

    fn accounts_now(&self) -> AppResult<Accounts> {
        use drx_core::keys;
        let p = &self.inner.profile;
        let prefs = match std::fs::read(p.prefs_pb()) {
            Ok(b) => drx_core::prefs::Prefs::decode(&b)?,
            Err(_) => Default::default(),
        };
        let vault = drx_core::secrets::Vault::load(p)?;
        let has = |k: &str| vault.pref(k).is_some_and(|v| !v.is_empty());
        let name = |present: bool, key: &str, fallback: &str| {
            present.then(|| {
                prefs
                    .get_str(key)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(fallback)
                    .to_string()
            })
        };
        Ok(Accounts {
            spotify: name(
                has(keys::spotify::SP_DC),
                keys::spotify::USERNAME,
                "Spotify",
            ),
            yandex: name(
                has(keys::yandex::ACCESS_TOKEN),
                keys::yandex::USERNAME,
                "Yandex",
            ),
            vk: name(has(keys::vk::ACCESS_TOKEN), keys::vk::USER_ID, "VK"),
            youtube: name(
                has(keys::youtube::COOKIE),
                keys::youtube::ACCOUNT_NAME,
                "YouTube",
            ),
            friends_key: vault
                .shared(keys::sp::FRIENDS, keys::sp::FRIENDS_SECRET_KEY)
                .is_some(),
        })
    }

    /// Runs a command.
    pub async fn run(&self, cmd: Command) -> AppResult<CommandOutcome> {
        match cmd {
            Command::CreateBackup { path } => {
                let profile = self.inner.profile.clone();
                tokio::task::spawn_blocking(move || backup::create_backup(&profile, &path))
                    .await
                    .map_err(|e| AppError::Other(e.to_string()))??;
                Ok(CommandOutcome::Done)
            }
            Command::ImportBackup {
                path,
                keep_youtube_session,
            } => {
                let profile = self.inner.profile.clone();
                let scratch = profile.cache_dir();
                // Check first, so a bad file never closes the current library.
                let pv = {
                    let path = path.clone();
                    tokio::task::spawn_blocking(move || backup::preview(&path, &scratch))
                        .await
                        .map_err(|e| AppError::Other(e.to_string()))??
                };
                self.store().shutdown();
                let opts = ImportOptions {
                    keep_youtube_session,
                };
                let result = {
                    let profile = profile.clone();
                    tokio::task::spawn_blocking(move || {
                        backup::import_android_backup(&path, &profile, &opts)
                    })
                    .await
                    .map_err(|e| AppError::Other(e.to_string()))?
                };
                // Reopen whatever is on disk now (new data, or the old file if the import failed early).
                let store = Store::open(&profile.song_db())?;
                *self.inner.store.write().expect("store lock") = store;
                // New sessions came with the backup: rebuild the clients and audio sources.
                // (The VK client is created once per run; a new import needs a restart to switch accounts.)
                self.install_providers();
                self.watch_tables();
                self.inner.lists.clear();
                let _ = self.inner.events.send(AppEvent::LibraryReplaced);
                let report = result?;
                self.refresh_state().await?;
                Ok(CommandOutcome::Imported {
                    songs: pv.songs,
                    playlists: pv.playlists,
                    accounts: report.accounts,
                    chat_cursors_reset: report.chat_cursors_reset,
                    youtube_session_dropped: report.youtube_session_dropped,
                })
            }
        }
    }

    /// Rows of the library panel: local liked songs, the accounts' liked lists, local playlists, saved albums and
    /// artists, then the accounts' playlists (after [`App::refresh_remote_library`]).
    pub async fn library_entries(&self) -> AppResult<Vec<LibraryEntry>> {
        let mut entries = self.db_library_entries().await?;
        let accounts = self.account_entries();
        let at = 1.min(entries.len());
        entries.splice(at..at, accounts);
        entries.extend(self.remote_entries());
        Ok(entries)
    }

    /// The library panel as the view asks: chip filter, order, search (LibraryMixScreen). The liked lists stay on top
    /// (Android pins «Любимые»); under a search they are filtered like everything else.
    pub async fn library_view(&self, view: &LibraryView) -> AppResult<Vec<LibraryEntry>> {
        let mut all = self.library_entries().await?;
        if let Some(f) = view.filter {
            // The playlists chip keeps the liked lists: they are playlists to the listener (auto playlists).
            all.retain(|e| {
                e.group == f || (f == LibraryGroup::Playlists && e.group == LibraryGroup::Liked)
            });
        }
        let q = view.query.trim().to_lowercase();
        if !q.is_empty() {
            all.retain(|e| {
                e.title.to_lowercase().contains(&q) || e.subtitle.to_lowercase().contains(&q)
            });
        }
        let (mut pinned, mut rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|e| e.pinned);
        match view.sort {
            MixSort::Added => rest.sort_by_key(|e| e.added_ms),
            MixSort::Updated => rest.sort_by_key(|e| e.updated_ms),
            MixSort::Name => rest.sort_by_key(|e| e.title.to_lowercase()),
        }
        // Newest/«Я…» first when descending; rows without a date (account playlists) keep their order at the end.
        if view.descending {
            rest.reverse();
            if view.sort != MixSort::Name {
                let (dated, undated): (Vec<_>, Vec<_>) = rest.into_iter().partition(|e| {
                    (if view.sort == MixSort::Added {
                        e.added_ms
                    } else {
                        e.updated_ms
                    }) > 0
                });
                rest = dated;
                let mut undated = undated;
                undated.reverse();
                rest.extend(undated);
            }
        }
        pinned.extend(rest);
        Ok(pinned)
    }

    /// The saved library view (phone keys: `chipSortType`, `mixSortType`, `albumSortDescending`).
    pub fn library_view_saved(&self) -> LibraryView {
        let prefs = std::fs::read(self.inner.profile.prefs_pb())
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .unwrap_or_default();
        LibraryView {
            filter: match prefs.get_str("chipSortType") {
                Some("PLAYLISTS") => Some(LibraryGroup::Playlists),
                Some("ALBUMS") => Some(LibraryGroup::Albums),
                Some("ARTISTS") => Some(LibraryGroup::Artists),
                _ => None,
            },
            sort: match prefs.get_str("mixSortType") {
                Some("NAME") => MixSort::Name,
                Some("LAST_UPDATED") => MixSort::Updated,
                _ => MixSort::Added,
            },
            descending: prefs.get_bool("albumSortDescending").unwrap_or(true),
            query: String::new(),
        }
    }

    /// Saves the chip, order and direction (the search is not kept).
    pub fn save_library_view(&self, v: &LibraryView) -> AppResult<()> {
        use drx_core::prefs::PrefValue;
        let chip = match v.filter {
            Some(LibraryGroup::Playlists) => "PLAYLISTS",
            Some(LibraryGroup::Albums) => "ALBUMS",
            Some(LibraryGroup::Artists) => "ARTISTS",
            _ => "LIBRARY",
        };
        let sort = match v.sort {
            MixSort::Added => "CREATE_DATE",
            MixSort::Name => "NAME",
            MixSort::Updated => "LAST_UPDATED",
        };
        self.set_pref("chipSortType", PrefValue::String(chip.into()))?;
        self.set_pref("mixSortType", PrefValue::String(sort.into()))?;
        self.set_pref("albumSortDescending", PrefValue::Bool(v.descending))
    }

    async fn db_library_entries(&self) -> AppResult<Vec<LibraryEntry>> {
        let covers = self.profile().covers_dir();
        let top_size = self.top_size();
        Ok(self
            .store()
            .call(move |c| {
                let liked: i64 = c.query_row(&format!("SELECT count(*) FROM {}", lists::LIKED_UNIQUE), [], |r| r.get(0))?;
                let mut out = vec![
                    LibraryEntry {
                        kind: LibraryKind::Liked,
                        id: "liked".into(),
                        title: "Любимые треки".into(),
                        subtitle: format!("{liked} треков"),
                        thumbnail: None,
                        pinned: true,
                        group: LibraryGroup::Liked,
                        added_ms: 0,
                        updated_ms: 0,
                    },
                    // The phone's «Мой топ N» auto playlist (topSize, 50 by default).
                    LibraryEntry {
                        kind: LibraryKind::Top,
                        id: "top".into(),
                        title: format!("Мой топ {top_size}"),
                        subtitle: "Автоплейлист".into(),
                        thumbnail: None,
                        pinned: true,
                        group: LibraryGroup::Liked,
                        added_ms: 0,
                        updated_ms: 0,
                    },
                ];
                let mut st = c.prepare(&format!(
                    "SELECT p.id, p.name, p.thumbnailUrl, (SELECT count(*) FROM playlist_song_map m WHERE m.playlistId = p.id),
                     p.createdAt, coalesce(p.lastUpdateTime, p.createdAt), {PLAYLIST_FIRST_COVER}
                     FROM playlist p ORDER BY coalesce(p.lastUpdateTime, p.createdAt) DESC"
                ))?;
                let rows = st.query_map([], |r| {
                    Ok(LibraryEntry {
                        kind: LibraryKind::Playlist,
                        id: r.get(0)?,
                        title: r.get(1)?,
                        subtitle: format!("Плейлист · {}", r.get::<_, i64>(3)?),
                        thumbnail: playlist_cover(&covers, r.get(2)?, r.get(6)?),
                        pinned: false,
                        group: LibraryGroup::Playlists,
                        added_ms: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        updated_ms: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    })
                })?;
                for r in rows {
                    out.push(r?);
                }
                let mut st = c.prepare(
                    "SELECT a.id, a.title, a.thumbnailUrl, (SELECT group_concat(ar.name, ', ') FROM album_artist_map m JOIN artist ar ON ar.id = m.artistId WHERE m.albumId = a.id),
                     a.bookmarkedAt, a.lastUpdateTime
                     FROM album a WHERE a.bookmarkedAt IS NOT NULL ORDER BY a.bookmarkedAt DESC",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(LibraryEntry {
                        kind: LibraryKind::Album,
                        id: r.get(0)?,
                        title: r.get(1)?,
                        subtitle: format!("Альбом · {}", r.get::<_, Option<String>>(3)?.unwrap_or_default()),
                        thumbnail: r.get(2)?,
                        pinned: false,
                        group: LibraryGroup::Albums,
                        added_ms: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        updated_ms: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    })
                })?;
                for r in rows {
                    out.push(r?);
                }
                let mut st = c.prepare(
                    "SELECT id, name, thumbnailUrl, bookmarkedAt, lastUpdateTime FROM artist WHERE bookmarkedAt IS NOT NULL ORDER BY bookmarkedAt DESC",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(LibraryEntry {
                        kind: LibraryKind::Artist,
                        id: r.get(0)?,
                        title: r.get(1)?,
                        subtitle: "Исполнитель".into(),
                        thumbnail: r.get(2)?,
                        pinned: false,
                        group: LibraryGroup::Artists,
                        added_ms: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        updated_ms: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    })
                })?;
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await?)
    }

    /// Opens a live list. Rows are fetched by range; [`AppEvent::ListChanged`] tells when to fetch again.
    pub fn open_list(&self, spec: ListSpec) -> ListHandle {
        self.inner.lists.open(spec)
    }

    pub fn close_list(&self, handle: ListHandle) {
        self.inner.lists.close(handle);
        self.inner.headers.lock().expect("headers").remove(&handle);
    }

    /// The heading of a catalog page, once its load has fetched it.
    pub fn list_header(&self, handle: ListHandle) -> Option<ListHeader> {
        self.inner
            .headers
            .lock()
            .expect("headers")
            .get(&handle)
            .cloned()
    }

    pub(crate) fn set_header(&self, handle: ListHandle, h: ListHeader) {
        self.inner
            .headers
            .lock()
            .expect("headers")
            .insert(handle, h);
    }

    /// The page an artist link opens: the database page when the library has this artist's songs, otherwise
    /// the catalog page (popular tracks from Spotify, Yandex or VK).
    pub async fn artist_page(&self, id: &str) -> ListSpec {
        let id2 = id.to_string();
        let local = self
            .store()
            .call(move |c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM song_artist_map WHERE artistId = ?1)",
                    [&id2],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .await
            .unwrap_or(false);
        if local {
            ListSpec::Artist { id: id.to_string() }
        } else {
            ListSpec::CatalogArtist { id: id.to_string() }
        }
    }

    /// The page an album link opens: the saved album from the database, otherwise the catalog album.
    pub async fn album_page(&self, id: &str) -> ListSpec {
        let id2 = id.to_string();
        let local = self
            .store()
            .call(move |c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM song_album_map WHERE albumId = ?1)",
                    [&id2],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .await
            .unwrap_or(false);
        if local {
            ListSpec::Album { id: id.to_string() }
        } else {
            ListSpec::CatalogAlbum { id: id.to_string() }
        }
    }

    /// Heading of a database playlist, album or artist page (catalog pages get theirs from `list_header` after loading).
    pub async fn local_header(&self, spec: &ListSpec) -> Option<ListHeader> {
        let spec = spec.clone();
        let covers = self.profile().covers_dir();
        self.store()
            .call(move |c| {
                Ok(match &spec {
                    ListSpec::Playlist { id } => c
                        .query_row(
                            &format!("SELECT p.name, p.thumbnailUrl, {PLAYLIST_FIRST_COVER} FROM playlist p WHERE p.id = ?1"),
                            [id],
                            |r| {
                                Ok(ListHeader {
                                    title: r.get(0)?,
                                    subtitle: String::new(),
                                    image: playlist_cover(&covers, r.get(1)?, r.get(2)?),
                                    releases: Vec::new(),
                                    appears_on: Vec::new(),
                                    related: Vec::new(),
                                })
                            },
                        )
                        .ok(),
                    ListSpec::Album { id } => c
                        .query_row(
                            "SELECT a.title, a.year, a.thumbnailUrl, (SELECT group_concat(ar.name, ', ') FROM album_artist_map m JOIN artist ar ON ar.id = m.artistId WHERE m.albumId = a.id) FROM album a WHERE a.id = ?1",
                            [id],
                            |r| {
                                let mut sub = vec!["Альбом".to_string()];
                                if let Some(a) = r.get::<_, Option<String>>(3)? {
                                    sub.push(a);
                                }
                                if let Some(y) = r.get::<_, Option<i64>>(1)? {
                                    sub.push(y.to_string());
                                }
                                Ok(ListHeader {
                                    title: r.get(0)?,
                                    subtitle: sub.join(" · "),
                                    image: r.get(2)?,
                                    releases: Vec::new(),
                                    appears_on: Vec::new(),
                                    related: Vec::new(),
                                })
                            },
                        )
                        .ok(),
                    ListSpec::Artist { id } => c
                        .query_row(
                            "SELECT name, thumbnailUrl FROM artist WHERE id = ?1",
                            [id],
                            |r| {
                                Ok(ListHeader {
                                    title: r.get(0)?,
                                    subtitle: "Исполнитель".into(),
                                    image: r.get(1)?,
                                    releases: Vec::new(),
                                    appears_on: Vec::new(),
                                    related: Vec::new(),
                                })
                            },
                        )
                        .ok(),
                    _ => None,
                })
            })
            .await
            .ok()
            .flatten()
    }

    /// Number of rows in a list.
    pub async fn list_len(&self, handle: ListHandle) -> AppResult<usize> {
        let spec = self
            .inner
            .lists
            .spec(handle)
            .ok_or_else(|| AppError::Other("list is closed".into()))?;
        if spec.is_remote() {
            return Ok(self
                .inner
                .remote
                .lock()
                .expect("remote")
                .get(&handle)
                .map(Vec::len)
                .unwrap_or(0));
        }
        Ok(self.store().call(move |c| lists::count(c, &spec)).await?)
    }

    /// Rows `range` of a list.
    pub async fn list_rows(
        &self,
        handle: ListHandle,
        range: std::ops::Range<usize>,
    ) -> AppResult<Vec<SongRow>> {
        let spec = self
            .inner
            .lists
            .spec(handle)
            .ok_or_else(|| AppError::Other("list is closed".into()))?;
        if spec.is_remote() {
            let mut out: Vec<SongRow> = {
                let remote = self.inner.remote.lock().expect("remote");
                let rows = remote.get(&handle).map(Vec::as_slice).unwrap_or(&[]);
                let end = range.end.min(rows.len());
                let start = range.start.min(end);
                rows[start..end].iter().map(row_of).collect()
            };
            let flags = self.liked_flags(out.iter().map(|r| r.id.clone()).collect()).await;
            for (r, f) in out.iter_mut().zip(flags) {
                r.liked = f;
            }
            return Ok(out);
        }
        Ok(self
            .store()
            .call(move |c| lists::rows(c, &spec, range))
            .await?)
    }

    /// Audio sources available with this profile's sessions.
    fn install_providers(&self) {
        *self.inner.playback.store.lock().expect("store") = Some(self.store());
        let mut list: Vec<Arc<dyn drx_resolver::provider::AudioProvider>> = Vec::new();
        list.push(Arc::new(
            drx_resolver::providers::soundcloud::SoundCloud::new(self.inner.http.clone()),
        ));
        list.push(Arc::new(drx_resolver::providers::youtube::YouTube::new(
            self.inner.http.clone(),
        )));
        if let Some(vk) = self.vk() {
            list.push(Arc::new(drx_resolver::providers::vk::VkProvider::new(vk)));
        }
        *self.inner.playback.providers.lock().expect("providers") = list;
    }

    pub fn has_vk(&self) -> bool {
        self.vk().is_some()
    }

    /// Plays a list from `index`; the list becomes the queue.
    pub async fn play_list(&self, handle: ListHandle, index: usize) -> AppResult<()> {
        let items = self.list_items(handle).await?;
        self.remember(items.iter().map(|p| &p.track));
        self.inner.playback.play_list(items, index);
        Ok(())
    }

    /// Total length of a list in seconds (for «12 треков · 47 мин» under a page title).
    pub async fn list_seconds(&self, handle: ListHandle) -> AppResult<i64> {
        let spec = self
            .inner
            .lists
            .spec(handle)
            .ok_or_else(|| AppError::Other("list is closed".into()))?;
        if spec.is_remote() {
            return Ok(self
                .inner
                .remote
                .lock()
                .expect("remote")
                .get(&handle)
                .map(|r| r.iter().map(|p| (p.track.duration_ms / 1000) as i64).sum())
                .unwrap_or(0));
        }
        Ok(self
            .store()
            .call(move |c| lists::total_seconds(c, &spec))
            .await?)
    }

    /// Every row of a list as playable tracks.
    async fn list_items(&self, handle: ListHandle) -> AppResult<Vec<Playable>> {
        let spec = self
            .inner
            .lists
            .spec(handle)
            .ok_or_else(|| AppError::Other("list is closed".into()))?;
        let items: Vec<Playable> = if spec.is_remote() {
            self.inner
                .remote
                .lock()
                .expect("remote")
                .get(&handle)
                .cloned()
                .unwrap_or_default()
        } else {
            let n = self.list_len(handle).await?;
            self.list_rows(handle, 0..n)
                .await?
                .into_iter()
                .map(playable_of)
                .collect()
        };
        Ok(items)
    }

    /// A decoded cover if it is already cached at this size (cheap; call from the UI thread).
    pub fn image(&self, url: &str, size: u32) -> Option<Arc<drx_core::images::Pixels>> {
        self.inner.images.cached(url, size)
    }

    /// Starts loading a cover; [`AppEvent::ImageReady`] follows when it is decoded.
    pub fn request_image(&self, url: &str, size: u32) {
        let (images, events, url) = (
            self.inner.images.clone(),
            self.inner.events.clone(),
            url.to_string(),
        );
        self.runtime.spawn(async move {
            if images.load(&url, size).await.is_some() {
                let _ = events.send(AppEvent::ImageReady { url, size });
            }
        });
    }

    pub fn toggle_play(&self) {
        self.inner.playback.toggle();
    }

    /// A list as the phone's PlaylistExporter writes it: `csv` (Title,Artist,Album,YouTube Video ID) or `m3u`
    /// (#EXTINF and #YTM:<id>), into `dir` as `<name>.<ext>`. Returns the file written.
    pub async fn export_list(
        &self,
        handle: ListHandle,
        name: &str,
        format: &str,
        dir: &std::path::Path,
    ) -> Result<std::path::PathBuf, String> {
        let items = self.list_items(handle).await.map_err(|e| e.to_string())?;
        let tracks: Vec<&drx_catalog::model::Track> = items.iter().map(|p| &p.track).collect();
        let (text, ext) = if format == "m3u" { (export_m3u(&tracks), "m3u") } else { (export_csv(&tracks), "csv") };
        let path = unique_path(dir, name, ext);
        tokio::fs::write(&path, text.as_bytes()).await.map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// «Скачать» in the picture viewer (CoverSaver.save): the largest original into `dir` («Загрузки») as
    /// `<name>.<ext>` (` (2)`, ` (3)`… when taken). Returns the file written.
    pub async fn save_image(&self, url: &str, name: &str, dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
        let bytes = self.inner.images.original(url).await.ok_or("картинка не скачалась")?;
        let ext = drx_core::images::image_extension(&bytes);
        let path = unique_path(dir, name, ext);
        tokio::fs::write(&path, &bytes).await.map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// «Перемешать» on a page: the list in random order (shuffle turns on).
    pub async fn play_list_shuffled(&self, handle: ListHandle) -> AppResult<()> {
        let items = self.list_items(handle).await?;
        self.remember(items.iter().map(|p| &p.track));
        self.inner.playback.play_shuffled(items);
        self.save_shuffle_repeat();
        Ok(())
    }

    pub fn set_shuffle(&self, on: bool) {
        self.inner.playback.set_shuffle(on);
        self.save_shuffle_repeat();
    }

    pub fn set_repeat(&self, r: Repeat) {
        self.inner.playback.set_repeat(r);
        self.save_shuffle_repeat();
    }

    /// Shuffle and repeat are remembered like on the phone (`shuffleMode`, `repeatMode`; `rememberShuffleAndRepeat`).
    fn save_shuffle_repeat(&self) {
        let now = self.inner.playback.now.borrow().clone();
        let _ = self.set_pref(
            "repeatMode",
            drx_core::prefs::PrefValue::Int(now.repeat as i32),
        );
        let _ = self.set_pref("shuffleMode", drx_core::prefs::PrefValue::Bool(now.shuffle));
    }

    pub(crate) fn restore_shuffle_repeat(&self) {
        let prefs = std::fs::read(self.inner.profile.prefs_pb())
            .ok()
            .and_then(|b| drx_core::prefs::Prefs::decode(&b).ok())
            .unwrap_or_default();
        let repeat = Repeat::from_i32(prefs.get_i64("repeatMode").unwrap_or(0) as i32);
        let remember = prefs.get_bool("rememberShuffleAndRepeat").unwrap_or(true);
        let shuffle = remember && prefs.get_bool("shuffleMode").unwrap_or(false);
        self.inner.playback.now.send_modify(|n| {
            n.repeat = repeat;
            n.shuffle = shuffle;
        });
    }
    pub fn next_track(&self) {
        self.inner.playback.next();
    }
    pub fn previous_track(&self) {
        self.inner.playback.previous();
    }
    pub fn seek(&self, secs: f64) {
        self.inner.playback.seek(secs);
    }
    pub fn set_volume(&self, v: f32) {
        self.inner.playback.set_volume(v);
    }
    /// The whole queue and the playing index.
    pub fn queue(&self) -> (Vec<drx_catalog::model::Track>, usize) {
        self.inner.playback.queue_snapshot()
    }
    pub fn queue_remove(&self, index: usize) {
        self.inner.playback.queue_remove(index);
    }
    pub fn queue_move(&self, from: usize, to: usize) {
        self.inner.playback.queue_move(from, to);
    }
    pub fn queue_clear_upcoming(&self) {
        self.inner.playback.queue_clear_upcoming();
    }
    pub fn queue_play(&self, index: usize) {
        self.inner.playback.play_index(index);
    }
    pub fn now_playing(&self) -> watch::Receiver<NowPlaying> {
        self.inner.playback.now.subscribe()
    }
    pub fn clock(&self) -> PlaybackClock {
        self.inner.playback.clock()
    }
}

/// `dir/<name>.<ext>` with a name Windows takes (no \/:*?"<>|), ` (2)`, ` (3)`… when taken.
fn unique_path(dir: &std::path::Path, name: &str, ext: &str) -> std::path::PathBuf {
    let base: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect::<String>()
        .trim()
        .trim_end_matches('.')
        .chars()
        .take(120)
        .collect();
    let base = if base.is_empty() { "drxwnify".to_string() } else { base };
    let mut path = dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{base} ({n}).{ext}"));
        n += 1;
    }
    path
}

fn csv_cell(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// The id a phone knows a track by (its song id), for exports.
fn song_id(t: &drx_catalog::model::Track) -> String {
    crate::social::ref_of(t).media_id.unwrap_or_else(|| t.id.clone())
}

/// PlaylistExporter.exportPlaylistAsCSV.
pub fn export_csv(tracks: &[&drx_catalog::model::Track]) -> String {
    let mut out = String::from("Title,Artist,Album,YouTube Video ID\n");
    for t in tracks {
        let artists = t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join("; ");
        let album = t.album.as_ref().map(|a| a.name.as_str()).unwrap_or("");
        out.push_str(&format!("{},{},{},{}\n", csv_cell(&t.title), csv_cell(&artists), csv_cell(album), song_id(t)));
    }
    out
}

/// PlaylistExporter.exportPlaylistAsM3U.
pub fn export_m3u(tracks: &[&drx_catalog::model::Track]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for t in tracks {
        let artists = t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(" - ");
        out.push_str(&format!("#EXTINF:{},{artists} - {}\n", t.duration_ms / 1000, t.title));
        out.push_str(&format!("#YTM:{}\n", song_id(t)));
    }
    out
}

fn row_of(p: &Playable) -> SongRow {
    let t = &p.track;
    SongRow {
        id: t.id.clone(),
        title: t.title.clone(),
        artists: t.artist_names(),
        album: t.album.as_ref().map(|a| a.name.clone()),
        duration_s: (t.duration_ms / 1000) as i64,
        thumbnail: t
            .album
            .as_ref()
            .and_then(|a| drx_catalog::model::best_image(&a.images, 120))
            .map(|i| i.url.clone()),
        liked: false,
        artist_id: t
            .artists
            .first()
            .map(|a| a.id.clone())
            .filter(|i| !i.is_empty()),
        album_id: t
            .album
            .as_ref()
            .map(|a| a.id.clone())
            .filter(|i| !i.is_empty()),
        plays: t.plays.filter(|n| *n > 0),
    }
}

/// A database row as a track. Its audio is found by the resolver (milestone 5), so no URL yet.
fn playable_of(r: SongRow) -> Playable {
    use drx_catalog::model::{AlbumRef, ArtistRef, Track};
    Playable {
        track: Track {
            id: r.id,
            title: r.title,
            // Only the first artist's id is known per row (it links to that artist's page).
            artists: r
                .artists
                .split(", ")
                .filter(|a| !a.is_empty())
                .enumerate()
                .map(|(i, a)| ArtistRef {
                    id: if i == 0 {
                        r.artist_id.clone().unwrap_or_default()
                    } else {
                        String::new()
                    },
                    name: a.to_string(),
                })
                .collect(),
            // The song's own thumbnail stands in for the album cover (now playing shows it).
            album: (r.album.is_some() || r.thumbnail.is_some()).then(|| AlbumRef {
                id: r.album_id.clone().unwrap_or_default(),
                name: r.album.unwrap_or_default(),
                images: r
                    .thumbnail
                    .map(|url| drx_catalog::model::Image { url, width: None })
                    .into_iter()
                    .collect(),
            }),
            duration_ms: r.duration_s.max(0) as u64 * 1000,
            isrc: None,
            plays: None,
            explicit: false,
        },
        url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_real_backup_and_lists_songs() {
        let Ok(path) = std::env::var("DRX_TEST_BACKUP") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let app = App::start(Profile::at(dir.path()), EditionProfile::mini()).unwrap();
        let rt = app.runtime();
        let out = rt
            .block_on(app.run(Command::ImportBackup {
                path: path.into(),
                keep_youtube_session: false,
            }))
            .unwrap();
        let CommandOutcome::Imported {
            songs, accounts, ..
        } = out
        else {
            panic!()
        };
        assert!(songs > 0);
        assert!(accounts.friends_key);
        let h = app.open_list(ListSpec::LikedSongs);
        let n = rt.block_on(app.list_len(h)).unwrap();
        let rows = rt.block_on(app.list_rows(h, 0..20)).unwrap();
        assert!(n > 0 && !rows.is_empty());
        let entries = rt.block_on(app.library_entries()).unwrap();
        assert!(entries.len() > 1 && entries[0].kind == LibraryKind::Liked);
        let state = app.state().borrow().clone();
        eprintln!("liked={n} state={:?}", state.library);
        assert_eq!(state.library.liked as usize, n);
        // Album and artist pages from the library open from the database.
        if let Some(album) = entries.iter().find(|e| e.kind == LibraryKind::Album) {
            let h = app.open_list(ListSpec::Album {
                id: album.id.clone(),
            });
            let n = rt.block_on(app.list_len(h)).unwrap();
            eprintln!("album {:?}: {n} tracks", album.title);
            assert!(n > 0);
        }
        // Statistics and history from the backup's 5789 events.
        let st = rt.block_on(app.stats(StatPeriod::All)).unwrap();
        assert!(st.plays > 0 && st.total_ms > 0 && !st.songs.is_empty() && !st.artists.is_empty());
        assert!(st.songs.windows(2).all(|w| w[0].ms >= w[1].ms));
        let h = rt.block_on(app.history(0, 30)).unwrap();
        assert_eq!(h.len(), 30);
        assert!(h.windows(2).all(|w| w[0].at_ms >= w[1].at_ms));
        eprintln!(
            "stats: plays={} hours={} songs={} artists={} albums={}",
            st.plays,
            st.total_ms / 3_600_000,
            st.songs.len(),
            st.artists.len(),
            st.albums.len()
        );
        // Library view: chips, orders, search (LibraryMixScreen).
        let v = |filter, sort, descending, query: &str| LibraryView {
            filter,
            sort,
            descending,
            query: query.into(),
        };
        let albums = rt
            .block_on(app.library_view(&v(Some(LibraryGroup::Albums), MixSort::Added, true, "")))
            .unwrap();
        assert!(!albums.is_empty() && albums.iter().all(|e| e.group == LibraryGroup::Albums));
        let added: Vec<i64> = albums.iter().map(|e| e.added_ms).collect();
        assert!(added.windows(2).all(|w| w[0] >= w[1]), "newest first");
        let by_name = rt
            .block_on(app.library_view(&v(None, MixSort::Name, false, "")))
            .unwrap();
        let names: Vec<String> = by_name
            .iter()
            .filter(|e| !e.pinned)
            .map(|e| e.title.to_lowercase())
            .collect();
        assert!(names.windows(2).all(|w| w[0] <= w[1]), "А→Я");
        assert!(by_name[0].pinned, "liked stays on top");
        let playlists = rt
            .block_on(app.library_view(&v(
                Some(LibraryGroup::Playlists),
                MixSort::Updated,
                true,
                "",
            )))
            .unwrap();
        assert!(
            playlists
                .iter()
                .all(|e| matches!(e.group, LibraryGroup::Playlists | LibraryGroup::Liked))
        );
        let q = albums[0].title.chars().take(4).collect::<String>();
        let found = rt
            .block_on(app.library_view(&v(None, MixSort::Added, true, &q)))
            .unwrap();
        assert!(found.iter().any(|e| e.id == albums[0].id));
        eprintln!(
            "library view: albums={} playlists={} search={}",
            albums.len(),
            playlists.len(),
            found.len()
        );
        if let Some(artist) = entries.iter().find(|e| e.kind == LibraryKind::Artist) {
            let h = app.open_list(ListSpec::Artist {
                id: artist.id.clone(),
            });
            let n = rt.block_on(app.list_len(h)).unwrap();
            eprintln!("artist: {n} songs");
        }
    }
}

#[cfg(test)]
mod live_playback {
    use super::*;

    /// Plays the first VK «Моя музыка» track through the real device at volume 0 and checks the clock moves.
    /// DRX_TEST_PROFILE = an imported profile folder. Not run in CI.
    #[test]
    fn plays_vk_track() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        assert!(app.has_vk());
        app.set_volume(0.0);
        let rt = app.runtime();
        let h = app.open_list(ListSpec::VkMyMusic);
        let n = rt.block_on(app.load_remote(h)).unwrap();
        assert!(n > 0);
        rt.block_on(app.play_list(h, 0)).unwrap();
        let mut moved = false;
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let c = app.clock();
            if c.position_s > 1.0 {
                eprintln!(
                    "playing: pos={:.1}s dur={:?}",
                    c.position_s,
                    c.duration_s.map(|d| d.round())
                );
                moved = true;
                break;
            }
            if let Some(e) = app.now_playing().borrow().error.clone() {
                panic!("playback error: {e}");
            }
        }
        assert!(moved, "clock did not move");
        app.seek(30.0);
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let c = app.clock();
        eprintln!("after seek: pos={:.1}s", c.position_s);
        assert!(c.position_s > 29.0);
        app.next_track();
        std::thread::sleep(std::time::Duration::from_millis(4000));
        let c = app.clock();
        eprintln!(
            "next track: pos={:.1}s index={:?}",
            c.position_s,
            app.now_playing().borrow().index
        );
    }
}

#[cfg(test)]
mod live_resolve {
    use super::*;

    /// Plays the first liked song from the database (a catalog track without audio): the resolver must find it
    /// on VK. Volume 0. DRX_TEST_PROFILE; not run in CI.
    #[test]
    fn plays_liked_song_via_resolver() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        app.set_volume(0.0);
        let rt = app.runtime();
        let h = app.open_list(ListSpec::LikedSongs);
        let mut ok = 0;
        for i in 0..5 {
            rt.block_on(app.play_list(h, i)).unwrap();
            let mut played = false;
            for _ in 0..80 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if app.clock().position_s > 0.5 {
                    played = true;
                    break;
                }
                if app.now_playing().borrow().error.is_some() {
                    break;
                }
            }
            let np = app.now_playing().borrow().clone();
            eprintln!("liked #{i}: played={played} error={:?}", np.error);
            if played {
                ok += 1;
            }
        }
        assert!(
            ok >= 3,
            "resolver found too few of the first 5 liked songs: {ok}"
        );
    }

    /// Plays the first track of the first saved album (a YouTube Music song: the video itself is the audio).
    /// DRX_TEST_PROFILE; not run in CI.
    #[test]
    fn plays_album_track() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        app.set_volume(0.0);
        let rt = app.runtime();
        let entries = rt.block_on(app.library_entries()).unwrap();
        let Some(album) = entries.iter().find(|e| e.kind == LibraryKind::Album) else {
            return;
        };
        let h = app.open_list(ListSpec::Album {
            id: album.id.clone(),
        });
        rt.block_on(app.play_list(h, 0)).unwrap();
        let mut played = false;
        for _ in 0..80 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if app.clock().position_s > 0.5 {
                played = true;
                break;
            }
            if let Some(e) = app.now_playing().borrow().error.clone() {
                panic!("album track failed: {e}");
            }
        }
        assert!(played, "album track did not start");
    }
}

#[cfg(test)]
mod live_soundcloud {
    use super::*;

    /// SoundCloud HLS (fragmented MP4 AAC) plays: resolve a known SoundCloud track and check the clock.
    /// DRX_TEST_NET=1; not run in CI.
    #[test]
    fn plays_soundcloud_hls() {
        if std::env::var("DRX_TEST_NET").is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let app = App::start(Profile::at(dir.path()), EditionProfile::mini()).unwrap();
        app.set_volume(0.0);
        use drx_catalog::model::{ArtistRef, Track};
        let track = Track {
            id: "test-sc".into(),
            title: "goodbye".into(),
            artists: vec![ArtistRef {
                id: String::new(),
                name: "Sewerslvt".into(),
            }],
            album: None,
            duration_ms: 1_019_000,
            isrc: None,
            plays: None,
            explicit: false,
        };
        app.inner
            .playback
            .play_list(vec![Playable { track, url: None }], 0);
        let mut pos = 0.0;
        for _ in 0..80 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            pos = app.clock().position_s;
            if pos > 1.0 {
                break;
            }
            if let Some(e) = app.now_playing().borrow().error.clone() {
                panic!("{e}");
            }
        }
        eprintln!("soundcloud hls pos={pos:.1}");
        assert!(pos > 1.0);
        app.seek(120.0);
        std::thread::sleep(std::time::Duration::from_millis(3000));
        eprintln!("after seek pos={:.1}", app.clock().position_s);
    }
}

#[cfg(test)]
mod live_youtube {
    use super::*;

    /// A YouTube stream (VISIONOS, WebM Opus) plays and seeks past the first MiB (no 1 MiB preview cap).
    /// DRX_TEST_NET=1; not run in CI.
    #[test]
    fn plays_youtube_and_seeks_far() {
        if std::env::var("DRX_TEST_NET").is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let app = App::start(Profile::at(dir.path()), EditionProfile::mini()).unwrap();
        app.set_volume(0.0);
        // Only YouTube, so the test checks this source.
        app.inner
            .playback
            .providers
            .lock()
            .unwrap()
            .retain(|p| p.id() == drx_resolver::provider::ProviderId::Youtube);
        use drx_catalog::model::{ArtistRef, Track};
        let track = Track {
            id: "test-yt".into(),
            title: "Blinding Lights".into(),
            artists: vec![ArtistRef {
                id: String::new(),
                name: "The Weeknd".into(),
            }],
            album: None,
            duration_ms: 200_000,
            isrc: None,
            plays: None,
            explicit: false,
        };
        app.inner
            .playback
            .play_list(vec![Playable { track, url: None }], 0);
        let mut pos = 0.0;
        for _ in 0..80 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            pos = app.clock().position_s;
            if pos > 1.0 {
                break;
            }
            if let Some(e) = app.now_playing().borrow().error.clone() {
                panic!("{e}");
            }
        }
        eprintln!("youtube pos={pos:.1}");
        assert!(pos > 1.0);
        app.seek(150.0);
        std::thread::sleep(std::time::Duration::from_millis(4000));
        let p = app.clock().position_s;
        eprintln!("after far seek pos={p:.1}");
        assert!(p > 150.0, "seek past the first MiB failed");
    }
}

#[cfg(test)]
mod live_search {
    use super::*;

    /// Search in the main account's catalog returns tracks. DRX_TEST_PROFILE; not run in CI.
    #[test]
    fn searches_main_catalog() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        let h = app.open_list(ListSpec::Search {
            query: "Sewerslvt goodbye".into(),
        });
        let n = app.runtime().block_on(app.load_remote(h)).unwrap();
        eprintln!("search results={n}");
        assert!(n > 0);
    }
}

#[cfg(test)]
mod live_pages {
    use super::*;

    /// For each linked catalog: the first liked track's artist and album pages load from the network with a
    /// heading. Prints only counts. DRX_TEST_PROFILE; not run in CI.
    #[test]
    fn catalog_artist_and_album_pages() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        let rt = app.runtime();
        for liked in [
            ListSpec::SpotifyLiked,
            ListSpec::YandexLiked,
            ListSpec::VkMyMusic,
        ] {
            let h = app.open_list(liked.clone());
            if rt.block_on(app.load_remote(h)).is_err() {
                eprintln!("{liked:?}: no account");
                continue;
            }
            let rows = rt.block_on(app.list_rows(h, 0..20)).unwrap();
            let Some(row) = rows.iter().find(|r| r.artist_id.is_some()) else {
                eprintln!("{liked:?}: no artist ids");
                continue;
            };
            let artist = rt.block_on(app.artist_page(row.artist_id.as_deref().unwrap()));
            let ha = app.open_list(artist.clone());
            let n = rt.block_on(app.load_remote(ha));
            let head = app.list_header(ha);
            eprintln!(
                "{liked:?}: artist page {} rows={n:?} header={} image={} releases={}",
                matches!(artist, ListSpec::CatalogArtist { .. }),
                head.as_ref().is_some_and(|h| !h.title.is_empty()),
                head.as_ref().is_some_and(|h| h.image.is_some()),
                head.as_ref().map_or(0, |h| h.releases.len())
            );
            assert!(n.unwrap_or(0) > 0);
            if let Some(album) = rows.iter().find_map(|r| r.album_id.clone()) {
                let spec = rt.block_on(app.album_page(&album));
                let hb = app.open_list(spec);
                let n = rt.block_on(app.load_remote(hb));
                let head = app.list_header(hb);
                eprintln!(
                    "{liked:?}: album rows={n:?} header={}",
                    head.as_ref().is_some_and(|h| !h.title.is_empty())
                );
                assert!(n.unwrap_or(0) > 0);
            }
        }
    }
}

#[cfg(test)]
mod live_home {
    use super::*;

    /// Builds the home page from the real profile. Prints row kinds and sizes only. DRX_TEST_PROFILE; not in CI.
    #[test]
    fn builds_home() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        let t = std::time::Instant::now();
        let feed = app.runtime().block_on(app.home(true)).unwrap();
        for r in &feed.rows {
            // Genre rows say their genre (not personal); other titles only their first word.
            let kind = if r.title.starts_with("Похожее в ") {
                r.title.clone()
            } else {
                format!("{}…", r.title.split(' ').next().unwrap_or(""))
            };
            eprintln!(
                "row {kind}: tracks={} albums={}",
                r.tracks.len(),
                r.albums.len()
            );
        }
        eprintln!("built in {:.1}s", t.elapsed().as_secs_f32());
        assert!(feed.rows.len() >= 3);
        assert!(app.home_cached().is_some());
    }
}

/// SQL column: the artwork of a playlist's first track that has one (by position).
pub(crate) const PLAYLIST_FIRST_COVER: &str = "(SELECT s.thumbnailUrl FROM playlist_song_map m JOIN song s ON s.id = m.songId      WHERE m.playlistId = p.id AND s.thumbnailUrl IS NOT NULL AND s.thumbnailUrl != '' ORDER BY m.position LIMIT 1)";

/// A playlist's cover as Android 2.3.0 shows it (`Playlist.thumbnails`): its own cover, else its first track's
/// artwork. Own covers chosen on the phone are `content://<app>.FileProvider/covers/<file>`; the backup brings the
/// file into the profile's `covers/`, so that file is shown.
pub(crate) fn playlist_cover(
    covers: &std::path::Path,
    own: Option<String>,
    first_track: Option<String>,
) -> Option<String> {
    let own = own.filter(|u| !u.is_empty()).and_then(|u| {
        if u.starts_with("http") {
            return Some(u);
        }
        let name = u.rsplit('/').next()?;
        let path = covers.join(name);
        path.is_file()
            .then(|| format!("{}{}", drx_core::images::LOCAL_PREFIX, path.display()))
    });
    own.or(first_track.filter(|u| !u.is_empty()))
}

#[cfg(test)]
mod live_search_page {
    use super::*;

    /// Search page in every source, a pasted link, suggestions and history. Counts only. DRX_TEST_PROFILE.
    #[test]
    fn search_everywhere() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        let rt = app.runtime();
        for (src, label) in app.search_sources() {
            let r = rt.block_on(app.search_page("Sleep Token", src)).unwrap();
            eprintln!(
                "{label}: tracks={} artists={} albums={} playlists={} playable={} library={}",
                r.results.tracks.len(),
                r.results.artists.len(),
                r.results.albums.len(),
                r.results.playlists.len(),
                r.playable.len(),
                r.library
            );
        }
        let link = rt
            .block_on(app.search_page(
                "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT",
                SearchSource::Main,
            ))
            .unwrap();
        eprintln!(
            "spotify track link: tracks={} album_known={}",
            link.results.tracks.len(),
            link.results
                .tracks
                .first()
                .and_then(|t| t.album.as_ref())
                .is_some_and(|a| !a.id.is_empty())
        );
        assert!(link.from_link && link.results.tracks.len() == 1);
        let yt = rt
            .block_on(app.search_page("https://youtu.be/dQw4w9WgXcQ", SearchSource::Main))
            .unwrap();
        assert_eq!(yt.results.tracks.len(), 1);
        let (hist, online) = rt.block_on(app.search_suggestions("sleep tok"));
        eprintln!(
            "suggestions: history={} online={}",
            hist.len(),
            online.len()
        );
        assert!(!online.is_empty());
        let h = rt.block_on(app.search_history("", 5));
        assert_eq!(
            h.first().map(String::as_str),
            Some("https://youtu.be/dQw4w9WgXcQ")
        );
        rt.block_on(app.forget_search("https://youtu.be/dQw4w9WgXcQ"))
            .unwrap();
        rt.block_on(app.forget_search("https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT"))
            .unwrap();
    }
}

#[cfg(test)]
mod playlist_cover_tests {
    use super::playlist_cover;

    #[test]
    fn own_cover_then_first_track() {
        let dir = std::env::temp_dir().join(format!("drx-covers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("playlist_cover_crop_1.jpg"), b"x").unwrap();
        let own = "content://com.drxwnify.app.FileProvider/covers/playlist_cover_crop_1.jpg";
        let got = playlist_cover(&dir, Some(own.into()), Some("https://t/1".into())).unwrap();
        assert!(
            got.starts_with(drx_core::images::LOCAL_PREFIX)
                && got.ends_with("playlist_cover_crop_1.jpg")
        );
        // The phone's file did not come with the backup: the first track's artwork.
        let missing = "content://com.drxwnify.app.FileProvider/covers/gone.jpg";
        assert_eq!(
            playlist_cover(&dir, Some(missing.into()), Some("https://t/1".into())).as_deref(),
            Some("https://t/1")
        );
        assert_eq!(
            playlist_cover(&dir, Some("https://own".into()), None).as_deref(),
            Some("https://own")
        );
        assert_eq!(playlist_cover(&dir, None, None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod live_like_report {
    use super::*;

    /// Compares the app's liked songs with the Spotify account's Liked Songs (numbers only).
    /// DRX_TEST_PROFILE = an imported profile folder. Not run in CI.
    #[test]
    fn liked_vs_spotify() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        let rt = app.runtime();
        let sp = app.spotify().expect("spotify account");
        let mut account = std::collections::HashSet::new();
        let mut offset = 0;
        loop {
            let page = rt.block_on(sp.liked_songs(offset, 50)).unwrap();
            let n = page.items.len() as u32;
            account.extend(page.items.into_iter().map(|t| t.id));
            offset += 50;
            if n == 0 || offset >= page.total {
                break;
            }
        }
        let rows: Vec<(String, Option<String>)> = rt
            .block_on(app.store().call(|c| {
                let mut st = c.prepare(
                    "SELECT s.id, (SELECT m.spotifyId FROM spotify_match m WHERE m.youtubeId = s.id ORDER BY m.isManualOverride DESC, m.matchScore DESC LIMIT 1) FROM song s WHERE s.liked = 1",
                )?;
                let r = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(r)
            }))
            .unwrap();
        let (mut by_catalog, mut yt_only, mut sources) = (Vec::new(), 0, 0);
        for (id, matched) in &rows {
            let sp_id = id
                .strip_prefix("mfb:")
                .or_else(|| id.strip_prefix("spotify:"))
                .filter(|s| s.len() == 22 && s.chars().all(|c| c.is_ascii_alphanumeric()))
                .map(str::to_string)
                .or_else(|| matched.clone());
            match sp_id {
                Some(s) => by_catalog.push(s),
                None if id.starts_with("mfb:") => sources += 1,
                None => yt_only += 1,
            }
        }
        let distinct: std::collections::HashSet<_> = by_catalog.iter().cloned().collect();
        let both = distinct.intersection(&account).count();
        eprintln!(
            "app liked rows={} | spotify-backed rows={} distinct tracks={} (duplicate rows={}) | youtube without spotify={} | other sources={}",
            rows.len(),
            by_catalog.len(),
            distinct.len(),
            by_catalog.len() - distinct.len(),
            yt_only,
            sources
        );
        eprintln!(
            "spotify account liked={} | in both={} | liked in app only={} | liked on spotify only={}",
            account.len(),
            both,
            distinct.len() - both,
            account.len() - both
        );
    }
}

#[cfg(test)]
mod live_likes {
    use super::*;

    /// Like and unlike one track on Spotify and Yandex (net nothing), checking the accounts' first liked page.
    /// DRX_TEST_PROFILE = a copy of an imported profile; DRX_LIVE_WRITES=1 to allow account writes. Not run in CI.
    #[test]
    fn like_round_trip_on_accounts() {
        let (Ok(dir), Ok(_)) = (
            std::env::var("DRX_TEST_PROFILE"),
            std::env::var("DRX_LIVE_WRITES"),
        ) else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        let rt = app.runtime();
        if let Some(sp) = app.spotify() {
            let liked: std::collections::HashSet<String> =
                rt.block_on(sp.liked_songs(0, 50)).unwrap().items.into_iter().map(|t| t.id).collect();
            let found = rt.block_on(sp.search_tracks("Iglooghost Clear Tamei", 10)).unwrap();
            let t = found.into_iter().find(|t| !liked.contains(&t.id)).expect("a track not liked");
            let uri = vec![format!("spotify:track:{}", t.id)];
            rt.block_on(sp.set_in_library(&uri, true)).unwrap();
            let after: Vec<String> = rt.block_on(sp.liked_songs(0, 5)).unwrap().items.into_iter().map(|t| t.id).collect();
            rt.block_on(sp.set_in_library(&uri, false)).unwrap();
            let gone: Vec<String> = rt.block_on(sp.liked_songs(0, 5)).unwrap().items.into_iter().map(|t| t.id).collect();
            eprintln!("spotify: liked first={} after unlike present={}", after.first() == Some(&t.id), gone.contains(&t.id));
            assert_eq!(after.first(), Some(&t.id));
            assert!(!gone.contains(&t.id));
        }
        if let Some(y) = app.yandex() {
            let found = rt.block_on(y.search("Iglooghost Clear Tamei", 10)).unwrap();
            let liked: std::collections::HashSet<String> =
                rt.block_on(y.liked_songs(0, 50)).unwrap().items.into_iter().map(|t| t.id).collect();
            let t = found.into_iter().find(|t| !liked.contains(&t.id)).expect("a track not liked");
            rt.block_on(y.set_liked("track", &t.id, true)).unwrap();
            let after: std::collections::HashSet<String> =
                rt.block_on(y.liked_songs(0, 50)).unwrap().items.into_iter().map(|t| t.id).collect();
            rt.block_on(y.set_liked("track", &t.id, false)).unwrap();
            let gone: std::collections::HashSet<String> =
                rt.block_on(y.liked_songs(0, 50)).unwrap().items.into_iter().map(|t| t.id).collect();
            eprintln!("yandex: liked={} after unlike present={}", after.contains(&t.id), gone.contains(&t.id));
            assert!(after.contains(&t.id));
            assert!(!gone.contains(&t.id));
        }
    }
}

#[cfg(test)]
mod live_liked_head {
    use super::*;

    /// Prints the first three tracks of the Spotify account's Liked Songs (DRX_TEST_PROFILE). Not run in CI.
    #[test]
    fn spotify_liked_head() {
        let Ok(dir) = std::env::var("DRX_TEST_PROFILE") else {
            return;
        };
        let app = App::start(Profile::at(dir), EditionProfile::mini()).unwrap();
        let sp = app.spotify().expect("spotify");
        let page = app.runtime().block_on(sp.liked_songs(0, 3)).unwrap();
        for t in page.items {
            eprintln!("liked head: {} {}", t.id, t.title);
        }
    }
}
