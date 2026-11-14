//! Web-player tokens from the `sp_dc` cookie (port of `spotify/SpotifyAuth.kt`).
//!
//! `/api/token` needs a TOTP (RFC 6238, HMAC-SHA1, 30 s, 6 digits) from a secret Spotify rotates; the current secret
//! and its version come from a community gist (the highest `v` wins), the time from `/api/server-time`.
//!
//! Important (spec risk from the review): Android wipes the session when the token answer says "anonymous" or
//! "expired" (SpotifyTokenManager.kt). Here a rejected cookie is only *reported* ([`AuthError::SessionRejected`]);
//! the app decides what to do (ask to sign in again), so an imported session is never deleted by a hiccup.

use drx_core::http::{Http, HttpError, Method};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha1::Sha1;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TOKEN_URL: &str = "https://open.spotify.com/api/token";
const SERVER_TIME_URL: &str = "https://open.spotify.com/api/server-time";
const NUANCE_GIST_URL: &str = "https://api.github.com/gists/22ed9c6ba463899e933427f7de1f0eef";

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("Spotify did not accept the saved session (sp_dc): sign in again")]
    SessionRejected,
    #[error("unexpected answer: {0}")]
    Bad(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    pub access_token: String,
    pub access_token_expiration_timestamp_ms: i64,
    #[serde(default)]
    pub is_anonymous: bool,
    #[serde(default)]
    pub client_id: String,
}

impl Token {
    /// Still good for at least a minute.
    pub fn fresh(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        self.access_token_expiration_timestamp_ms - now > 60_000
    }
}

#[derive(Deserialize)]
struct Nuance {
    s: String,
    v: i64,
}

pub fn base32_decode(input: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut buf, mut bits) = (0u32, 0u32);
    let mut out = Vec::new();
    for c in input.to_ascii_uppercase().bytes().filter(|&c| c != b'=') {
        let Some(v) = A.iter().position(|&a| a == c) else {
            continue;
        };
        buf = (buf << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

/// RFC 6238 TOTP: HMAC-SHA1, 30-second steps, 6 digits.
pub fn totp(secret_base32: &str, unix_secs: u64) -> String {
    let key = base32_decode(secret_base32);
    let step = unix_secs / 30;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).expect("HMAC accepts any key length");
    mac.update(&step.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[h.len() - 1] & 0x0F) as usize;
    let code = ((h[o] as u32 & 0x7F) << 24)
        | ((h[o + 1] as u32) << 16)
        | ((h[o + 2] as u32) << 8)
        | h[o + 3] as u32;
    format!("{:06}", code % 1_000_000)
}

async fn nuance(http: &Http) -> Result<Nuance, AuthError> {
    let gist = http.get_json(NUANCE_GIST_URL).await?;
    let content = gist
        .get("files")
        .and_then(|f| f.as_object())
        .and_then(|f| f.values().next())
        .and_then(|f| f.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| AuthError::Bad("gist has no files".into()))?;
    let list: Vec<Nuance> =
        serde_json::from_str(content).map_err(|e| AuthError::Bad(e.to_string()))?;
    list.into_iter()
        .max_by_key(|n| n.v)
        .ok_or_else(|| AuthError::Bad("no TOTP secret in gist".into()))
}

async fn server_time(http: &Http) -> Result<u64, AuthError> {
    let v = http.get_json(SERVER_TIME_URL).await?;
    v.get("serverTime")
        .and_then(|t| t.as_u64())
        .ok_or_else(|| AuthError::Bad("no serverTime".into()))
}

/// Gets a token: for the account when `sp_dc` is given, otherwise an anonymous one (public catalog only).
pub async fn fetch_token(
    http: &Http,
    sp_dc: Option<&str>,
    sp_key: Option<&str>,
) -> Result<Token, AuthError> {
    let n = nuance(http).await?;
    let t = server_time(http).await?;
    let code = totp(&n.s, t);
    let reason = if sp_dc.is_some() { "transport" } else { "init" };
    let url = format!(
        "{TOKEN_URL}?reason={reason}&productType=web-player&totp={code}&totpServer={code}&totpVer={}",
        n.v
    );
    let mut req = http
        .request(Method::GET, &url)
        .header("Accept", "application/json, text/plain, */*")
        .header("Accept-Language", "en")
        .timeout(Duration::from_secs(15));
    if let Some(dc) = sp_dc {
        let mut cookie = format!("sp_dc={dc}");
        if let Some(k) = sp_key.filter(|k| !k.is_empty()) {
            cookie.push_str(&format!("; sp_key={k}"));
        }
        req = req.header("Cookie", cookie);
    }
    let resp = http.send(req).await?;
    let token: Token = resp
        .json()
        .await
        .map_err(|e| AuthError::Bad(e.to_string()))?;
    if token.access_token.is_empty() || (sp_dc.is_some() && token.is_anonymous) {
        return Err(AuthError::SessionRejected);
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_matches_rfc6238_sha1_vectors() {
        // RFC 6238 appendix B, SHA1 secret "12345678901234567890" (base32 below), 8-digit codes; ours keeps 6.
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        assert_eq!(totp(secret, 59), "287082");
        assert_eq!(totp(secret, 1_111_111_109), "081804");
        assert_eq!(totp(secret, 1_234_567_890), "005924");
        assert_eq!(totp(secret, 2_000_000_000), "279037");
    }

    #[test]
    fn base32_roundtrip_known() {
        assert_eq!(base32_decode("MZXW6YTBOI======"), b"foobar");
    }
}
