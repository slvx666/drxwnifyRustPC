//! `settings.preferences_pb` in memory.
//!
//! The phone keeps big lists in the same file (`exportedSongUris` alone is ~0.7 MB), and reading and decoding it on
//! every `pref_*` call held the UI thread for about two seconds when the settings page opened (dozens of keys).
//! Now the file is read once and again only when it changed on disk (import, a restored backup); the copy in memory
//! leaves out values bigger than [`BIG_VALUE`] (nothing on the PC reads them). Writes go to a background thread that
//! applies each change to the file itself (read, change one key, write), so the big values stay in the file.

use drx_core::prefs::{PrefValue, Prefs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

/// Values bigger than this stay only in the file.
const BIG_VALUE: usize = 16 * 1024;

type Stamp = Option<(SystemTime, u64)>;

struct State {
    stamp: Stamp,
    loaded: bool,
    prefs: Arc<Prefs>,
}

struct Shared {
    path: PathBuf,
    state: Mutex<State>,
    /// Changes sent to the writer and not yet in the file: while any are pending the file is older than memory.
    pending: AtomicUsize,
}

pub(crate) struct PrefsCache {
    shared: Arc<Shared>,
    tx: Mutex<mpsc::Sender<(String, PrefValue)>>,
}

fn stamp_of(path: &std::path::Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn size_of(v: &PrefValue) -> usize {
    match v {
        PrefValue::String(s) => s.len(),
        PrefValue::StringSet(v) => v.iter().map(|s| s.len() + 2).sum(),
        PrefValue::Bytes(b) => b.len(),
        _ => 8,
    }
}

fn read_light(path: &std::path::Path) -> Prefs {
    let mut p = std::fs::read(path).ok().and_then(|b| Prefs::decode(&b).ok()).unwrap_or_default();
    p.values.retain(|_, v| size_of(v) <= BIG_VALUE);
    p
}

impl PrefsCache {
    pub(crate) fn new(path: PathBuf) -> Self {
        let shared = Arc::new(Shared {
            path,
            state: Mutex::new(State { stamp: None, loaded: false, prefs: Arc::default() }),
            pending: AtomicUsize::new(0),
        });
        let (tx, rx) = mpsc::channel::<(String, PrefValue)>();
        let s = shared.clone();
        let _ = std::thread::Builder::new().name("drx-prefs".into()).stack_size(256 * 1024).spawn(move || {
            while let Ok((key, value)) = rx.recv() {
                let mut prefs = std::fs::read(&s.path).ok().and_then(|b| Prefs::decode(&b).ok()).unwrap_or_default();
                prefs.set(key, value);
                // Everything else already queued goes into the same write.
                while let Ok((k, v)) = rx.try_recv() {
                    prefs.set(k, v);
                    s.pending.fetch_sub(1, Ordering::AcqRel);
                }
                if let Err(e) = drx_core::paths::write_atomic(&s.path, &prefs.encode()) {
                    eprintln!("settings not saved: {e}");
                }
                // The file now holds what memory holds: remember its stamp so it is not read back.
                let mut st = s.state.lock().expect("prefs");
                st.stamp = stamp_of(&s.path);
                s.pending.fetch_sub(1, Ordering::AcqRel);
            }
        });
        PrefsCache { shared, tx: Mutex::new(tx) }
    }

    /// The settings now (cheap: one file stat unless the file changed).
    pub(crate) fn get(&self) -> Arc<Prefs> {
        let s = &self.shared;
        let mut st = s.state.lock().expect("prefs");
        if s.pending.load(Ordering::Acquire) == 0 {
            let now = stamp_of(&s.path);
            if !st.loaded || now != st.stamp {
                st.prefs = Arc::new(read_light(&s.path));
                st.stamp = now;
                st.loaded = true;
            }
        }
        st.prefs.clone()
    }

    pub(crate) fn set(&self, key: &str, value: PrefValue) {
        let s = &self.shared;
        {
            let mut st = s.state.lock().expect("prefs");
            if !st.loaded {
                st.prefs = Arc::new(read_light(&s.path));
                st.stamp = stamp_of(&s.path);
                st.loaded = true;
            }
            let p = Arc::make_mut(&mut st.prefs);
            if size_of(&value) <= BIG_VALUE {
                p.set(key, value.clone());
            } else {
                p.remove(key);
            }
            s.pending.fetch_add(1, Ordering::AcqRel);
        }
        if self.tx.lock().expect("prefs tx").send((key.to_string(), value)).is_err() {
            s.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Waits (up to `limit`) until every change is in the file: before exit and before the file is replaced.
    pub(crate) fn flush(&self, limit: Duration) {
        let until = Instant::now() + limit;
        while self.shared.pending.load(Ordering::Acquire) > 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The file was replaced (import): read it again on next use.
    pub(crate) fn invalidate(&self) {
        self.flush(Duration::from_secs(2));
        self.shared.state.lock().expect("prefs").loaded = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_keep_big_values_and_reads_see_changes() {
        let dir = std::env::temp_dir().join(format!("drx-prefs-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.preferences_pb");
        let mut p = Prefs::default();
        p.set("big", PrefValue::StringSet((0..5000).map(|i| format!("content://song/{i}")).collect()));
        p.set("a", PrefValue::Bool(true));
        drx_core::paths::write_atomic(&path, &p.encode()).unwrap();

        let c = PrefsCache::new(path.clone());
        assert_eq!(c.get().get_bool("a"), Some(true));
        assert!(c.get().get("big").is_none(), "big values stay out of memory");
        c.set("a", PrefValue::Bool(false));
        c.set("b", PrefValue::String("x".into()));
        assert_eq!(c.get().get_bool("a"), Some(false), "a change is seen at once");
        c.flush(Duration::from_secs(5));
        let on_disk = Prefs::decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk.get_bool("a"), Some(false));
        assert_eq!(on_disk.get_str("b"), Some("x"));
        assert!(on_disk.get("big").is_some(), "the file keeps the big list");

        // Someone else replaces the file: the next read sees it.
        std::thread::sleep(Duration::from_millis(20));
        let mut q = Prefs::default();
        q.set("a", PrefValue::Bool(true));
        q.set("pad", PrefValue::String("changed".into()));
        drx_core::paths::write_atomic(&path, &q.encode()).unwrap();
        assert_eq!(c.get().get_str("pad"), Some("changed"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
