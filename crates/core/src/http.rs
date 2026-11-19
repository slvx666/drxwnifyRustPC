//! The one HTTP client of the app.
//!
//! - TLS: rustls with the `ring` provider and the OS certificate store (rustls-platform-verifier), so certificates
//!   from user-installed antivirus or corporate roots work, as in a browser. One TLS stack for the whole app.
//! - Per-host pacing: a small token bucket per host so a catalog or a background job cannot flood one service.
//! - `429`/`503` with `Retry-After`: waits once if the wait is short, otherwise returns [`HttpError::RateLimited`]
//!   so the caller can back off (Spotify's `me/top` is never retried, see PORTING-2.3.0 §1).
//! - Cookies are passed explicitly per request by the catalog that owns the account (no shared cookie jar).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

pub use reqwest::{Method, RequestBuilder, StatusCode, header};

/// A desktop browser User-Agent for sites that refuse unknown clients.
pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),
    #[error("{host} is rate limiting us; retry after {retry_after:?}")]
    RateLimited {
        host: String,
        retry_after: Option<Duration>,
    },
    #[error("HTTP {status} from {url}")]
    Status {
        status: StatusCode,
        url: String,
        body: String,
    },
}

pub type HttpResult<T> = std::result::Result<T, HttpError>;

#[derive(Debug, Clone, Copy)]
pub struct Pace {
    /// Requests allowed at once.
    pub burst: u32,
    /// Requests refilled per second.
    pub per_second: f64,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            burst: 10,
            per_second: 5.0,
        }
    }
}

struct Bucket {
    tokens: f64,
    last: Instant,
    pace: Pace,
}

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    buckets: Arc<Mutex<HashMap<String, Bucket>>>,
    paces: Arc<Mutex<HashMap<String, Pace>>>,
    /// Longest `Retry-After` we wait for inside a call.
    max_inline_wait: Duration,
}

static RUSTLS_PROVIDER: Once = Once::new();

impl Http {
    pub fn new() -> HttpResult<Self> {
        RUSTLS_PROVIDER.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
        let client = reqwest::Client::builder()
            .user_agent(BROWSER_UA)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(8)
            .tcp_nodelay(true)
            .build()?;
        Ok(Self {
            client,
            buckets: Arc::default(),
            paces: Arc::default(),
            max_inline_wait: Duration::from_secs(5),
        })
    }

    /// Sets the pacing for one host (for example VK classification: 400 ms between requests).
    pub fn set_pace(&self, host: &str, pace: Pace) {
        self.paces
            .lock()
            .expect("pace lock")
            .insert(host.to_string(), pace);
    }

    /// The underlying client, for streaming downloads.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.client.request(method, url)
    }

    /// How long to wait before the next request to `host` is allowed; takes a token when it returns zero.
    fn reserve(&self, host: &str) -> Duration {
        let pace = self
            .paces
            .lock()
            .expect("pace lock")
            .get(host)
            .copied()
            .unwrap_or_default();
        let mut buckets = self.buckets.lock().expect("bucket lock");
        let b = buckets.entry(host.to_string()).or_insert_with(|| Bucket {
            tokens: pace.burst as f64,
            last: Instant::now(),
            pace,
        });
        b.pace = pace;
        let now = Instant::now();
        b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * pace.per_second)
            .min(pace.burst as f64);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - b.tokens) / pace.per_second.max(0.001))
        }
    }

    /// Sends a request with pacing and one short `Retry-After` wait. Non-2xx statuses become [`HttpError::Status`].
    pub async fn send(&self, req: reqwest::RequestBuilder) -> HttpResult<reqwest::Response> {
        let built = req.build()?;
        let host = built.url().host_str().unwrap_or_default().to_string();
        let mut attempt = 0;
        loop {
            loop {
                let wait = self.reserve(&host);
                if wait.is_zero() {
                    break;
                }
                tokio::time::sleep(wait).await;
            }
            let req = built
                .try_clone()
                .expect("request body must be clonable (no streams)");
            let resp = self.client.execute(req).await?;
            let status = resp.status();
            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
            {
                let retry_after = retry_after(resp.headers());
                match retry_after {
                    Some(d) if attempt == 0 && d <= self.max_inline_wait => {
                        attempt += 1;
                        tracing::debug!(%host, ?d, "rate limited, waiting once");
                        tokio::time::sleep(d).await;
                        continue;
                    }
                    _ => return Err(HttpError::RateLimited { host, retry_after }),
                }
            }
            if !status.is_success() {
                let url = redact_url(resp.url().as_str());
                let body = resp.text().await.unwrap_or_default();
                let body: String = body.chars().take(500).collect();
                return Err(HttpError::Status { status, url, body });
            }
            return Ok(resp);
        }
    }

    pub async fn get_text(&self, url: &str) -> HttpResult<String> {
        Ok(self
            .send(self.request(Method::GET, url))
            .await?
            .text()
            .await?)
    }

    pub async fn get_json(&self, url: &str) -> HttpResult<serde_json::Value> {
        Ok(self
            .send(self.request(Method::GET, url))
            .await?
            .json()
            .await?)
    }
}

fn retry_after(h: &header::HeaderMap) -> Option<Duration> {
    let v = h.get(header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Removes query values that may carry tokens before a URL goes into logs or errors.
pub fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        None => url.to_string(),
        Some((base, q)) => {
            let parts: Vec<String> = q
                .split('&')
                .map(|kv| match kv.split_once('=') {
                    Some((k, _)) => format!("{k}=…"),
                    None => kv.to_string(),
                })
                .collect();
            format!("{base}?{}", parts.join("&"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_query_values() {
        assert_eq!(
            redact_url("https://a.b/c?access_token=xyz&v=5"),
            "https://a.b/c?access_token=…&v=…"
        );
        assert_eq!(redact_url("https://a.b/c"), "https://a.b/c");
    }

    #[test]
    fn bucket_paces_requests() {
        let http = Http::new().unwrap();
        http.set_pace(
            "x",
            Pace {
                burst: 2,
                per_second: 1.0,
            },
        );
        assert!(http.reserve("x").is_zero());
        assert!(http.reserve("x").is_zero());
        let w = http.reserve("x");
        assert!(w > Duration::from_millis(500) && w <= Duration::from_secs(1));
    }

    #[test]
    fn retry_after_seconds() {
        let mut h = header::HeaderMap::new();
        h.insert(header::RETRY_AFTER, "8".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_secs(8)));
    }

    /// Real network: TLS with the OS store works. Set DRX_TEST_NET=1. Not run in CI.
    #[test]
    fn real_tls_request() {
        if std::env::var("DRX_TEST_NET").is_err() {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let http = Http::new().unwrap();
            let t = http
                .get_text("https://drxwnify.pages.dev/config.json")
                .await
                .unwrap();
            assert!(t.trim_start().starts_with('{'));
        });
    }
}
