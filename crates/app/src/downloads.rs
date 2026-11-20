//! Tracks kept for offline listening (Android DownloadUtil, DL-001): the best stream found is downloaded whole into
//! `pc_downloads/` of the profile, listed in `pc_downloads.json`; playback takes the file instead of the network.
//! One download at a time, in the order asked; the UI follows a progress line.

use crate::App;
use crate::playback::{Playable, cache_name};
use drx_catalog::model::Track;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DIR: &str = "pc_downloads";
const INDEX: &str = "pc_downloads.json";
/// A track that takes longer than this to download is given up (and can be asked again).
const MAX_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DownloadEntry {
    pub track: Track,
    /// The file's name in `pc_downloads/`.
    pub file: String,
    pub ext: Option<String>,
    pub bytes: u64,
    /// Unix ms.
    pub at: i64,
}

/// What the downloader does now.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DownloadProgress {
    /// Tracks waiting (the current one included).
    pub queued: usize,
    pub current: Option<String>,
    /// 0..1 of the current track, when its size is known.
    pub fraction: f32,
    /// Done and failed in this run of the queue.
    pub done: usize,
    pub failed: usize,
}

/// Deletes a file the download thread may still hold for a moment (Windows refuses until it lets go).
fn remove_soon(path: &std::path::Path) {
    for _ in 0..30 {
        if std::fs::remove_file(path).is_ok() || !path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

impl App {
    fn downloads_dir(&self) -> std::path::PathBuf {
        self.inner.profile.root().join(DIR)
    }

    /// Every downloaded track, the newest first.
    pub fn downloads(&self) -> Vec<DownloadEntry> {
        let mut v: Vec<DownloadEntry> = std::fs::read(self.inner.profile.root().join(INDEX))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        v.sort_by_key(|b| std::cmp::Reverse(b.at));
        v
    }

    fn write_downloads(&self, list: &[DownloadEntry]) {
        if let Ok(b) = serde_json::to_vec(list) {
            let _ = drx_core::paths::write_atomic(&self.inner.profile.root().join(INDEX), &b);
        }
        self.publish_downloads(list);
    }

    /// Tells playback which tracks have a file, and the UI that the list changed.
    fn publish_downloads(&self, list: &[DownloadEntry]) {
        let dir = self.downloads_dir();
        let map = list
            .iter()
            .filter(|e| dir.join(&e.file).exists())
            .map(|e| (e.track.id.clone(), (dir.join(&e.file), e.ext.clone())))
            .collect();
        *self.inner.playback.downloaded.lock().expect("downloaded") = map;
        self.inner.downloads_version.send_modify(|v| *v += 1);
    }

    /// Reads the index at launch (files deleted by hand drop out).
    pub fn load_downloads(&self) {
        let dir = self.downloads_dir();
        let list: Vec<DownloadEntry> = self
            .downloads()
            .into_iter()
            .filter(|e| dir.join(&e.file).exists())
            .collect();
        // Halves of downloads cut off by a quit or a failure.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.path().extension().is_some_and(|x| x == "part") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        self.publish_downloads(&list);
    }

    pub fn is_downloaded(&self, id: &str) -> bool {
        self.inner
            .playback
            .downloaded
            .lock()
            .expect("downloaded")
            .contains_key(id)
    }

    /// Changes of the downloads (a counter): the downloads page redraws.
    pub fn downloads_version(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.downloads_version.subscribe()
    }

    pub fn download_progress(&self) -> tokio::sync::watch::Receiver<DownloadProgress> {
        self.inner.download_progress.subscribe()
    }

    /// Size of every downloaded file, bytes.
    pub fn downloads_bytes(&self) -> u64 {
        self.downloads().iter().map(|e| e.bytes).sum()
    }

    /// Takes a track's file away.
    pub fn delete_download(&self, id: &str) {
        let mut list = self.downloads();
        if let Some(i) = list.iter().position(|e| e.track.id == id) {
            let e = list.remove(i);
            let _ = std::fs::remove_file(self.downloads_dir().join(&e.file));
            self.write_downloads(&list);
        }
    }

    /// Puts tracks in the download queue (already downloaded or queued ones are skipped). Returns how many joined.
    pub fn download_tracks(&self, tracks: Vec<Track>) -> usize {
        let fresh: Vec<Track> = {
            let mut q = self.inner.download_queue.lock().expect("download queue");
            let fresh: Vec<Track> = tracks
                .into_iter()
                .filter(|t| {
                    !t.id.starts_with("local:")
                        && !self.is_downloaded(&t.id)
                        && !q.iter().any(|x| x.id == t.id)
                })
                .collect();
            q.extend(fresh.iter().cloned());
            fresh
        };
        if fresh.is_empty() {
            return 0;
        }
        self.remember(fresh.iter());
        let n = self
            .inner
            .download_queue
            .lock()
            .expect("download queue")
            .len();
        self.inner.download_progress.send_modify(|p| p.queued = n);
        let start = !self
            .inner
            .download_running
            .swap(true, std::sync::atomic::Ordering::SeqCst);
        if start {
            let app = self.clone();
            let _ = std::thread::Builder::new()
                .name("downloads".into())
                .spawn(move || app.download_worker());
        }
        fresh.len()
    }

    /// Every track of a list into the queue (a page's «Скачать все треки»).
    pub async fn download_list(&self, handle: crate::ListHandle) -> crate::AppResult<usize> {
        let items = self.list_items(handle).await?;
        Ok(self.download_tracks(items.into_iter().map(|p| p.track).collect()))
    }

    /// «Копировать в мои плейлисты» (the phone's import of a playlist): every track of a list into a new playlist of
    /// mine named `name`. Returns its id and how many tracks went in.
    pub async fn copy_list_to_playlist(
        &self,
        handle: crate::ListHandle,
        name: &str,
    ) -> crate::AppResult<(String, usize)> {
        let items = self.list_items(handle).await?;
        if items.is_empty() {
            return Err(crate::AppError::Other("в списке нет треков".into()));
        }
        let id = self.create_playlist(name.trim()).await?;
        let n = self
            .add_to_playlist(&id, items.into_iter().map(|p| p.track).collect())
            .await?;
        Ok((id, n))
    }

    /// Stops after the current track and forgets the rest of the queue.
    pub fn cancel_downloads(&self) {
        self.inner
            .download_queue
            .lock()
            .expect("download queue")
            .clear();
    }

    fn download_worker(&self) {
        loop {
            let next = {
                let q = self.inner.download_queue.lock().expect("download queue");
                q.front().cloned()
            };
            let Some(track) = next else { break };
            let label = format!("{} — {}", track.artist_names(), track.title);
            self.inner.download_progress.send_modify(|p| {
                p.current = Some(label.clone());
                p.fraction = 0.0;
            });
            // A stream that breaks off gets one more try (a fresh source from the resolver).
            let ok = self.download_one(&track).or_else(|e| {
                tracing::info!("download {} retried after: {e}", track.id);
                self.download_one(&track)
            });
            {
                let mut q = self.inner.download_queue.lock().expect("download queue");
                if q.front().is_some_and(|t| t.id == track.id) {
                    q.pop_front();
                }
                let n = q.len();
                self.inner.download_progress.send_modify(|p| {
                    p.queued = n;
                    if ok.is_ok() {
                        p.done += 1;
                    } else {
                        p.failed += 1;
                    }
                });
            }
            if let Err(e) = ok {
                tracing::warn!("download {}: {e}", track.id);
            }
        }
        self.inner
            .download_running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.inner.download_progress.send_modify(|p| {
            p.current = None;
            p.fraction = 0.0;
        });
        // Something asked for in the last instant starts a new worker.
        if !self
            .inner
            .download_queue
            .lock()
            .expect("download queue")
            .is_empty()
            && !self
                .inner
                .download_running
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let app = self.clone();
            let _ = std::thread::Builder::new()
                .name("downloads".into())
                .spawn(move || app.download_worker());
        }
    }

    fn download_one(&self, track: &Track) -> Result<(), String> {
        let dir = self.downloads_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let name = cache_name(&track.id);
        let part = dir.join(format!("{name}.part"));
        let p = Playable {
            track: track.clone(),
            url: None,
        };
        let (src, ext, _) = self
            .inner
            .playback
            .stream_into(&p, &part)
            .inspect_err(|_| remove_soon(&part))?;
        let started = std::time::Instant::now();
        loop {
            if src.is_complete() {
                break;
            }
            if let Some(e) = src.error() {
                drop(src);
                remove_soon(&part);
                return Err(e);
            }
            if started.elapsed() > MAX_WAIT {
                drop(src);
                remove_soon(&part);
                return Err("слишком долго".into());
            }
            let (have, total) = src.progress();
            if let Some(t) = total.filter(|t| *t > 0) {
                self.inner
                    .download_progress
                    .send_modify(|p| p.fraction = (have as f32 / t as f32).clamp(0.0, 1.0));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        drop(src);
        let file = name.clone();
        // The download thread lets go of the file a moment after it finishes.
        let mut moved = std::fs::rename(&part, dir.join(&file));
        for _ in 0..30 {
            if moved.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            moved = std::fs::rename(&part, dir.join(&file));
        }
        moved.map_err(|e| e.to_string())?;
        let bytes = std::fs::metadata(dir.join(&file))
            .map(|m| m.len())
            .unwrap_or(0);
        let mut list = self.downloads();
        list.retain(|e| e.track.id != track.id);
        list.push(DownloadEntry {
            track: track.clone(),
            file,
            ext: ext.map(str::to_string),
            bytes,
            at: crate::songs::now_ms(),
        });
        self.write_downloads(&list);
        Ok(())
    }
}
