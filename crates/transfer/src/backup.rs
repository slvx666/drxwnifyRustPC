//! Backups in the Android format (`spec/CRITICAL.md` 4.1, `android/.../viewmodels/BackupRestoreViewModel.kt`).
//!
//! A backup is a ZIP with `settings.preferences_pb`, `song.db` (Room v40, WAL merged), `shared_prefs/<name>.xml`
//! and `covers/<file>`. The PC profile has the same layout ([`drx_core::paths::Profile`]), so:
//! - importing = check the database version, take a snapshot of the current profile, copy the files in;
//! - creating = checkpoint the database and zip the files. The phone can restore it.
//!
//! What import changes on purpose:
//! - YouTube session keys are dropped (decision D-012), the user signs in to YouTube on the PC again;
//! - chat cursors (`chat.xml`, `cursor_*`) are removed so chat history is fetched again from the relays
//!   (`chat.db` is not in backups; decision D-011).
//!
//! Callers must close the profile's database before importing.

use drx_core::db::{self, SchemaState};
use drx_core::keys;
use drx_core::paths::{Profile, write_atomic};
use drx_core::prefs::Prefs;
use drx_core::shared_prefs::SharedPrefs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("not a Drxwnify backup: {0}")]
    NotABackup(String),
    #[error("the backup is from an older app version (database {0}); importing old backups is not supported yet")]
    TooOld(i64),
    #[error("the backup is from a newer app version (database {0}); update Drxwnify for PC")]
    TooNew(i64),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Core(#[from] drx_core::CoreError),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, BackupError>;

/// Which accounts a backup (or profile) holds. Names only, never secrets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Accounts {
    pub spotify: Option<String>,
    pub yandex: Option<String>,
    pub vk: Option<String>,
    pub youtube: Option<String>,
    pub friends_key: bool,
}

#[derive(Debug, Clone)]
pub struct BackupPreview {
    pub songs: i64,
    pub playlists: i64,
    pub history_events: i64,
    pub accounts: Accounts,
}

#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub snapshot: Option<PathBuf>,
    pub shared_prefs_files: usize,
    pub covers: usize,
    pub youtube_session_dropped: bool,
    pub chat_cursors_reset: usize,
    pub accounts: Accounts,
}

/// SharedPreferences files Android never puts in a backup, and WebView files that mean nothing on the PC.
fn skip_shared_prefs(name: &str) -> bool {
    name.starts_with("WebView") || name.starts_with("com.google") || name.starts_with("AwOriginVisitLogger")
}

/// A safe relative path inside the archive, or None for anything with `..`, roots or drive letters.
fn safe_entry(name: &str) -> Option<&str> {
    let ok = !name.is_empty()
        && !name.starts_with('/')
        && !name.starts_with('\\')
        && !name.contains(':')
        && !name.split(['/', '\\']).any(|p| p == ".." || p == ".");
    ok.then_some(name)
}

fn non_empty(p: &Prefs, key: &str) -> bool {
    p.get_str(key).is_some_and(|s| !s.is_empty())
}

fn accounts_from(prefs: &Prefs, friends: Option<&SharedPrefs>) -> Accounts {
    let name = |present: bool, name_key: &str, fallback: &str| -> Option<String> {
        present.then(|| prefs.get_str(name_key).filter(|s| !s.is_empty()).unwrap_or(fallback).to_string())
    };
    Accounts {
        spotify: name(non_empty(prefs, keys::spotify::SP_DC), keys::spotify::USERNAME, "Spotify"),
        yandex: name(non_empty(prefs, keys::yandex::ACCESS_TOKEN), keys::yandex::USERNAME, "Yandex"),
        vk: name(non_empty(prefs, keys::vk::ACCESS_TOKEN), keys::vk::USER_ID, "VK"),
        youtube: name(non_empty(prefs, keys::youtube::COOKIE), keys::youtube::ACCOUNT_NAME, "YouTube"),
        friends_key: friends
            .and_then(|f| f.get_str(keys::sp::FRIENDS_SECRET_KEY))
            .is_some_and(|k| !k.is_empty()),
    }
}

