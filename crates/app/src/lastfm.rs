//! Last.fm (Android LastFM / ScrobbleManager, INT-016): scrobbles, «now playing» and loved tracks for the listener's
//! account. The PC signs in on its own (desktop auth: a token approved in the browser, then a session key) — the
//! phone's session belongs to the phone's API key. The app's key and shared secret are not in the repository:
//! `DRX_LASTFM_KEY` / `DRX_LASTFM_SECRET` at build time (from the untracked local.env) or at run time.
//!
//! The phone's switches and rules: `lastfmScrobblingEnable`, `lastfmUseNowPlaying`, `lastfmUseSendLikes`; a track of
//! 30 s or more is scrobbled after half of it or 180 s, whichever comes first (`scrobbleDelayPercent`,
//! `scrobbleMinSongDuration`, `scrobbleDelaySeconds`).

use crate::App;
use drx_catalog::model::Track;
use md5::{Digest, Md5};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

const API: &str = "https://ws.audioscrobbler.com/2.0/";
const SESSION_KEY: &str = "pcLastfmSession";
const USER_KEY: &str = "pcLastfmUser";
pub const SCROBBLE_KEY: &str = "lastfmScrobblingEnable";
pub const NOW_PLAYING_KEY: &str = "lastfmUseNowPlaying";
pub const LOVE_KEY: &str = "lastfmUseSendLikes";

fn key() -> Option<String> {
    std::env::var("DRX_LASTFM_KEY").ok().or_else(|| option_env!("DRX_LASTFM_KEY").map(str::to_string)).filter(|k| !k.is_empty())
}

fn secret() -> Option<String> {
    std::env::var("DRX_LASTFM_SECRET").ok().or_else(|| option_env!("DRX_LASTFM_SECRET").map(str::to_string)).filter(|k| !k.is_empty())
}

/// The app can sign in (its key and secret are known).
pub fn available() -> bool {
    key().is_some() && secret().is_some()
}

