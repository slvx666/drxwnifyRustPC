//! Facade for both UIs: commands, queries, list handles, state snapshots and the edition profile.
//! The UIs depend on this crate only (see docs/ARCHITECTURE.md). No business logic lives in the UIs.

pub mod edition;
pub mod lists;

use drx_core::http::Http;
use drx_core::paths::Profile;
use drx_core::store::Store;
use drx_transfer::backup::{self, Accounts, ImportOptions};
pub use edition::{Edition, EditionProfile};
pub use lists::{ListHandle, ListSpec, SongRow};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryKind {
    Liked,
    Playlist,
    Album,
    Artist,
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
}

struct Inner {
    profile: Profile,
    edition: EditionProfile,
    store: RwLock<Store>,
    http: Http,
    state_tx: watch::Sender<AppState>,
    events: broadcast::Sender<AppEvent>,
    lists: lists::Lists,
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
        let inner = Arc::new(Inner {
            http: Http::new()?,
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
                    liked: n("SELECT count(*) FROM song WHERE liked = 1")?,
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

    /// Rows of the library panel: liked songs, local playlists, saved albums and artists (newest first).
    pub async fn library_entries(&self) -> AppResult<Vec<LibraryEntry>> {
        Ok(self
            .store()
            .call(|c| {
                let liked: i64 = c.query_row("SELECT count(*) FROM song WHERE liked = 1", [], |r| r.get(0))?;
                let mut out = vec![LibraryEntry {
                    kind: LibraryKind::Liked,
                    id: "liked".into(),
                    title: "Любимые треки".into(),
                    subtitle: format!("{liked} треков"),
                    thumbnail: None,
                    pinned: true,
                }];
                let mut st = c.prepare(
                    "SELECT p.id, p.name, p.thumbnailUrl, (SELECT count(*) FROM playlist_song_map m WHERE m.playlistId = p.id)
                     FROM playlist p ORDER BY coalesce(p.lastUpdateTime, p.createdAt) DESC",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(LibraryEntry {
                        kind: LibraryKind::Playlist,
                        id: r.get(0)?,
                        title: r.get(1)?,
                        subtitle: format!("Плейлист · {}", r.get::<_, i64>(3)?),
                        thumbnail: r.get(2)?,
                        pinned: false,
                    })
                })?;
                for r in rows {
                    out.push(r?);
                }
                let mut st = c.prepare(
                    "SELECT a.id, a.title, a.thumbnailUrl, (SELECT group_concat(ar.name, ', ') FROM album_artist_map m JOIN artist ar ON ar.id = m.artistId WHERE m.albumId = a.id)
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
                    })
                })?;
                for r in rows {
                    out.push(r?);
                }
                let mut st = c.prepare(
                    "SELECT id, name, thumbnailUrl FROM artist WHERE bookmarkedAt IS NOT NULL ORDER BY bookmarkedAt DESC",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(LibraryEntry {
                        kind: LibraryKind::Artist,
                        id: r.get(0)?,
                        title: r.get(1)?,
                        subtitle: "Исполнитель".into(),
                        thumbnail: r.get(2)?,
                        pinned: false,
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
    }

    /// Number of rows in a list.
    pub async fn list_len(&self, handle: ListHandle) -> AppResult<usize> {
        let spec = self
            .inner
            .lists
            .spec(handle)
            .ok_or_else(|| AppError::Other("list is closed".into()))?;
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
        Ok(self
            .store()
            .call(move |c| lists::rows(c, &spec, range))
            .await?)
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
    }
}
