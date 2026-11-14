//! Facade for both UIs: commands, queries, list handles, state snapshots and the edition profile.
//! The UIs depend on this crate only (see docs/ARCHITECTURE.md). No business logic lives in the UIs.

mod accounts;
pub mod edition;
pub mod lists;
pub mod playback;

pub use drx_audio::player::PlaybackClock;
use drx_core::http::Http;
use drx_core::paths::Profile;
use drx_core::store::Store;
use drx_transfer::backup::{self, Accounts, ImportOptions};
pub use edition::{Edition, EditionProfile};
pub use lists::{ListHandle, ListHeader, ListSpec, SongRow};
pub use playback::{NowPlaying, Playable};
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryKind {
    Liked,
    Playlist,
    Album,
    Artist,
    /// A list of a linked account (liked, playlist, album), loaded from its catalog. `id` is the list's entry id.
    Remote,
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
    clients: accounts::Clients,
    images: drx_core::images::Images,
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
            clients: accounts::Clients::default(),
            images,
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

    async fn db_library_entries(&self) -> AppResult<Vec<LibraryEntry>> {
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

    /// Heading of a database album or artist page (catalog pages get theirs from `list_header` after loading).
    pub async fn local_header(&self, spec: &ListSpec) -> Option<ListHeader> {
        let spec = spec.clone();
        self.store()
            .call(move |c| {
                Ok(match &spec {
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
            let remote = self.inner.remote.lock().expect("remote");
            let rows = remote.get(&handle).map(Vec::as_slice).unwrap_or(&[]);
            let end = range.end.min(rows.len());
            let start = range.start.min(end);
            return Ok(rows[start..end].iter().map(row_of).collect());
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
        self.inner.playback.play_list(items, index);
        Ok(())
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
    pub fn now_playing(&self) -> watch::Receiver<NowPlaying> {
        self.inner.playback.now.subscribe()
    }
    pub fn clock(&self) -> PlaybackClock {
        self.inner.playback.clock()
    }
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