struct Archive {
    zip: zip::ZipArchive<File>,
}

impl Archive {
    fn open(path: &Path) -> Result<Self> {
        let zip = zip::ZipArchive::new(File::open(path)?).map_err(|e| BackupError::NotABackup(e.to_string()))?;
        Ok(Self { zip })
    }
    fn names(&self) -> Vec<String> {
        self.zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect()
    }
    fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        let mut f = match self.zip.by_name(name) {
            Ok(f) => f,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        Ok(Some(buf))
    }
    fn extract_to(&mut self, name: &str, dest: &Path) -> Result<bool> {
        let mut f = match self.zip.by_name(name) {
            Ok(f) => f,
            Err(zip::result::ZipError::FileNotFound) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = File::create(dest)?;
        std::io::copy(&mut f, &mut out)?;
        Ok(true)
    }
}

fn check_db(path: &Path) -> Result<(i64, i64, i64)> {
    let (conn, state) = db::open_read_only(path)?;
    match state {
        SchemaState::Current => {}
        SchemaState::Older(v) => return Err(BackupError::TooOld(v)),
        SchemaState::Newer(v) => return Err(BackupError::TooNew(v)),
        SchemaState::Created | SchemaState::Foreign => {
            return Err(BackupError::NotABackup("song.db is not a Drxwnify database".into()));
        }
    }
    let count = |t: &str| conn.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get::<_, i64>(0));
    Ok((count("song")?, count("playlist")?, count("event")?))
}

/// Reads a backup without changing anything: database version, counts and which accounts it carries.
pub fn preview(backup: &Path, scratch: &Path) -> Result<BackupPreview> {
    let mut a = Archive::open(backup)?;
    let db_path = scratch.join("preview-song.db");
    if !a.extract_to("song.db", &db_path)? {
        return Err(BackupError::NotABackup("no song.db inside".into()));
    }
    let counts = check_db(&db_path);
    let _ = std::fs::remove_file(&db_path);
    let (songs, playlists, history_events) = counts?;
    let prefs = match a.read("settings.preferences_pb")? {
        Some(b) => Prefs::decode(&b)?,
        None => Prefs::default(),
    };
    let friends = match a.read("shared_prefs/friends.xml")? {
        Some(b) => Some(SharedPrefs::parse(&String::from_utf8_lossy(&b))?),
        None => None,
    };
    Ok(BackupPreview { songs, playlists, history_events, accounts: accounts_from(&prefs, friends.as_ref()) })
}

/// Options for [`import_android_backup`].
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Keep the YouTube session from the backup. Off by default (D-012).
    pub keep_youtube_session: bool,
}