/// `api_sig`: MD5 of the parameters sorted by name, each name then value, then the secret (format excluded).
pub fn signature(params: &BTreeMap<String, String>, secret: &str) -> String {
    let mut s = String::new();
    for (k, v) in params {
        if k != "format" && k != "callback" {
            s.push_str(k);
            s.push_str(v);
        }
    }
    s.push_str(secret);
    Md5::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Who is signed in.
#[derive(Debug, Clone, PartialEq)]
pub struct LastfmState {
    pub available: bool,
    pub user: Option<String>,
    pub scrobble: bool,
    pub now_playing: bool,
    pub love: bool,
}

impl App {
    async fn lastfm_call(&self, method: &str, mut params: BTreeMap<String, String>, post: bool) -> crate::AppResult<Value> {
        let (Some(k), Some(sec)) = (key(), secret()) else {
            return Err(crate::AppError::Other("нет ключа Last.fm (local.env)".into()));
        };
        params.insert("method".into(), method.into());
        params.insert("api_key".into(), k);
        let sig = signature(&params, &sec);
        params.insert("api_sig".into(), sig);
        params.insert("format".into(), "json".into());
        let http = &self.inner.http;
        let body: String = params
            .iter()
            .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let req = if post {
            http.request(drx_core::http::Method::POST, API)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body)
        } else {
            http.request(drx_core::http::Method::GET, &format!("{API}?{body}"))
        };
        let resp = http.send(req).await.map_err(|e| crate::AppError::Other(e.to_string()))?;
        let text = resp.text().await.map_err(|e| crate::AppError::Other(e.to_string()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| crate::AppError::Other(e.to_string()))?;
        if let Some(e) = v.get("error") {
            let msg = v.get("message").and_then(Value::as_str).unwrap_or("ошибка");
            return Err(crate::AppError::Other(format!("Last.fm {e}: {msg}")));
        }
        Ok(v)
    }

    pub fn lastfm_state(&self) -> LastfmState {
        LastfmState {
            available: available(),
            user: self.pref_string(USER_KEY).filter(|u| !u.is_empty() && self.pref_string(SESSION_KEY).is_some_and(|s| !s.is_empty())),
            scrobble: self.pref_bool(SCROBBLE_KEY).unwrap_or(false),
            now_playing: self.pref_bool(NOW_PLAYING_KEY).unwrap_or(false),
            love: self.pref_bool(LOVE_KEY).unwrap_or(false),
        }
    }

    /// Sign in, step one: a token and the Last.fm page where the listener approves it (the UI opens it).
    pub async fn lastfm_begin(&self) -> crate::AppResult<(String, String)> {
        let v = self.lastfm_call("auth.getToken", BTreeMap::new(), false).await?;
        let token = v.get("token").and_then(Value::as_str).ok_or_else(|| crate::AppError::Other("Last.fm не дал токен".into()))?.to_string();
        let url = format!("https://www.last.fm/api/auth/?api_key={}&token={token}", key().unwrap_or_default());
        Ok((token, url))
    }

    /// Step two: the session once the page was approved (asked every 3 s for 3 minutes). Returns the account name.
    pub async fn lastfm_finish(&self, token: &str) -> crate::AppResult<String> {
        let token = token.to_string();
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut p = BTreeMap::new();
            p.insert("token".to_string(), token.clone());
            if let Ok(v) = self.lastfm_call("auth.getSession", p, false).await {
                let s = v.pointer("/session/key").and_then(Value::as_str).unwrap_or_default().to_string();
                let name = v.pointer("/session/name").and_then(Value::as_str).unwrap_or_default().to_string();
                if !s.is_empty() {
                    let _ = self.set_pref(SESSION_KEY, drx_core::prefs::PrefValue::String(s));
                    let _ = self.set_pref(USER_KEY, drx_core::prefs::PrefValue::String(name.clone()));
                    // Signed in: scrobbling starts on, as on the phone after its login.
                    if self.pref_bool(SCROBBLE_KEY).is_none() {
                        let _ = self.set_pref(SCROBBLE_KEY, drx_core::prefs::PrefValue::Bool(true));
                    }
                    return Ok(name);
                }
            }
        }
        Err(crate::AppError::Other("вход не подтверждён в браузере".into()))
    }

    pub fn lastfm_sign_out(&self) {
        let _ = self.set_pref(SESSION_KEY, drx_core::prefs::PrefValue::String(String::new()));
        let _ = self.set_pref(USER_KEY, drx_core::prefs::PrefValue::String(String::new()));
    }

    fn lastfm_session(&self) -> Option<String> {
        self.pref_string(SESSION_KEY).filter(|s| !s.is_empty())
    }

    fn track_params(t: &Track) -> BTreeMap<String, String> {
        let mut p = BTreeMap::new();
        p.insert("artist".into(), t.artists.first().map(|a| a.name.clone()).unwrap_or_default());
        p.insert("track".into(), t.title.clone());
        if let Some(a) = t.album.as_ref().filter(|a| !a.name.is_empty()) {
            p.insert("album".into(), a.name.clone());
        }
        if t.duration_ms > 0 {
            p.insert("duration".into(), (t.duration_ms / 1000).to_string());
        }
        p
    }

    /// A like sent as «love» (or «unlove») when that switch is on.
    pub fn lastfm_love(&self, t: &Track, loved: bool) {
        let Some(sk) = self.lastfm_session() else { return };
        if !self.pref_bool(LOVE_KEY).unwrap_or(false) {
            return;
        }
        let mut p = BTreeMap::new();
        p.insert("artist".to_string(), t.artists.first().map(|a| a.name.clone()).unwrap_or_default());
        p.insert("track".to_string(), t.title.clone());
        p.insert("sk".to_string(), sk);
        let app = self.clone();
        self.runtime.spawn(async move {
            let m = if loved { "track.love" } else { "track.unlove" };
            if let Err(e) = app.lastfm_call(m, p, true).await {
                tracing::info!("{m}: {e}");
            }
        });
    }

    /// Follows the playback: «now playing» when a track starts, a scrobble once enough of it was heard.
    pub fn keep_scrobbling(&self) {
        let app = self.clone();
        self.runtime.spawn(async move {
            let mut current: Option<(Track, i64)> = None; // the track and when it started (Unix s)
            let mut heard = 0.0f64;
            let mut done = false;
            let mut last_tick = std::time::Instant::now();
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(sk) = app.lastfm_session() else {
                    current = None;
                    continue;
                };
                let np = app.now_playing().borrow().clone();
                let id = np.track.as_ref().map(|t| t.id.clone());
                if id != current.as_ref().map(|c| c.0.id.clone()) {
                    heard = 0.0;
                    done = false;
                    current = np.track.clone().map(|t| (t, crate::songs::now_ms() / 1000));
                    if let Some((t, _)) = &current
                        && np.playing
                        && app.pref_bool(NOW_PLAYING_KEY).unwrap_or(false)
                    {
                        let mut p = Self::track_params(t);
                        p.insert("sk".into(), sk.clone());
                        if let Err(e) = app.lastfm_call("track.updateNowPlaying", p, true).await {
                            tracing::info!("now playing: {e}");
                        }
                    }
                    last_tick = std::time::Instant::now();
                    continue;
                }
                let dt = last_tick.elapsed().as_secs_f64();
                last_tick = std::time::Instant::now();
                if np.playing {
                    heard += dt.min(2.0);
                }
                let Some((t, started)) = &current else { continue };
                if done || !app.pref_bool(SCROBBLE_KEY).unwrap_or(false) {
                    continue;
                }
                let dur = t.duration_ms as f64 / 1000.0;
                let percent = app.pref_f32("scrobbleDelayPercent").unwrap_or(0.5) as f64;
                let min_len = app.pref_i64("scrobbleMinSongDuration").unwrap_or(30) as f64;
                let delay = app.pref_i64("scrobbleDelaySeconds").unwrap_or(180) as f64;
                if dur < min_len || heard < (dur * percent).min(delay) {
                    continue;
                }
                done = true;
                let mut p = Self::track_params(t);
                p.insert("timestamp".into(), started.to_string());
                p.insert("sk".into(), sk);
                match app.lastfm_call("track.scrobble", p, true).await {
                    // Last.fm answers 200 with «ignored» for a scrobble it refused: say why.
                    Ok(v) if v.pointer("/scrobbles/@attr/ignored").and_then(Value::as_i64).unwrap_or(0) > 0 => {
                        let why = v.pointer("/scrobbles/scrobble/ignoredMessage/#text").and_then(Value::as_str).unwrap_or("отклонён").to_string();
                        let _ = app.inner.events.send(crate::AppEvent::Notice(format!("Last.fm не принял скроббл: {why}")));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let _ = app.inner.events.send(crate::AppEvent::Notice(format!("Скроббл не ушёл: {e}")));
                    }
                }
            }
        });
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_as_last_fm_describes() {
        let mut p = BTreeMap::new();
        p.insert("api_key".to_string(), "xxxxxxxx".to_string());
        p.insert("method".to_string(), "auth.getSession".to_string());
        p.insert("token".to_string(), "yyyyyy".to_string());
        p.insert("format".to_string(), "json".to_string());
        let want: String = Md5::digest(b"api_keyxxxxxxxxmethodauth.getSessiontokenyyyyyyilovecher").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(signature(&p, "ilovecher"), want);
    }
}
