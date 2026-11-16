//! Playback in the core: queue, turning a catalog track into a byte source, the player, now-playing state.
//! The UIs only send commands (play list from index, toggle, next, previous, seek, volume) and render
//! [`NowPlaying`] plus the player clock.

use drx_audio::hls::{Fetcher, HlsOpener, parse_media_playlist};
use drx_audio::player::{PlaybackClock, Player, PlayerEvent};
use drx_audio::source::{CachedSource, RangeOpener};
use drx_catalog::model::Track;
use drx_core::http::{Http, Method};
use drx_resolver::provider::{
    AudioProvider, AudioQuery, AudioStream, DEFAULT_ORDER, ProviderId, ProviderMatch,
};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::watch;

/// A track the player can start: catalog metadata plus where its audio is (when already known).
#[derive(Debug, Clone, PartialEq)]
pub struct Playable {
    pub track: Track,
    /// Direct stream URL (VK gives one with the track). None: the resolver has to find audio (milestone 5).
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NowPlaying {
    pub track: Option<Track>,
    pub index: Option<usize>,
    pub playing: bool,
    pub error: Option<String>,
    /// The next tracks of the queue (up to 5), for «Далее».
    pub upcoming: Vec<Track>,
    /// Lyrics of the current track, when found (LRCLIB).
    pub lyrics: Option<Arc<drx_lyrics::Lyrics>>,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// Moves on every change of the queue (the queue panel redraws on it).
    pub queue_rev: u64,
}

/// Repeat mode (Media3 REPEAT_MODE_*: the numbers are the phone's `repeatMode` setting).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Repeat {
    #[default]
    Off = 0,
    One = 1,
    All = 2,
}