/// Replaces the profile with the backup's contents. Takes a snapshot of the current profile first.
pub fn import_android_backup(backup: &Path, profile: &Profile, opts: &ImportOptions) -> Result<ImportReport> {
    profile.ensure_dirs()?;
    let mut a = Archive::open(backup)?;
    let names = a.names();
    if !names.iter().any(|n| n == "song.db") {
        return Err(BackupError::NotABackup("no song.db inside".into()));
    }

    // 1. Database: extract next to the profile and check it before touching anything.
    let staging = profile.root().join(".import");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let staged_db = staging.join("song.db");
    a.extract_to("song.db", &staged_db)?;
    check_db(&staged_db)?;

    // 2. Snapshot of the current profile, if there is one.
    let mut report = ImportReport::default();
    if profile.song_db().exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let snap = profile.snapshots_dir().join(format!("before-import-{stamp}.backup"));
        create_backup(profile, &snap)?;
        report.snapshot = Some(snap);
    }

    // 3. Settings.
    let mut prefs = match a.read("settings.preferences_pb")? {
        Some(b) => Prefs::decode(&b)?,
        None => Prefs::default(),
    };
    if !opts.keep_youtube_session {
        for k in keys::youtube::SESSION_KEYS {
            report.youtube_session_dropped |= prefs.remove(k).is_some();
        }
    }

    // 4. SharedPreferences: parse all first, so a broken file aborts before anything is replaced.
    let mut sp_files: Vec<(String, SharedPrefs)> = Vec::new();
    for n in &names {
        let Some(rel) = safe_entry(n) else { continue };
        let Some(file) = rel.strip_prefix("shared_prefs/") else { continue };
        let Some(name) = file.strip_suffix(".xml") else { continue };
        if name.contains('/') || skip_shared_prefs(name) {
            continue;
        }
        let bytes = a.read(n)?.unwrap_or_default();
        let mut sp = SharedPrefs::parse(&String::from_utf8_lossy(&bytes))?;
        if name == keys::sp::CHAT {
            let before = sp.values.len();
            sp.values.retain(|k, _| !k.starts_with(keys::sp::CHAT_CURSOR_PREFIX));
            report.chat_cursors_reset = before - sp.values.len();
        }
        sp_files.push((name.to_string(), sp));
    }
    let friends = sp_files.iter().find(|(n, _)| n == keys::sp::FRIENDS).map(|(_, s)| s);
    report.accounts = accounts_from(&prefs, friends);

    // 5. Replace. The database goes last-but-one so a failure before it leaves the old data in place.
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(profile.root().join(format!("song.db{ext}")));
    }
    if profile.song_db().exists() {
        std::fs::remove_file(profile.song_db())?;
    }
    std::fs::rename(&staged_db, profile.song_db())?;
    write_atomic(&profile.prefs_pb(), &prefs.encode())?;

    for e in std::fs::read_dir(profile.shared_prefs_dir())? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "xml") {
            std::fs::remove_file(p)?;
        }
    }
    for (name, sp) in &sp_files {
        write_atomic(&profile.shared_prefs(name), sp.to_xml().as_bytes())?;
    }
    report.shared_prefs_files = sp_files.len();

    for e in std::fs::read_dir(profile.covers_dir())? {
        let p = e?.path();
        if p.is_file() {
            std::fs::remove_file(p)?;
        }
    }
    for n in &names {
        let Some(rel) = safe_entry(n) else { continue };
        let Some(file) = rel.strip_prefix("covers/") else { continue };
        if file.is_empty() || file.contains('/') {
            continue;
        }
        if a.extract_to(n, &profile.covers_dir().join(file))? {
            report.covers += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    tracing::info!(
        shared_prefs = report.shared_prefs_files,
        covers = report.covers,
        cursors_reset = report.chat_cursors_reset,
        "backup imported"
    );
    Ok(report)
}

/// Writes the profile as an Android-compatible backup (SYS-200). The phone can restore it.
pub fn create_backup(profile: &Profile, out: &Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Merge the WAL into the main file, as Android does with checkpoint() before zipping.
    if profile.song_db().exists() {
        let conn = rusqlite::Connection::open(profile.song_db())?;
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    }
    let tmp = out.with_extension("partial");
    {
        let mut zw = zip::ZipWriter::new(File::create(&tmp)?);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut add = |name: &str, path: &Path| -> Result<()> {
            if path.is_file() {
                zw.start_file(name, opts)?;
                let mut f = File::open(path)?;
                std::io::copy(&mut f, &mut zw)?;
            }
            Ok(())
        };
        add("settings.preferences_pb", &profile.prefs_pb())?;
        add("song.db", &profile.song_db())?;
        for dir in ["shared_prefs", "covers"] {
            let d = profile.root().join(dir);
            if !d.is_dir() {
                continue;
            }
            let mut files: Vec<_> = std::fs::read_dir(&d)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
            files.sort();
            for p in files {
                if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                    if n.starts_with('.') {
                        continue;
                    }
                    add(&format!("{dir}/{n}"), &p)?;
                }
            }
        }
        zw.finish()?.flush()?;
    }
    std::fs::rename(&tmp, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        assert!(safe_entry("../x").is_none());
        assert!(safe_entry("shared_prefs/../../x.xml").is_none());
        assert!(safe_entry("/etc/x").is_none());
        assert!(safe_entry("C:/x").is_none());
        assert_eq!(safe_entry("shared_prefs/chat.xml"), Some("shared_prefs/chat.xml"));
    }

    fn make_profile(dir: &Path) -> Profile {
        let p = Profile::at(dir);
        p.ensure_dirs().unwrap();
        let (conn, _) = db::open(&p.song_db()).unwrap();
        conn.execute("INSERT INTO playlist (id, name, createdAt, lastUpdateTime, isEditable, bookmarkedAt, isLocal, isAutoSync, browseId) VALUES ('p1','Test',0,0,1,NULL,1,0,NULL)", []).ok();
        drop(conn);
        let mut prefs = Prefs::default();
        prefs.set(keys::spotify::SP_DC, drx_core::prefs::PrefValue::String("secret".into()));
        prefs.set(keys::spotify::USERNAME, drx_core::prefs::PrefValue::String("me".into()));
        prefs.set(keys::youtube::COOKIE, drx_core::prefs::PrefValue::String("yt".into()));
        write_atomic(&p.prefs_pb(), &prefs.encode()).unwrap();
        let chat = "<map>\n<long name=\"cursor_abc\" value=\"5\" />\n<string name=\"other\">x</string>\n</map>";
        write_atomic(&p.shared_prefs("chat"), chat.as_bytes()).unwrap();
        write_atomic(&p.shared_prefs("friends"), b"<map><string name=\"sk\">00</string></map>").unwrap();
        p
    }

    #[test]
    fn backup_roundtrip_drops_youtube_and_cursors() {
        let src = tempfile::tempdir().unwrap();
        let profile = make_profile(src.path());
        let file = src.path().join("out.backup");
        create_backup(&profile, &file).unwrap();

        let scratch = tempfile::tempdir().unwrap();
        let pv = preview(&file, scratch.path()).unwrap();
        assert_eq!(pv.accounts.spotify.as_deref(), Some("me"));
        assert!(pv.accounts.youtube.is_some());
        assert!(pv.accounts.friends_key);

        let dst = tempfile::tempdir().unwrap();
        let target = Profile::at(dst.path());
        let r = import_android_backup(&file, &target, &ImportOptions::default()).unwrap();
        assert!(r.youtube_session_dropped);
        assert_eq!(r.chat_cursors_reset, 1);
        assert_eq!(r.shared_prefs_files, 2);
        let prefs = Prefs::decode(&std::fs::read(target.prefs_pb()).unwrap()).unwrap();
        assert!(prefs.get_str(keys::youtube::COOKIE).is_none());
        assert_eq!(prefs.get_str(keys::spotify::SP_DC), Some("secret"));
        let chat = SharedPrefs::parse(&std::fs::read_to_string(target.shared_prefs("chat")).unwrap()).unwrap();
        assert!(chat.values.contains_key("other") && !chat.values.contains_key("cursor_abc"));

        // A second import keeps a snapshot of what was there.
        let r2 = import_android_backup(&file, &target, &ImportOptions::default()).unwrap();
        assert!(r2.snapshot.unwrap().exists());
    }

    /// Imports the real backup, when available. Set DRX_TEST_BACKUP. Not run in CI.
    #[test]
    fn real_backup_import() {
        let Ok(path) = std::env::var("DRX_TEST_BACKUP") else { return };
        let dst = tempfile::tempdir().unwrap();
        let pv = preview(Path::new(&path), dst.path()).unwrap();
        assert!(pv.songs > 0);
        let target = Profile::at(dst.path().join("profile"));
        let r = import_android_backup(Path::new(&path), &target, &ImportOptions::default()).unwrap();
        assert!(r.shared_prefs_files > 10);
        let out = dst.path().join("again.backup");
        create_backup(&target, &out).unwrap();
        let pv2 = preview(&out, dst.path()).unwrap();
        assert_eq!(pv2.songs, pv.songs);
        assert_eq!(pv2.history_events, pv.history_events);
        eprintln!("songs={} playlists={} events={} covers={} cursors_reset={} accounts: spotify={} yandex={} vk={} youtube_dropped={} friends_key={}",
            pv.songs, pv.playlists, pv.history_events, r.covers, r.chat_cursors_reset,
            r.accounts.spotify.is_some(), r.accounts.yandex.is_some(), r.accounts.vk.is_some(), r.youtube_session_dropped, r.accounts.friends_key);
    }
}
