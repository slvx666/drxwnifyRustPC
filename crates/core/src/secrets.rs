//! Account secrets kept out of the plain settings files.
//!
//! On Android every session sits in plain text in `settings.preferences_pb` and the Nostr key in `friends.xml`.
//! On the PC they move into `secrets.dat`, encrypted for the current Windows user with DPAPI. The settings file then
//! holds no secrets. When a backup is created, the secrets are put back so the phone can restore it as before.
//!
//! Linux: Secret Service comes with the Linux milestone; until then the file is plain with 0600 permissions.

use crate::error::{Result, malformed};
use crate::keys;
use crate::paths::{Profile, write_atomic};
use crate::prefs::{PrefValue, Prefs};
use crate::shared_prefs::{SharedPrefs, SpValue};
use std::collections::BTreeMap;

/// DataStore keys whose values are secrets. Names as in `PreferenceKeys.kt`.
pub const SECRET_PREF_KEYS: [&str; 10] = [
    keys::spotify::SP_DC,
    keys::spotify::SP_KEY,
    keys::spotify::ACCESS_TOKEN,
    keys::yandex::ACCESS_TOKEN,
    keys::vk::ACCESS_TOKEN,
    keys::youtube::COOKIE,
    "discordToken",
    "listenTogetherSessionToken",
    "lastfmSession",
    "soulseekPassword",
];

/// SharedPreferences (file, key) pairs whose values are secrets.
pub const SECRET_SP_KEYS: [(&str, &str); 1] = [(keys::sp::FRIENDS, keys::sp::FRIENDS_SECRET_KEY)];

const FILE: &str = "secrets.dat";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Vault {
    values: BTreeMap<String, String>,
}

fn pref_slot(key: &str) -> String {
    format!("pref:{key}")
}
fn sp_slot(file: &str, key: &str) -> String {
    format!("sp:{file}:{key}")
}

impl Vault {
    pub fn load(profile: &Profile) -> Result<Self> {
        let path = profile.root().join(FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let plain = unprotect(&std::fs::read(path)?)?;
        let values =
            serde_json::from_slice(&plain).map_err(|e| malformed("secrets", e.to_string()))?;
        Ok(Self { values })
    }

    pub fn save(&self, profile: &Profile) -> Result<()> {
        let plain =
            serde_json::to_vec(&self.values).map_err(|e| malformed("secrets", e.to_string()))?;
        let path = profile.root().join(FILE);
        write_atomic(&path, &protect(&plain)?)?;
        restrict_permissions(&path);
        Ok(())
    }

    pub fn pref(&self, key: &str) -> Option<&str> {
        self.values.get(&pref_slot(key)).map(String::as_str)
    }
    pub fn set_pref(&mut self, key: &str, value: impl Into<String>) {
        self.values.insert(pref_slot(key), value.into());
    }
    pub fn remove_pref(&mut self, key: &str) {
        self.values.remove(&pref_slot(key));
    }
    pub fn shared(&self, file: &str, key: &str) -> Option<&str> {
        self.values.get(&sp_slot(file, key)).map(String::as_str)
    }

    /// Moves secret values out of `prefs` into the vault.
    pub fn take_from_prefs(&mut self, prefs: &mut Prefs) {
        for k in SECRET_PREF_KEYS {
            if let Some(PrefValue::String(v)) = prefs.remove(k) {
                self.set_pref(k, v);
            }
        }
    }
    /// Moves secret values out of one SharedPreferences file into the vault.
    pub fn take_from_shared(&mut self, file: &str, sp: &mut SharedPrefs) {
        for (f, k) in SECRET_SP_KEYS {
            if f == file
                && let Some(SpValue::String(v)) = sp.values.remove(k)
            {
                self.values.insert(sp_slot(f, k), v);
            }
        }
    }
    /// Puts the secrets back into `prefs` (for a backup the phone can restore).
    pub fn restore_into_prefs(&self, prefs: &mut Prefs) {
        for k in SECRET_PREF_KEYS {
            if let Some(v) = self.pref(k) {
                prefs.set(k, PrefValue::String(v.to_string()));
            }
        }
    }
    pub fn restore_into_shared(&self, file: &str, sp: &mut SharedPrefs) {
        for (f, k) in SECRET_SP_KEYS {
            if f == file
                && let Some(v) = self.shared(f, k)
            {
                sp.values
                    .insert(k.to_string(), SpValue::String(v.to_string()));
            }
        }
    }
}

#[cfg(windows)]
mod dpapi {
    use crate::error::{CoreError, Result};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI returned a LocalAlloc'd buffer of cbData bytes; copy it, then free it once.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as _);
            v
        }
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: all pointers are valid for the call; optional arguments are null.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(CoreError::Io(std::io::Error::last_os_error()));
        }
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as above.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(CoreError::Io(std::io::Error::last_os_error()));
        }
        Ok(take(out))
    }
}

#[cfg(windows)]
use dpapi::{protect, unprotect};

#[cfg(not(windows))]
fn protect(data: &[u8]) -> Result<Vec<u8>> {
    Ok(data.to_vec())
}
#[cfg(not(windows))]
fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
    Ok(data.to_vec())
}

/// Encrypts bytes for the current user (DPAPI on Windows). Used for internal snapshots that contain secrets.
pub fn protect_bytes(data: &[u8]) -> Result<Vec<u8>> {
    protect(data)
}

/// Reverses [`protect_bytes`].
pub fn unprotect_bytes(data: &[u8]) -> Result<Vec<u8>> {
    unprotect(data)
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn restrict_permissions(_: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_secrets_and_back() {
        let mut prefs = Prefs::default();
        prefs.set(keys::spotify::SP_DC, PrefValue::String("dc".into()));
        prefs.set("theme", PrefValue::String("dark".into()));
        let mut friends = SharedPrefs::default();
        friends
            .values
            .insert("sk".into(), SpValue::String("00aa".into()));
        friends
            .values
            .insert("name".into(), SpValue::String("me".into()));

        let mut v = Vault::default();
        v.take_from_prefs(&mut prefs);
        v.take_from_shared("friends", &mut friends);
        assert!(prefs.get(keys::spotify::SP_DC).is_none() && prefs.get("theme").is_some());
        assert!(!friends.values.contains_key("sk"));

        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::at(dir.path());
        v.save(&profile).unwrap();
        let raw = std::fs::read(dir.path().join(FILE)).unwrap();
        #[cfg(windows)]
        assert!(
            !raw.windows(4).any(|w| w == b"00aa"),
            "secrets must not be stored in plain text"
        );
        let _ = raw;
        let loaded = Vault::load(&profile).unwrap();
        assert_eq!(loaded, v);

        loaded.restore_into_prefs(&mut prefs);
        loaded.restore_into_shared("friends", &mut friends);
        assert_eq!(prefs.get_str(keys::spotify::SP_DC), Some("dc"));
        assert_eq!(friends.get_str("sk"), Some("00aa"));
    }
}
