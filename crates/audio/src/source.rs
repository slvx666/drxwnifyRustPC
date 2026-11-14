//! A seekable byte source over a network stream, backed by a cache file on disk.
//!
//! One background thread downloads into the file; the decoder reads from the file and waits for bytes that are not
//! there yet. A seek far ahead of the download restarts it at the new place (HTTP range request), so seeking in a
//! long track does not wait for everything before it. Whole tracks never sit in memory (Android keeps VK tracks in
//! RAM; on the PC that would break Mini's budget).
//!
//! The network side is abstracted as [`RangeOpener`], implemented by the app with the core HTTP client; it also
//! handles expired URLs (re-resolve and retry), which Android does 15 minutes before expiry.

use crate::decode::ByteSource;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Opens the stream at a byte offset. Returns the total length when known and a reader from that offset.
pub trait RangeOpener: Send + Sync + 'static {
    fn open(&self, offset: u64) -> std::io::Result<(Option<u64>, Box<dyn Read + Send>)>;
}

/// Restart the download when a read lands this far beyond what is downloaded.
const RESTART_DISTANCE: u64 = 256 * 1024;
const CHUNK: usize = 64 * 1024;

#[derive(Default)]
struct State {
    /// Downloaded byte ranges, sorted, non-overlapping, merged.
    ranges: Vec<(u64, u64)>,
    total: Option<u64>,
    /// Where the downloader currently writes.
    head: u64,
    restart_at: Option<u64>,
    error: Option<String>,
    stop: bool,
}

impl State {
    fn add(&mut self, start: u64, end: u64) {
        self.ranges.push((start, end));
        self.ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.ranges.len());
        for &(s, e) in &self.ranges {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.ranges = merged;
    }
    /// Bytes available at `pos` (0 if `pos` is not downloaded yet).
    fn available(&self, pos: u64) -> u64 {
        self.ranges
            .iter()
            .find(|&&(s, e)| s <= pos && pos < e)
            .map(|&(_, e)| e - pos)
            .unwrap_or(0)
    }
    fn complete(&self) -> bool {
        matches!((self.total, self.ranges.first()), (Some(t), Some(&(0, e))) if e >= t)
    }
    /// First byte at or after `from` that is not downloaded.
    fn next_gap(&self, from: u64) -> u64 {
        let mut p = from;
        for &(s, e) in &self.ranges {
            if s <= p && p < e {
                p = e;
            }
        }
        p
    }
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

/// The decoder-side handle.
pub struct CachedSource {
    file: File,
    pos: u64,
    shared: Arc<Shared>,
    path: PathBuf,
}

impl CachedSource {
    /// Starts downloading into `cache_file` and returns a reader. The first open happens before returning, so the
    /// total length (needed by some demuxers) is known.
    pub fn start(opener: Arc<dyn RangeOpener>, cache_file: &Path) -> std::io::Result<Self> {
        if let Some(dir) = cache_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let writer = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(cache_file)?;
        let reader = File::open(cache_file)?;
        let (total, first) = opener.open(0)?;
        if let Some(t) = total {
            writer.set_len(t)?;
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                total,
                ..Default::default()
            }),
            cv: Condvar::new(),
        });
        let s2 = shared.clone();
        std::thread::Builder::new()
            .name("audio-download".into())
            .spawn(move || download(opener, writer, first, s2))?;
        Ok(Self {
            file: reader,
            pos: 0,
            shared,
            path: cache_file.to_path_buf(),
        })
    }

    /// True when the whole stream is in the cache file.
    pub fn is_complete(&self) -> bool {
        self.shared.state.lock().expect("source lock").complete()
    }

    pub fn cache_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CachedSource {
    fn drop(&mut self) {
        self.shared.state.lock().expect("source lock").stop = true;
        self.shared.cv.notify_all();
    }
}

