//! The database writer: one thread owns the only read-write connection; everything else sends it jobs.
//!
//! Every change to a table is reported on a broadcast channel ([`TableChanged`]) so list handles in the facade can
//! refresh the rows on screen without polling (Android does this with Room `Flow` queries; DatabaseDao.kt has 128).
//! Changes are coalesced per job: one event per table per finished job.

use crate::db::{self, SchemaState};
use crate::error::{CoreError, Result};
use rusqlite::Connection;
use rusqlite::hooks::Action;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, oneshot};

/// Tables changed by one finished job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableChanged {
    pub tables: Vec<String>,
}

/// A job runs on the database thread and returns a completion that is called after change events are sent,
/// so a caller that awaits the job sees its own changes already announced.
type Job = Box<dyn FnOnce(&mut Connection) -> Box<dyn FnOnce() + Send> + Send>;

/// Handle to the database thread. Cheap to clone.
enum Msg {
    Job(Job),
    /// Closes the connection (releases the file) and stops the thread; the sender is told when done.
    Stop(std::sync::mpsc::Sender<()>),
}

#[derive(Clone)]
pub struct Store {
    tx: std::sync::mpsc::Sender<Msg>,
    changes: broadcast::Sender<TableChanged>,
}

impl Store {
    /// Opens the database and starts the writer thread. Refuses databases that are not the current version.
    pub fn open(path: &Path) -> Result<Self> {
        let (conn, state) = db::open(path)?;
        match state {
            SchemaState::Created | SchemaState::Current => {}
            SchemaState::Older(v) | SchemaState::Newer(v) => {
                return Err(CoreError::SchemaVersion {
                    found: v,
                    expected: db::ROOM_VERSION,
                });
            }
            SchemaState::Foreign => {
                return Err(crate::error::malformed(
                    "song.db",
                    "not a Drxwnify database",
                ));
            }
        }
        Ok(Self::start(conn))
    }

    fn start(mut conn: Connection) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        let (changes, _) = broadcast::channel(64);
        let pending: Arc<Mutex<BTreeSet<String>>> = Arc::default();
        {
            let pending = pending.clone();
            // Registering a hook on a fresh connection cannot fail in practice; without it lists would not refresh.
            conn.update_hook(Some(move |_: Action, _db: &str, table: &str, _row: i64| {
                pending
                    .lock()
                    .expect("pending lock")
                    .insert(table.to_string());
            }))
            .expect("register update hook");
        }
        let out = changes.clone();
        std::thread::Builder::new()
            .name("db-writer".into())
            .spawn(move || {
                while let Ok(msg) = rx.recv() {
                    let job = match msg {
                        Msg::Job(j) => j,
                        Msg::Stop(done) => {
                            drop(conn);
                            let _ = done.send(());
                            return;
                        }
                    };
                    let complete = job(&mut conn);
                    let tables: Vec<String> =
                        std::mem::take(&mut *pending.lock().expect("pending lock"))
                            .into_iter()
                            .collect();
                    if !tables.is_empty() {
                        let _ = out.send(TableChanged { tables });
                    }
                    complete();
                }
            })
            .expect("spawn db thread");
        Self { tx, changes }
    }

    /// Runs `f` on the database thread and returns its result.
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (done, wait) = oneshot::channel();
        self.tx
            .send(Msg::Job(Box::new(move |c| {
                let r = f(c);
                Box::new(move || {
                    let _ = done.send(r);
                })
            })))
            .map_err(|_| crate::error::malformed("store", "database thread stopped"))?;
        wait.await
            .map_err(|_| crate::error::malformed("store", "database job dropped"))?
    }

    /// Blocking variant for code outside the async runtime (tests, startup).
    pub fn call_blocking<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (done, wait) = std::sync::mpsc::channel();
        self.tx
            .send(Msg::Job(Box::new(move |c| {
                let r = f(c);
                Box::new(move || {
                    let _ = done.send(r);
                })
            })))
            .map_err(|_| crate::error::malformed("store", "database thread stopped"))?;
        wait.recv()
            .map_err(|_| crate::error::malformed("store", "database job dropped"))?
    }

    /// Closes the database file and stops the thread. Every clone of this handle stops working.
    /// Used before an import replaces `song.db`.
    pub fn shutdown(&self) {
        let (done, wait) = std::sync::mpsc::channel();
        if self.tx.send(Msg::Stop(done)).is_ok() {
            let _ = wait.recv();
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TableChanged> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_changed_tables_once_per_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("song.db")).unwrap();
        let mut rx = store.subscribe();
        store
            .call_blocking(|c| {
                c.execute("INSERT INTO search_history (query) VALUES ('a')", [])?;
                c.execute("INSERT INTO search_history (query) VALUES ('b')", [])?;
                Ok(())
            })
            .unwrap();
        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.tables, vec!["search_history".to_string()]);
        assert!(rx.try_recv().is_err());
        let n: i64 = store
            .call_blocking(|c| {
                Ok(c.query_row("SELECT count(*) FROM search_history", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(n, 2);
    }
}
