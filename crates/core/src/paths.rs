//! Where the app keeps its data. The layout mirrors an Android backup so backups go both ways by copying files:
//! ```text
//! <root>/song.db                    music database (Room v40)
//! <root>/settings.preferences_pb    settings (DataStore format)
//! <root>/shared_prefs/<name>.xml    Android SharedPreferences files (friends key, chat cursors, ...)
//! <root>/covers/                    custom playlist covers
//! <root>/snapshots/                 automatic backups made before an import
//! <root>/cache/                     audio and image cache (never backed up)
//! ```
//! Full and Mini share one profile.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Profile {
    root: PathBuf,
}

impl Profile {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default profile: `%APPDATA%\Drxwnify` on Windows, `$XDG_DATA_HOME/drxwnify` (or `~/.local/share/drxwnify`) on Linux.
    pub fn default_location() -> Option<Self> {
        #[cfg(windows)]
        {
            std::env::var_os("APPDATA").map(|a| Self::at(PathBuf::from(a).join("Drxwnify")))
        }
        #[cfg(not(windows))]
        {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
                })?;
            Some(Self::at(base.join("drxwnify")))
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn song_db(&self) -> PathBuf {
        self.root.join("song.db")
    }
    pub fn prefs_pb(&self) -> PathBuf {
        self.root.join("settings.preferences_pb")
    }
    pub fn shared_prefs_dir(&self) -> PathBuf {
        self.root.join("shared_prefs")
    }
    pub fn shared_prefs(&self, name: &str) -> PathBuf {
        self.shared_prefs_dir().join(format!("{name}.xml"))
    }
    pub fn covers_dir(&self) -> PathBuf {
        self.root.join("covers")
    }
    pub fn snapshots_dir(&self) -> PathBuf {
        self.root.join("snapshots")
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for d in [
            self.root.clone(),
            self.shared_prefs_dir(),
            self.covers_dir(),
            self.snapshots_dir(),
            self.cache_dir(),
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

/// Writes a file atomically: temp file in the same folder, then rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