fn download(
    opener: Arc<dyn RangeOpener>,
    mut file: File,
    first: Box<dyn Read + Send>,
    shared: Arc<Shared>,
) {
    let mut reader = first;
    let mut offset = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        // Fill from `offset` until the stream ends, a restart is requested, or we stop.
        let mut restart = None;
        loop {
            {
                let mut st = shared.state.lock().expect("source lock");
                if st.stop {
                    return;
                }
                if let Some(r) = st.restart_at.take() {
                    restart = Some(r);
                    break;
                }
                st.head = offset;
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let mut st = shared.state.lock().expect("source lock");
                    st.error = Some(e.to_string());
                    shared.cv.notify_all();
                    return;
                }
            };
            if file
                .seek(SeekFrom::Start(offset))
                .and_then(|_| file.write_all(&buf[..n]))
                .is_err()
            {
                let mut st = shared.state.lock().expect("source lock");
                st.error = Some("cache write failed".into());
                shared.cv.notify_all();
                return;
            }
            let mut st = shared.state.lock().expect("source lock");
            st.add(offset, offset + n as u64);
            offset += n as u64;
            shared.cv.notify_all();
        }
        // Decide where to continue: the requested place, or the first gap after the start.
        let next = {
            let mut st = shared.state.lock().expect("source lock");
            if st.total.is_none() && restart.is_none() {
                // Unknown length and the stream ended: that was everything.
                st.total = Some(offset);
            }
            if st.complete() {
                shared.cv.notify_all();
                return;
            }
            let from = restart.unwrap_or(0);
            let gap = st.next_gap(from);
            match st.total {
                Some(t) if gap >= t => st.next_gap(0),
                _ => gap,
            }
        };
        match opener.open(next) {
            Ok((_, r)) => {
                reader = r;
                offset = next;
            }
            Err(e) => {
                let mut st = shared.state.lock().expect("source lock");
                st.error = Some(e.to_string());
                shared.cv.notify_all();
                return;
            }
        }
    }
}

impl Read for CachedSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let avail = {
            let mut st = self.shared.state.lock().expect("source lock");
            loop {
                if let Some(t) = st.total
                    && self.pos >= t
                {
                    return Ok(0);
                }
                let a = st.available(self.pos);
                if a > 0 {
                    break a;
                }
                if let Some(e) = &st.error {
                    return Err(std::io::Error::other(e.clone()));
                }
                let far = self.pos > st.head + RESTART_DISTANCE
                    || self.pos < st.head.saturating_sub(CHUNK as u64) && !st.complete();
                if far && st.restart_at.is_none() {
                    st.restart_at = Some(self.pos - self.pos % CHUNK as u64);
                }
                let (g, _) = self
                    .shared
                    .cv
                    .wait_timeout(st, Duration::from_millis(500))
                    .expect("source lock");
                st = g;
            }
        };
        let n = (buf.len() as u64).min(avail) as usize;
        self.file.seek(SeekFrom::Start(self.pos))?;
        let got = self.file.read(&mut buf[..n])?;
        self.pos += got as u64;
        Ok(got)
    }
}

impl Seek for CachedSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let total = self.shared.state.lock().expect("source lock").total;
        let new = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => match total {
                Some(t) => t as i64 + d,
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "length unknown",
                    ));
                }
            },
        };
        if new < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

impl ByteSource for CachedSource {
    fn byte_len(&self) -> Option<u64> {
        self.shared.state.lock().expect("source lock").total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Serves bytes from memory in small slow chunks; counts opens.
    struct Slow {
        data: Arc<Vec<u8>>,
        opens: Arc<AtomicUsize>,
    }
    struct SlowReader {
        data: Arc<Vec<u8>>,
        pos: usize,
    }
    impl Read for SlowReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(2));
            let n = buf.len().min(16 * 1024).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }
    impl RangeOpener for Slow {
        fn open(&self, offset: u64) -> std::io::Result<(Option<u64>, Box<dyn Read + Send>)> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok((
                Some(self.data.len() as u64),
                Box::new(SlowReader {
                    data: self.data.clone(),
                    pos: offset as usize,
                }),
            ))
        }
    }

    fn data(n: usize) -> Arc<Vec<u8>> {
        Arc::new((0..n).map(|i| (i * 31 % 251) as u8).collect())
    }

    #[test]
    fn reads_everything_in_order() {
        let d = data(1_000_000);
        let dir = tempfile::tempdir().unwrap();
        let opens = Arc::new(AtomicUsize::new(0));
        let mut s = CachedSource::start(
            Arc::new(Slow {
                data: d.clone(),
                opens: opens.clone(),
            }),
            &dir.path().join("a"),
        )
        .unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        assert_eq!(out, *d);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn far_seek_restarts_download_there() {
        let d = data(20_000_000);
        let dir = tempfile::tempdir().unwrap();
        let opens = Arc::new(AtomicUsize::new(0));
        let mut s = CachedSource::start(
            Arc::new(Slow {
                data: d.clone(),
                opens: opens.clone(),
            }),
            &dir.path().join("b"),
        )
        .unwrap();
        s.seek(SeekFrom::Start(15_000_000)).unwrap();
        let mut buf = vec![0u8; 100_000];
        let t = std::time::Instant::now();
        s.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..], &d[15_000_000..15_100_000]);
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "did not wait for the whole prefix: {:?}",
            t.elapsed()
        );
        assert!(opens.load(Ordering::SeqCst) >= 2);
        // Going back to the start still works (the gap gets filled).
        s.seek(SeekFrom::Start(0)).unwrap();
        let mut head = vec![0u8; 50_000];
        s.read_exact(&mut head).unwrap();
        assert_eq!(&head[..], &d[..50_000]);
    }
}
