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

fn cache_name(id: &str) -> String {
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
    current_id: Mutex<Option<drx_audio::player::TrackId>>,
    pub now: watch::Sender<NowPlaying>,
    volume: Mutex<f32>,
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
            current_id: Mutex::new(None),
            now: watch::channel(NowPlaying::default()).0,
            volume: Mutex::new(0.8),
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
        player.set_volume(*self.volume.lock().expect("volume"));
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
                            // Track finished on its own: continue with the queue.
                            let next = this.queue.lock().expect("queue").1 + 1;
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

    fn source_for(
        &self,
        p: &Playable,
    ) -> Result<(CachedSource, Option<&'static str>, f32), String> {
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
        let file = self.cache_dir.join("audio").join(cache_name(&p.track.id));
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
    pub fn play_list(self: &Arc<Self>, list: Vec<Playable>, index: usize) {
        *self.queue.lock().expect("queue") = (list, index);
        self.start_index(index);
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
        self.now.send_modify(|n| {
            n.track = Some(item.track.clone());
            n.index = Some(index);
            n.playing = true;
            n.error = None;
        });
        let this = self.clone();
        // Opening a stream does network round-trips; never on the UI thread.
        std::thread::spawn(move || {
            let res = this.source_for(&item).and_then(|(src, ext, gain)| {
                let player = this.player()?;
                player.play(src, ext, 0.0, gain)
            });
            match res {
                Ok(id) => *this.current_id.lock().expect("current") = Some(id),
                Err(e) => {
                    tracing::warn!("cannot play {}: {e}", item.track.id);
                    this.now.send_modify(|n| {
                        n.playing = false;
                        n.error = Some(e);
                    });
                }
            }
        });
    }

    pub fn toggle(self: &Arc<Self>) {
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
        let i = self.queue.lock().expect("queue").1 + 1;
        self.start_index(i);
    }

    pub fn previous(self: &Arc<Self>) {
        // Like every player: past 3 seconds «previous» restarts the track.
        if self.clock().position_s > 3.0 {
            self.seek(0.0);
            return;
        }
        let i = self.queue.lock().expect("queue").1.saturating_sub(1);
        self.start_index(i);
    }

    pub fn seek(&self, secs: f64) {
        if let Some(p) = self.player.lock().expect("player lock").as_ref() {
            p.seek(secs);
        }
    }

    pub fn set_volume(&self, v: f32) {
        *self.volume.lock().expect("volume") = v;
        if let Some(p) = self.player.lock().expect("player lock").as_ref() {
            p.set_volume(v);
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
                position_s: 0.0,
                duration_s: None,
                playing: false,
                at: std::time::Instant::now(),
            },
        }
    }
}
