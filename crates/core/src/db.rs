//! The music database. Its schema is **exactly** the Android Room database `song.db`, version 40
//! (`android/app/schemas/com.metrolist.music.db.InternalDatabase/40.json`, copied to `schema/room-40.json`), so that:
//! - an Android backup is imported by opening its `song.db` as is;
//! - a PC backup can be restored on the phone.
//!
//! Data that exists only on the PC goes into tables prefixed `pc_`; Room ignores unknown tables.

use crate::error::{Result, malformed};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::path::Path;

/// Room schema version this build reads and writes.
pub const ROOM_VERSION: i64 = 40;
/// Room identity hash of version 40; Android checks it on open.
pub const ROOM_IDENTITY_HASH: &str = "05d9c338b1e9133ed535e9f15a20221d";

const SCHEMA_JSON: &str = include_str!("../schema/room-40.json");

#[derive(Deserialize)]
struct SchemaFile {
    database: SchemaDb,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaDb {
    version: i64,
    identity_hash: String,
    entities: Vec<Entity>,
    views: Vec<View>,
    setup_queries: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entity {
    table_name: String,
    create_sql: String,
    #[serde(default)]
    indices: Vec<Index>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    create_sql: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct View {
    view_name: String,
    create_sql: String,
}

/// All statements that create an empty version-40 database, in order.
pub fn schema_statements() -> Result<Vec<String>> {
    let file: SchemaFile =
        serde_json::from_str(SCHEMA_JSON).map_err(|e| malformed("room schema", e.to_string()))?;
    let db = file.database;
    if db.version != ROOM_VERSION || db.identity_hash != ROOM_IDENTITY_HASH {
        return Err(malformed(
            "room schema",
            "embedded schema does not match ROOM_VERSION / hash",
        ));
    }
    let mut out = Vec::new();
    for e in &db.entities {
        out.push(e.create_sql.replace("${TABLE_NAME}", &e.table_name));
        for i in &e.indices {
            out.push(i.create_sql.replace("${TABLE_NAME}", &e.table_name));
        }
    }
    for v in &db.views {
        out.push(v.create_sql.replace("${VIEW_NAME}", &v.view_name));
    }
    out.extend(db.setup_queries.iter().cloned());
    Ok(out)
}

/// What an opened database file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaState {
    /// Empty file; the schema was just created.
    Created,
    /// Version 40 with the right identity hash.
    Current,
    /// Older Room version (an old backup). Needs migration before use.
    Older(i64),
    /// Newer Room version (a backup from a newer Android build). Refuse with a clear message.
    Newer(i64),
    /// Version 40 but a different identity hash: not a Drxwnify database or a broken one.
    Foreign,
}

/// Reads the Room version and identity hash of a connection.
pub fn inspect(conn: &Connection) -> Result<SchemaState> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let has_tables: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%')",
        [],
        |r| r.get(0),
    )?;
    if version == 0 && !has_tables {
        return Ok(SchemaState::Created);
    }
    if version < ROOM_VERSION {
        return Ok(SchemaState::Older(version));
    }
    if version > ROOM_VERSION {
        return Ok(SchemaState::Newer(version));
    }
    let hash: Option<String> = conn
        .query_row(
            "SELECT identity_hash FROM room_master_table WHERE id = 42",
            [],
            |r| r.get(0),
        )
        .ok();
    Ok(if hash.as_deref() == Some(ROOM_IDENTITY_HASH) {
        SchemaState::Current
    } else {
        SchemaState::Foreign
    })
}

/// Settings for the single writer connection. Small cache on purpose (Mini memory budget).
fn tune(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA cache_size = -1024;
         PRAGMA mmap_size = 0;
         PRAGMA temp_store = MEMORY;",
    )?;
    Ok(())
}

/// Opens (or creates) the music database at `path`.
/// Returns the connection and what was found. Callers must not use an `Older`/`Newer`/`Foreign` database.
pub fn open(path: &Path) -> Result<(Connection, SchemaState)> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let state = inspect(&conn)?;
    if state == SchemaState::Created {
        create_schema(&conn)?;
    }
    if matches!(state, SchemaState::Created | SchemaState::Current) {
        tune(&conn)?;
    }
    Ok((conn, state))
}

/// Opens a database read-only without changing anything (for previewing a backup).
pub fn open_read_only(path: &Path) -> Result<(Connection, SchemaState)> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let state = inspect(&conn)?;
    Ok((conn, state))
}

fn create_schema(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for sql in schema_statements()? {
        tx.execute_batch(&sql)?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {ROOM_VERSION};"))?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_version_40() {
        let dir = tempfile::tempdir().unwrap();
        let (conn, state) = open(&dir.path().join("song.db")).unwrap();
        assert_eq!(state, SchemaState::Created);
        assert_eq!(inspect(&conn).unwrap(), SchemaState::Current);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name != 'room_master_table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 21);
        drop(conn);
        let (_c, again) = open(&dir.path().join("song.db")).unwrap();
        assert_eq!(again, SchemaState::Current);
    }

    /// Compares our schema with a real Android backup's `song.db`, when one is available locally.
    /// Set DRX_TEST_SONG_DB to an extracted song.db. Not run in CI (no private data there).
    #[test]
    fn matches_real_android_db() {
        let Ok(path) = std::env::var("DRX_TEST_SONG_DB") else {
            return;
        };
        let (real, state) = open_read_only(Path::new(&path)).unwrap();
        assert_eq!(state, SchemaState::Current);
        let dir = tempfile::tempdir().unwrap();
        let (ours, _) = open(&dir.path().join("song.db")).unwrap();
        let q = "SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND name != 'android_metadata' ORDER BY type, name";
        let dump = |c: &Connection| -> Vec<(String, String, Option<String>)> {
            let mut s = c.prepare(q).unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(|x| x.unwrap())
                .collect()
        };
        let norm = |v: Vec<(String, String, Option<String>)>| -> Vec<(String, String, String)> {
            v.into_iter()
                .map(|(t, n, s)| {
                    (
                        t,
                        n,
                        s.unwrap_or_default()
                            .replace("CREATE TABLE IF NOT EXISTS", "CREATE TABLE")
                            .replace("CREATE INDEX IF NOT EXISTS", "CREATE INDEX")
                            .replace("CREATE UNIQUE INDEX IF NOT EXISTS", "CREATE UNIQUE INDEX"),
                    )
                })
                .collect()
        };
        assert_eq!(norm(dump(&ours)), norm(dump(&real)));
    }
}