impl Repeat {
    pub fn from_i32(v: i32) -> Repeat {
        match v {
            1 => Repeat::One,
            2 => Repeat::All,
            _ => Repeat::Off,
        }
    }
    /// The next mode of the repeat button: off → all → one → off (as Android's player).
    pub fn next(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

/// Shuffles in place (xorshift seeded by the clock: no random crate needed for a queue).
pub(crate) fn shuffle_in_place<T>(v: &mut [T]) {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x2545_F491_4F6C_DD1D)
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

/// Blocking body reader for the download thread (it is not a runtime thread, so `block_on` is fine).
struct BlockingBody {
    handle: Handle,
    resp: reqwest::Response,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for BlockingBody {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.buf.len() {
            match self.handle.block_on(self.resp.chunk()) {
                Ok(Some(b)) => {
                    self.buf = b.to_vec();
                    self.pos = 0;
                }
                Ok(None) => return Ok(0),
                Err(e) => return Err(std::io::Error::other(e)),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Progressive HTTP with Range requests (MP3/M4A/WebM).
struct HttpOpener {
    http: Http,
    handle: Handle,
    url: String,
    user_agent: Option<&'static str>,
}

impl RangeOpener for HttpOpener {
    fn open(&self, offset: u64) -> std::io::Result<(Option<u64>, Box<dyn Read + Send>)> {
        let mut req = self
            .http
            .client()
            .request(Method::GET, &self.url)
            .timeout(Duration::from_secs(60 * 30));
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-"));
        }
        if let Some(ua) = self.user_agent {
            req = req.header("User-Agent", ua);
        }
        // send() must run inside the runtime: reqwest creates its timeout timer when the request is made.
        let resp = self
            .handle
            .block_on(async move { req.send().await })
            .map_err(std::io::Error::other)?;
        if !resp.status().is_success() {
            return Err(std::io::Error::other(format!("HTTP {}", resp.status())));
        }
        let total = if offset == 0 {
            resp.content_length()
        } else {
            resp.headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|t| t.parse().ok())
        };
        Ok((
            total,
            Box::new(BlockingBody {
                handle: self.handle.clone(),
                resp,
                buf: Vec::new(),
                pos: 0,
            }),
        ))
    }
}

struct HttpFetcher {
    http: Http,
    handle: Handle,
    user_agent: Option<&'static str>,
}

impl Fetcher for HttpFetcher {
    fn get(&self, url: &str) -> std::io::Result<Vec<u8>> {
        let mut req = self.http.client().request(Method::GET, url);
        if let Some(ua) = self.user_agent {
            req = req.header("User-Agent", ua);
        }
        self.handle.block_on(async {
            let r = req.send().await.map_err(std::io::Error::other)?;
            if !r.status().is_success() {
                return Err(std::io::Error::other(format!("HTTP {}", r.status())));
            }
            r.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(std::io::Error::other)
        })
    }
}

pub(crate) fn cache_name(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub(crate) struct Playback {
    http: Http,
    handle: Handle,
    cache_dir: PathBuf,
    player: Mutex<Option<Arc<Player>>>,
    queue: Mutex<(Vec<Playable>, usize)>,
    /// The queue in its own order while shuffle is on (to go back to it).
    original: Mutex<Option<Vec<Playable>>>,
    current_id: Mutex<Option<drx_audio::player::TrackId>>,
    /// A seek on a track not started yet (restored after launch): (its id, where), used when it starts.
    start_at: Mutex<Option<(String, f64)>>,
    pub now: Arc<watch::Sender<NowPlaying>>,
    volume: Mutex<f32>,
    /// A track that cannot start: go on to the next one (Android autoSkipNextOnError) instead of stopping.
    pub skip_on_error: std::sync::atomic::AtomicBool,
    /// Tracks skipped in a row for errors (a run of them stops: the network is likely down).
    skipped: std::sync::atomic::AtomicU32,
    /// Downloaded tracks: track id → (file, format hint). Played from disk, no network.
    pub downloaded: Mutex<std::collections::HashMap<String, (PathBuf, Option<String>)>>,
    /// The sleep timer's fade (0..1) on top of the volume.
    fade: Mutex<f32>,
    /// Audio providers for tracks without a URL (the resolver asks them all at once).
    pub providers: Mutex<Vec<Arc<dyn AudioProvider>>>,
    /// The music database: stored matches (`audio_fallback_match`, shared with Android) are tried before a search.
    pub store: Mutex<Option<drx_core::store::Store>>,
    /// Streams found this session, by catalog track id (provider match kept for a fresh URL later).
    resolved: Mutex<std::collections::HashMap<String, (ProviderMatch, AudioQuery)>>,
}

impl Playback {
    pub fn new(http: Http, handle: Handle, cache_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            http,
            handle,
            cache_dir,
            player: Mutex::new(None),
            queue: Mutex::new((Vec::new(), 0)),
            original: Mutex::new(None),
            current_id: Mutex::new(None),
            start_at: Mutex::new(None),
            now: Arc::new(watch::channel(NowPlaying::default()).0),
            volume: Mutex::new(0.8),
            fade: Mutex::new(1.0),
            downloaded: Mutex::new(std::collections::HashMap::new()),
            skip_on_error: std::sync::atomic::AtomicBool::new(false),
            skipped: std::sync::atomic::AtomicU32::new(0),
            providers: Mutex::new(Vec::new()),
            store: Mutex::new(None),
            resolved: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// The audio device opens on first use, so an idle app does not hold it.
    fn player(self: &Arc<Self>) -> Result<Arc<Player>, String> {
        let mut p = self.player.lock().expect("player lock");
        if let Some(x) = p.as_ref() {
            return Ok(x.clone());
        }
        let player = Player::new()?;
        player.set_volume(*self.volume.lock().expect("volume") * *self.fade.lock().expect("fade"));
        let me = Arc::downgrade(self);
        let pl = player.clone();
        std::thread::Builder::new()
            .name("playback-events".into())
            .spawn(move || {
                loop {
                    let Some(ev) = pl.next_event(Duration::from_millis(500)) else {
                        if me.strong_count() == 0 {
                            return;
                        }
                        continue;
                    };
                    let Some(this) = me.upgrade() else { return };
                    if let PlayerEvent::Ended(id) = ev {
                        let cur = *this.current_id.lock().expect("current");
                        if cur == Some(id) {
                            // Track finished on its own: repeat one, or continue (wrapping with repeat all).
                            let (i, len) = {
                                let q = this.queue.lock().expect("queue");
                                (q.1, q.0.len())
                            };
                            let next = match this.now.borrow().repeat {
                                Repeat::One => i,
                                Repeat::All if i + 1 >= len => 0,
                                _ => i + 1,
                            };
                            this.start_index(next);
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        *p = Some(player.clone());
        Ok(player)
    }

    /// Finds audio for a catalog track: every ready provider at once (ParallelAudioResolver, AUTO), then a fresh
    /// stream from the best match that can still give one.
    fn resolve(&self, track: &drx_catalog::model::Track) -> Result<(AudioStream, f32), String> {
        let providers = self.providers.lock().expect("providers").clone();
        let by_id = |id: ProviderId| providers.iter().find(|p| p.id() == id).cloned();
        // Library rows are `mfb:<catalog id>` (Android FallbackIds); the matches table is keyed by the catalog id.
        let catalog_id = track
            .id
            .strip_prefix("mfb:")
            .unwrap_or(&track.id)
            .to_string();
        let mut q = AudioQuery::from_track(track);
        q.catalog_id = Some(catalog_id.clone());

        // 1. A VK track in the library is itself the audio: no search.
        if let Some(full) = catalog_id.strip_prefix("src:VK:")
            && let Some(p) = by_id(ProviderId::Vk)
        {
            let m = ProviderMatch {
                provider: ProviderId::Vk,
                track_id: full.to_string(),
                title: track.title.clone(),
                artist: track.artist_names(),
                duration_ms: Some(track.duration_ms),
                confidence: 1.0,
                quality_kbps: None,
            };
            if let Ok(Some(s)) = self.handle.block_on(p.stream(&q, &m)) {
                return Ok((s, 1.0));
            }
        }
        // 1b. A YouTube song in the library (its id is the video id, as on Android): that video is the audio.
        if is_youtube_video_id(&catalog_id)
            && let Some(p) = by_id(ProviderId::Youtube)
        {
            let m = ProviderMatch {
                provider: ProviderId::Youtube,
                track_id: catalog_id.clone(),
                title: track.title.clone(),
                artist: track.artist_names(),
                duration_ms: Some(track.duration_ms),
                confidence: 1.0,
                quality_kbps: None,
            };
            if let Ok(Some(s)) = self.handle.block_on(p.stream(&q, &m)) {
                return Ok((
                    s,
                    drx_audio::dsp::normalization_gain(p.loudness_db(&m.track_id)),
                ));
            }
        }
        // 2. This session's choice, then the stored matches (the phone's 44k of them came with the backup).
        let mut known: Vec<ProviderMatch> = Vec::new();
        if let Some((m, _)) = self
            .resolved
            .lock()
            .expect("resolved")
            .get(&catalog_id)
            .cloned()
        {
            known.push(m);
        }
        known.extend(self.stored_matches(&catalog_id));
        for m in &known {
            let Some(p) = by_id(m.provider) else { continue };
            if let Ok(Some(s)) = self.handle.block_on(p.stream(&q, m)) {
                self.resolved
                    .lock()
                    .expect("resolved")
                    .insert(catalog_id.clone(), (m.clone(), q.clone()));
                return Ok((
                    s,
                    drx_audio::dsp::normalization_gain(p.loudness_db(&m.track_id)),
                ));
            }
        }
        // 3. Search every ready source at once.
        let out = self.handle.block_on(drx_resolver::parallel::resolve(
            &q,
            &providers,
            &DEFAULT_ORDER,
        ));
        tracing::info!(track = %track.title, report = ?out.report, "audio search");
        for m in &out.matches {
            let Some(p) = by_id(m.provider) else { continue };
            if let Ok(Some(s)) = self.handle.block_on(p.stream(&q, m)) {
                self.resolved
                    .lock()
                    .expect("resolved")
                    .insert(catalog_id.clone(), (m.clone(), q.clone()));
                self.store_matches(&q, &out.matches, m);
                return Ok((
                    s,
                    drx_audio::dsp::normalization_gain(p.loudness_db(&m.track_id)),
                ));
            }
        }
        Err(if providers.iter().any(|p| p.is_ready()) {
            "звук не найден ни в одном источнике".to_string()
        } else {
            "нет источников звука".to_string()
        })
    }

    /// Stored matches for a catalog track, the last winner first, then by confidence.
    fn stored_matches(&self, catalog_id: &str) -> Vec<ProviderMatch> {
        let Some(store) = self.store.lock().expect("store").clone() else {
            return Vec::new();
        };
        let id = catalog_id.to_string();
        store
            .call_blocking(move |c| {
                let mut st = c.prepare_cached(
                    "SELECT provider, providerTrackId, matchedTitle, matchedArtist, confidence, durationMs FROM audio_fallback_match WHERE catalogId = ?1 ORDER BY selected DESC, confidence DESC",
                )?;
                let rows = st.query_map([id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, f64>(4)?, r.get::<_, i64>(5)?))
                })?;
                Ok(rows
                    .filter_map(Result::ok)
                    .filter_map(|(p, t, mt, ma, conf, dur)| {
                        Some(ProviderMatch { provider: ProviderId::from_name(&p)?, track_id: t, title: mt, artist: ma, duration_ms: Some(dur.max(0) as u64), confidence: conf, quality_kbps: None })
                    })
                    .collect())
            })
            .unwrap_or_default()
    }

    /// Saves what a search found, so the next play (here or, after a backup, on the phone) needs no search.
    fn store_matches(&self, q: &AudioQuery, matches: &[ProviderMatch], winner: &ProviderMatch) {
        let Some(store) = self.store.lock().expect("store").clone() else {
            return;
        };
        let (q, matches, winner) = (q.clone(), matches.to_vec(), winner.clone());
        let _ = store.call_blocking(move |c| {
            let Some(id) = q.catalog_id.clone() else { return Ok(()) };
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
            let tx = c.transaction()?;
            tx.execute("UPDATE audio_fallback_match SET selected = 0 WHERE catalogId = ?1", [&id])?;
            for m in &matches {
                tx.execute(
                    "INSERT OR REPLACE INTO audio_fallback_match (catalogId, provider, providerTrackId, matchedTitle, matchedArtist, confidence, title, artists, album, durationMs, isrc, selected, matchedAt) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    rusqlite::params![
                        id, m.provider.name(), m.track_id, m.title, m.artist, m.confidence, q.title, q.artists.join(", "), q.album,
                        q.duration_ms as i64, q.isrc, (m == &winner) as i64, now
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        });
    }

    /// The audio of a track: its downloaded file when there is one, else the stream (cached as it plays).
    fn source_for(
        &self,
        p: &Playable,
    ) -> Result<(Box<dyn drx_audio::decode::ByteSource>, Option<&'static str>, f32), String> {
        let local = self.downloaded.lock().expect("downloaded").get(&p.track.id).cloned();
        if let Some((path, ext)) = local
            && let Ok(f) = std::fs::File::open(&path)
        {
            return Ok((Box::new(f), static_ext(ext.as_deref()), 1.0));
        }
        let file = self.cache_dir.join("audio").join(cache_name(&p.track.id));
        let (src, ext, gain) = self.stream_into(p, &file)?;
        Ok((Box::new(src), ext, gain))
    }

    /// Opens a track's stream, written into `file` as it comes (playback cache, or a download's file).
    pub(crate) fn stream_into(
        &self,
        p: &Playable,
        file: &std::path::Path,
    ) -> Result<(CachedSource, Option<&'static str>, f32), String> {
        let file = file.to_path_buf();
        let (url, ua, gain) = match p.url.clone() {
            Some(u) => (
                u,
                p.track
                    .id
                    .starts_with("src:VK:")
                    .then_some(drx_catalog::vk::CLIENT_USER_AGENT),
                1.0,
            ),
            None => {
                let (s, g) = self.resolve(&p.track)?;
                (s.url, s.user_agent, g)
            }
        };
        if url.contains(".m3u8") {
            // VK: try the MP3 next to the HLS playlist first (as Android), else play the HLS itself.
            let mp3 = drx_catalog::vk::to_mp3_url(&url);
            if mp3 != url {
                let opener = Arc::new(HttpOpener {
                    http: self.http.clone(),
                    handle: self.handle.clone(),
                    url: mp3,
                    user_agent: ua,
                });
                if let Ok(src) = CachedSource::start(opener, &file) {
                    return Ok((src, Some("mp3"), gain));
                }
            }
            let fetcher = Arc::new(HttpFetcher {
                http: self.http.clone(),
                handle: self.handle.clone(),
                user_agent: ua,
            });
            let text =
                String::from_utf8_lossy(&fetcher.get(&url).map_err(|e| e.to_string())?).to_string();
            let playlist = parse_media_playlist(&text, &url)?;
            let opener = Arc::new(HlsOpener { fetcher, playlist });
            let src = CachedSource::start(opener, &file).map_err(|e| e.to_string())?;
            return Ok((src, None, gain));
        }
        let opener = Arc::new(HttpOpener {
            http: self.http.clone(),
            handle: self.handle.clone(),
            url,
            user_agent: ua,
        });
        Ok((
            CachedSource::start(opener, &file).map_err(|e| e.to_string())?,
            None,
            gain,
        ))
    }

    /// Plays `list` from `index` (the queue becomes this list).
    /// Plays `list` from `index`. With shuffle on, that track plays first and the rest follow in random order.
    pub fn play_list(self: &Arc<Self>, list: Vec<Playable>, index: usize) {
        if self.now.borrow().shuffle && index < list.len() {
            let mut rest = list.clone();
            let first = rest.remove(index);
            shuffle_in_place(&mut rest);
            rest.insert(0, first);
            *self.original.lock().expect("original") = Some(list);
            *self.queue.lock().expect("queue") = (rest, 0);
            self.start_index(0);
        } else {
            *self.original.lock().expect("original") = None;
            *self.queue.lock().expect("queue") = (list, index);
            self.start_index(index);
        }
    }

    /// «Перемешать» on a page: shuffle on and the list from a random track.
    pub fn play_shuffled(self: &Arc<Self>, list: Vec<Playable>) {
        if list.is_empty() {
            return;
        }
        self.now.send_modify(|n| n.shuffle = true);
        let mut order: Vec<usize> = (0..list.len()).collect();
        shuffle_in_place(&mut order);
        self.play_list(list, order[0]);
    }

    /// Shuffle on: the current track stays, everything else is mixed. Off: the list's own order comes back, at
    /// the current track.
    pub fn set_shuffle(self: &Arc<Self>, on: bool) {
        if self.now.borrow().shuffle == on {
            return;
        }
        {
            let mut q = self.queue.lock().expect("queue");
            let mut orig = self.original.lock().expect("original");
            if on {
                if !q.0.is_empty() {
                    let mut rest = q.0.clone();
                    let cur = rest.remove(q.1.min(rest.len() - 1));
                    shuffle_in_place(&mut rest);
                    rest.insert(0, cur);
                    *orig = Some(std::mem::replace(&mut q.0, rest));
                    q.1 = 0;
                }
            } else if let Some(list) = orig.take() {
                let cur_id = q.0.get(q.1).map(|p| p.track.id.clone());
                let at = cur_id
                    .and_then(|id| list.iter().position(|p| p.track.id == id))
                    .unwrap_or(0);
                *q = (list, at);
            }
        }
        let upcoming = self.upcoming();
        self.now.send_modify(|n| {
            n.shuffle = on;
            n.upcoming = upcoming;
            n.queue_rev += 1;
            n.index = Some(self.queue.lock().expect("queue").1);
        });
    }

    pub fn set_repeat(&self, r: Repeat) {
        self.now.send_modify(|n| n.repeat = r);
    }

    /// Puts a track right after the playing one («Играть дальше»), or at the end of the queue.
    pub fn enqueue(&self, item: Playable, next: bool) {
        {
            let mut q = self.queue.lock().expect("queue");
            if q.0.is_empty() {
                q.0.push(item.clone());
            } else if next {
                let at = (q.1 + 1).min(q.0.len());
                q.0.insert(at, item.clone());
            } else {
                q.0.push(item.clone());
            }
            if let Some(o) = self.original.lock().expect("original").as_mut() {
                o.push(item);
            }
        }
        let up = self.upcoming();
        self.now.send_modify(|n| {
            n.upcoming = up;
            n.queue_rev += 1;
        });
    }

    /// Replaces what is queued after the track `current_id` (radio after the current track). If that track
    /// ended meanwhile, the new tracks follow the one playing by then.
    pub fn replace_upcoming(&self, current_id: &str, items: Vec<Playable>) {
        {
            let mut q = self.queue.lock().expect("queue");
            let at = q.0.iter().position(|p| p.track.id == current_id).filter(|i| *i <= q.1).unwrap_or(q.1);
            let keep = (at.max(q.1) + 1).min(q.0.len());
            q.0.truncate(keep);
            q.0.extend(items);
        }
        *self.original.lock().expect("original") = None;
        let up = self.upcoming();
        self.now.send_modify(|n| {
            n.upcoming = up;
            n.queue_rev += 1;
        });
    }

    /// The queue of the last run (PersistentQueue): shown paused; «play» starts it at `position_s`.
    pub fn restore(&self, items: Vec<Playable>, index: usize, position_s: f64) {
        if items.is_empty() {
            return;
        }
        let index = index.min(items.len() - 1);
        let track = items[index].track.clone();
        *self.queue.lock().expect("queue") = (items, index);
        *self.original.lock().expect("original") = None;
        *self.start_at.lock().expect("start at") = Some((track.id.clone(), position_s.max(0.0)));
        let up = self.upcoming();
        self.now.send_modify(|n| {
            n.track = Some(track);
            n.index = Some(index);
            n.playing = false;
            n.upcoming = up;
            n.queue_rev += 1;
        });
    }

    /// Where a track shown but not started yet will start (the restored one), else None.
    fn pending_start(&self) -> Option<f64> {
        if self.current_id.lock().expect("current").is_some() {
            return None;
        }
        let id = self.now.borrow().track.as_ref().map(|t| t.id.clone())?;
        self.start_at.lock().expect("start at").as_ref().filter(|(t, _)| *t == id).map(|(_, s)| *s)
    }

    /// The whole queue and the playing index (the queue panel).
    pub fn queue_snapshot(&self) -> (Vec<Track>, usize) {
        let q = self.queue.lock().expect("queue");
        (q.0.iter().map(|p| p.track.clone()).collect(), q.1)
    }

    fn queue_changed(&self) {
        let up = self.upcoming();
        self.now.send_modify(|n| {
            n.upcoming = up;
            n.queue_rev += 1;
        });
    }

    /// Takes a track out of the queue (not the playing one).
    pub fn queue_remove(&self, index: usize) {
        {
            let mut q = self.queue.lock().expect("queue");
            if index >= q.0.len() || index == q.1 {
                return;
            }
            let gone = q.0.remove(index);
            if index < q.1 {
                q.1 -= 1;
            }
            if let Some(o) = self.original.lock().expect("original").as_mut()
                && let Some(i) = o.iter().position(|p| p.track.id == gone.track.id)
            {
                o.remove(i);
            }
        }
        self.queue_changed();
    }

    /// Moves a queued track to another place (the playing one keeps playing wherever it ends up).
    pub fn queue_move(&self, from: usize, to: usize) {
        {
            let mut q = self.queue.lock().expect("queue");
            let len = q.0.len();
            if from >= len || to >= len || from == to {
                return;
            }
            let item = q.0.remove(from);
            q.0.insert(to, item);
            let cur = q.1;
            q.1 = if cur == from {
                to
            } else if from < cur && to >= cur {
                cur - 1
            } else if from > cur && to <= cur {
                cur + 1
            } else {
                cur
            };
        }
        self.queue_changed();
    }

    /// Everything after the playing track goes.
    pub fn queue_clear_upcoming(&self) {
        {
            let mut q = self.queue.lock().expect("queue");
            let keep = (q.1 + 1).min(q.0.len());
            q.0.truncate(keep);
        }
        *self.original.lock().expect("original") = None;
        self.queue_changed();
    }

    /// Plays the queue from `index` (a click in the queue panel).
    pub fn play_index(self: &Arc<Self>, index: usize) {
        self.start_index(index);
    }

    fn upcoming(&self) -> Vec<Track> {
        let q = self.queue.lock().expect("queue");
        q.0.iter()
            .skip(q.1 + 1)
            .take(5)
            .map(|p| p.track.clone())
            .collect()
    }

    fn start_index(self: &Arc<Self>, index: usize) {
        let item = {
            let mut q = self.queue.lock().expect("queue");
            if index >= q.0.len() {
                self.now.send_modify(|n| n.playing = false);
                return;
            }
            q.1 = index;
            q.0[index].clone()
        };
        let upcoming: Vec<Track> = {
            let q = self.queue.lock().expect("queue");
            q.0.iter()
                .skip(index + 1)
                .take(5)
                .map(|p| p.track.clone())
                .collect()
        };
        self.now.send_modify(|n| {
            n.track = Some(item.track.clone());
            n.index = Some(index);
            n.playing = true;
            n.error = None;
            n.upcoming = upcoming;
            n.queue_rev += 1;
            n.lyrics = None;
        });
        // Lyrics in the background; dropped if the track changed meanwhile.
        {
            let (now, http, t) = (self.now.clone(), self.http.clone(), item.track.clone());
            self.handle.spawn(async move {
                let found = drx_lyrics::find_any(
                    &http,
                    &t.title,
                    t.artists.first().map(|a| a.name.as_str()).unwrap_or(""),
                    Some(t.duration_ms / 1000).filter(|d| *d > 0),
                )
                .await;
                if let Some(l) = found {
                    now.send_if_modified(|n| {
                        let same = n.track.as_ref().is_some_and(|x| x.id == t.id);
                        if same {
                            n.lyrics = Some(Arc::new(l));
                        }
                        same
                    });
                }
            });
        }
        let this = self.clone();
        // Opening a stream does network round-trips; never on the UI thread.
        std::thread::spawn(move || {
            let res = this.source_for(&item).and_then(|(src, ext, gain)| {
                let player = this.player()?;
                // A position picked before the track started (only for this very track).
                let at = this
                    .start_at
                    .lock()
                    .expect("start at")
                    .take()
                    .filter(|(id, _)| *id == item.track.id)
                    .map_or(0.0, |(_, s)| s);
                // Opening a stream in the middle can be refused (YouTube answers a cold ranged request with 403):
                // start from the top and jump once the sound runs.
                player.play(src, ext, 0.0, gain).map(|id| (id, at, player))
            });
            match res {
                Ok((id, at, player)) => {
                    *this.current_id.lock().expect("current") = Some(id);
                    this.skipped.store(0, std::sync::atomic::Ordering::Relaxed);
                    if at > 0.5 {
                        for _ in 0..100 {
                            let c = player.clock();
                            if c.track == Some(id) && c.playing {
                                player.seek(at);
                                break;
                            }
                            if *this.current_id.lock().expect("current") != Some(id) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("cannot play {}: {e}", item.track.id);
                    use std::sync::atomic::Ordering::Relaxed;
                    let more = {
                        let q = this.queue.lock().expect("queue");
                        q.1 == index && index + 1 < q.0.len()
                    };
                    let skip = this.skip_on_error.load(Relaxed) && more && this.skipped.fetch_add(1, Relaxed) < 5;
                    this.now.send_modify(|n| {
                        n.playing = skip;
                        n.error = Some(e);
                    });
                    if skip {
                        this.start_index(index + 1);
                    } else {
                        this.skipped.store(0, Relaxed);
                    }
                }
            }
        });
    }

    pub fn toggle(self: &Arc<Self>) {
        // The queue restored after launch: nothing is loaded yet, «play» starts its track (where it was left).
        if self.current_id.lock().expect("current").is_none() {
            let (has, i) = {
                let q = self.queue.lock().expect("queue");
                (q.1 < q.0.len(), q.1)
            };
            if has && !self.now.borrow().playing {
                self.start_index(i);
            }
            return;
        }
        let Some(p) = self.player.lock().expect("player lock").clone() else {
            return;
        };
        let playing = self.now.borrow().playing;
        if playing {
            p.pause()
        } else {
            p.resume()
        }
        self.now.send_modify(|n| n.playing = !playing);
    }

    pub fn next(self: &Arc<Self>) {
        let (i, len) = {
            let q = self.queue.lock().expect("queue");
            (q.1 + 1, q.0.len())
        };
        // Past the end: back to the start with repeat all.
        let i = if i >= len && self.now.borrow().repeat == Repeat::All {
            0
        } else {
            i
        };
        self.start_index(i);
    }

    pub fn previous(self: &Arc<Self>) {
        // Like every player: past 3 seconds «previous» restarts the track.
        if self.clock().position_s > 3.0 {
            self.seek(0.0);
            return;
        }
        let (i, len) = {
            let q = self.queue.lock().expect("queue");
            (q.1, q.0.len())
        };
        let i = if i == 0 && self.now.borrow().repeat == Repeat::All {
            len.saturating_sub(1)
        } else {
            i.saturating_sub(1)
        };
        self.start_index(i);
    }

    pub fn seek(&self, secs: f64) {
        let loaded = self.current_id.lock().expect("current").is_some();
        match self.player.lock().expect("player lock").as_ref() {
            Some(p) if loaded => p.seek(secs),
            // Nothing loaded yet (the track shown after launch): it starts there when played.
            _ => {
                if let Some(t) = self.now.borrow().track.as_ref() {
                    *self.start_at.lock().expect("start at") = Some((t.id.clone(), secs.max(0.0)));
                }
            }
        }
    }

    pub fn set_volume(&self, v: f32) {
        *self.volume.lock().expect("volume") = v;
        let fade = *self.fade.lock().expect("fade");
        if let Some(p) = self.player.lock().expect("player lock").as_ref() {
            p.set_volume(v * fade);
        }
    }

    /// The sleep timer's fade-out: the volume times `m` (1 = as set).
    pub fn set_fade(&self, m: f32) {
        let m = m.clamp(0.0, 1.0);
        *self.fade.lock().expect("fade") = m;
        let v = *self.volume.lock().expect("volume");
        if let Some(p) = self.player.lock().expect("player lock").as_ref() {
            p.set_volume(v * m);
        }
    }

    pub fn clock(&self) -> PlaybackClock {
        match self.player.lock().expect("player lock").as_ref() {
            Some(p) => {
                let mut c = p.clock();
                // MP3 streams without a Xing header have no length until fully read: use the catalog's.
                if c.duration_s.is_none() {
                    c.duration_s = self
                        .now
                        .borrow()
                        .track
                        .as_ref()
                        .map(|t| t.duration_ms as f64 / 1000.0)
                        .filter(|d| *d > 0.0);
                }
                c
            }
            None => PlaybackClock {
                track: None,
                position_s: self.pending_start().unwrap_or(0.0),
                duration_s: self
                    .now
                    .borrow()
                    .track
                    .as_ref()
                    .map(|t| t.duration_ms as f64 / 1000.0)
                    .filter(|d| *d > 0.0),
                playing: false,
                at: std::time::Instant::now(),
            },
        }
    }
}

use crate::songs::is_youtube_video_id;

/// A format hint as the decoder takes it.
fn static_ext(ext: Option<&str>) -> Option<&'static str> {
    match ext? {
        "mp3" => Some("mp3"),
        "m4a" => Some("m4a"),
        "mp4" => Some("mp4"),
        "webm" => Some("webm"),
        "ogg" => Some("ogg"),
        "opus" => Some("opus"),
        "flac" => Some("flac"),
        _ => None,
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    #[test]
    fn shuffle_keeps_every_item() {
        let mut v: Vec<u32> = (0..200).collect();
        shuffle_in_place(&mut v);
        assert_ne!(v, (0..200).collect::<Vec<_>>());
        v.sort();
        assert_eq!(v, (0..200).collect::<Vec<_>>());
    }

    #[test]
    fn repeat_button_cycles_like_android() {
        assert_eq!(Repeat::Off.next(), Repeat::All);
        assert_eq!(Repeat::All.next(), Repeat::One);
        assert_eq!(Repeat::One.next(), Repeat::Off);
        assert_eq!(Repeat::from_i32(2), Repeat::All);
        assert_eq!(Repeat::from_i32(7), Repeat::Off);
    }
}

#[cfg(test)]
mod id_tests {
    #[test]
    fn youtube_video_ids() {
        assert!(super::is_youtube_video_id("GAqvJ6FhllE"));
        assert!(super::is_youtube_video_id("_LU6OTVCgvw"));
        assert!(!super::is_youtube_video_id("src:VK:1_2"));
        assert!(!super::is_youtube_video_id("SP_30F1234"));
        assert!(!super::is_youtube_video_id("MPREb_0Cabcdef"));
    }
}
