//! The relay pool, as Android's `friends/RelayPool.kt` (spec/CRITICAL.md §1.1): the same nine relays, every
//! publication to all of them and every subscription on all of them; a socket opens when something is to be sent
//! and closes after 90 s with no subscription and nothing sent; at most 20 messages at once and then 8 a second
//! per relay; a dropped socket comes back after 10 s × 2^failures (up to 5 min) while subscriptions are open, and
//! sends them again. Incoming events are checked (id and signature) and each is handed over once. Relays that ask
//! for authentication (NIP-42, damus) get a signed kind-22242 event and the closed subscription again.

use crate::nostr::{Event, Keys};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

pub const RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://nostr.mom",
    "wss://relay.snort.social",
    // Not behind Cloudflare: they keep working where it is cut.
    "wss://offchain.pub",
    "wss://nostr.oxtr.dev",
    "wss://nostr-pub.wellorder.net",
    "wss://nostr.einundzwanzig.space",
];

const IDLE_CLOSE: Duration = Duration::from_secs(90);
const BURST: f64 = 20.0;
const PER_SECOND: f64 = 8.0;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SEEN_KEEP: usize = 4000;

/// What a subscription receives.
#[derive(Debug, Clone)]
pub enum SubEvent {
    Event(Event),
    /// One relay finished sending what it had stored (or could not be reached).
    Done,
}

struct SubState {
    filters: Vec<Value>,
    tx: mpsc::UnboundedSender<SubEvent>,
}

/// Where a relay's `OK` for a published event goes: (relay, accepted, reason).
type AckSender = mpsc::UnboundedSender<(String, bool, String)>;

struct Shared {
    subs: Mutex<HashMap<String, SubState>>,
    acks: Mutex<HashMap<String, AckSender>>,
    seen: Mutex<(HashSet<String>, VecDeque<String>)>,
    signer: Mutex<Option<Keys>>,
    counter: Mutex<u64>,
}

/// One relay's outgoing queue (its task owns the socket).
struct RelayHandle {
    tx: mpsc::UnboundedSender<String>,
}

/// The relay pool. Cheap to clone; sockets live in tasks on the given runtime.
#[derive(Clone)]
pub struct Pool {
    shared: Arc<Shared>,
    relays: Arc<Vec<RelayHandle>>,
}

impl Pool {
    pub fn new(handle: &tokio::runtime::Handle) -> Pool {
        Self::with_relays(handle, RELAYS)
    }

    pub fn with_relays(handle: &tokio::runtime::Handle, urls: &[&str]) -> Pool {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let shared = Arc::new(Shared {
            subs: Mutex::default(),
            acks: Mutex::default(),
            seen: Mutex::default(),
            signer: Mutex::default(),
            counter: Mutex::new(0),
        });
        let relays = urls
            .iter()
            .map(|url| {
                let (tx, rx) = mpsc::unbounded_channel();
                handle.spawn(relay_task(url.to_string(), rx, Arc::downgrade(&shared)));
                RelayHandle { tx }
            })
            .collect();
        Pool {
            shared,
            relays: Arc::new(relays),
        }
    }

    /// The identity that answers relays' authentication requests (NIP-42).
    pub fn set_signer(&self, keys: Option<Keys>) {
        *self.shared.signer.lock().expect("signer") = keys;
    }

    fn broadcast(&self, msg: String) {
        for r in self.relays.iter() {
            let _ = r.tx.send(msg.clone());
        }
    }

    /// Publishes to every relay; resolves with how many accepted it within `wait` (0 = none or no answer).
    pub async fn publish(&self, event: &Event, wait: Duration) -> usize {
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.shared.acks.lock().expect("acks").insert(event.id.clone(), tx);
        self.broadcast(json!(["EVENT", event]).to_string());
        let mut ok = 0;
        let deadline = tokio::time::Instant::now() + wait;
        let mut answers = 0;
        while answers < self.relays.len() {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some((_, accepted, _))) => {
                    answers += 1;
                    if accepted {
                        ok += 1;
                        // One acceptance is enough to be sure it is out; the rest keep coming in the background.
                        break;
                    }
                }
                _ => break,
            }
        }
        let shared = self.shared.clone();
        let id = event.id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            shared.acks.lock().expect("acks").remove(&id);
        });
        ok
    }

    /// Opens a subscription on every relay. Dropping the returned value closes it.
    pub fn subscribe(&self, filters: Vec<Value>) -> Subscription {
        let id = {
            let mut c = self.shared.counter.lock().expect("counter");
            *c += 1;
            format!("drxw{:x}", *c)
        };
        let (tx, rx) = mpsc::unbounded_channel();
        self.shared.subs.lock().expect("subs").insert(
            id.clone(),
            SubState {
                filters: filters.clone(),
                tx,
            },
        );
        let mut req = vec![json!("REQ"), json!(id)];
        req.extend(filters);
        self.broadcast(Value::Array(req).to_string());
        Subscription {
            id,
            rx,
            pool: self.clone(),
            relays: self.relays.len(),
        }
    }

    fn close(&self, id: &str) {
        if self.shared.subs.lock().expect("subs").remove(id).is_some() {
            self.broadcast(json!(["CLOSE", id]).to_string());
        }
    }

    /// Stored events matching `filters`: everything until every relay has finished or `wait` has passed.
    pub async fn query(&self, filters: Vec<Value>, wait: Duration) -> Vec<Event> {
        let mut sub = self.subscribe(filters);
        let mut out = Vec::new();
        let mut done = 0;
        let deadline = tokio::time::Instant::now() + wait;
        while done < sub.relays {
            match tokio::time::timeout_at(deadline, sub.rx.recv()).await {
                Ok(Some(SubEvent::Event(e))) => out.push(e),
                Ok(Some(SubEvent::Done)) => done += 1,
                _ => break,
            }
        }
        out
    }
}

/// An open subscription: events arrive on [`Subscription::next`]; dropping it closes it on the relays.
pub struct Subscription {
    pub id: String,
    rx: mpsc::UnboundedReceiver<SubEvent>,
    pool: Pool,
    relays: usize,
}

impl Subscription {
    pub async fn next(&mut self) -> Option<SubEvent> {
        self.rx.recv().await
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.pool.close(&self.id);
    }
}

/// The pacing of one relay (BURST at once, then PER_SECOND).
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn wait(&mut self) -> Duration {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * PER_SECOND).min(BURST);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / PER_SECOND)
        }
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn relay_task(url: String, mut rx: mpsc::UnboundedReceiver<String>, shared: Weak<Shared>) {
    let mut failures: u32 = 0;
    let mut queue: VecDeque<String> = VecDeque::new();
    loop {
        // Idle: wait for something to send (or, with subscriptions open after a drop, retry by itself).
        if queue.is_empty() {
            match rx.recv().await {
                Some(m) => queue.push_back(m),
                None => return,
            }
        }
        let Some(sh) = shared.upgrade() else { return };
        // A CLOSE for a socket that is not open needs no connection.
        queue.retain(|m| !m.starts_with("[\"CLOSE\""));
        if queue.is_empty() {
            continue;
        }
        let socket = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str())).await {
            Ok(Ok((s, _))) => s,
            _ => {
                failures += 1;
                tracing::debug!("relay {url}: connect failed ({failures})");
                // Every open subscription hears this relay is done (it has nothing to give now).
                notify_done(&sh, &queue);
                drop(sh);
                let wait = Duration::from_millis((10_000u64 << failures.min(5)).min(300_000));
                // Drop what was waiting except subscriptions (they are sent again on the next connect).
                queue.clear();
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    m = rx.recv() => match m { Some(m) => queue.push_back(m), None => return },
                }
                if let Some(sh) = shared.upgrade()
                    && sh.subs.lock().expect("subs").is_empty()
                    && queue.is_empty()
                {
                    continue;
                }
                continue;
            }
        };
        failures = 0;
        // Open subscriptions go first (a reconnect asks again for what they want).
        let reqs: Vec<String> = sh
            .subs
            .lock()
            .expect("subs")
            .iter()
            .map(|(id, s)| {
                let mut v = vec![json!("REQ"), json!(id)];
                v.extend(s.filters.clone());
                Value::Array(v).to_string()
            })
            .collect();
        queue.retain(|m| !m.starts_with("[\"REQ\""));
        for r in reqs.into_iter().rev() {
            queue.push_front(r);
        }
        drop(sh);
        let res = run_socket(&url, socket, &mut rx, &mut queue, &shared).await;
        if res.is_err() {
            return; // the pool is gone
        }
    }
}

/// Tells every subscription in `queue` (being sent to a relay that could not be reached) that it is done.
fn notify_done(sh: &Shared, queue: &VecDeque<String>) {
    let subs = sh.subs.lock().expect("subs");
    for m in queue {
        if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(m)
            && a.first().and_then(Value::as_str) == Some("REQ")
            && let Some(s) = a.get(1).and_then(Value::as_str).and_then(|id| subs.get(id))
        {
            let _ = s.tx.send(SubEvent::Done);
        }
    }
}

/// Runs one open socket until it drops or goes idle. Err = the pool is gone.
async fn run_socket(
    url: &str,
    socket: Socket,
    rx: &mut mpsc::UnboundedReceiver<String>,
    queue: &mut VecDeque<String>,
    shared: &Weak<Shared>,
) -> Result<(), ()> {
    let (mut write, mut read) = socket.split();
    let mut bucket = Bucket { tokens: BURST, at: Instant::now() };
    let mut last_activity = Instant::now();
    let mut waiting_auth: Vec<String> = Vec::new();
    let mut auth_id: Option<String> = None;
    loop {
        // Send what is queued, paced.
        while let Some(m) = queue.front() {
            let wait = bucket.wait();
            if !wait.is_zero() {
                break;
            }
            if write.send(Message::Text(m.clone().into())).await.is_err() {
                return Ok(());
            }
            queue.pop_front();
            last_activity = Instant::now();
        }
        let pace = if queue.is_empty() { Duration::from_secs(5) } else { Duration::from_millis(130) };
        tokio::select! {
            m = rx.recv() => match m {
                Some(m) => queue.push_back(m),
                None => return Err(()),
            },
            msg = read.next() => {
                let Some(Ok(msg)) = msg else { return Ok(()) };
                let Message::Text(text) = msg else { continue };
                let Some(sh) = shared.upgrade() else { return Err(()) };
                handle_message(url, text.as_str(), &sh, queue, &mut waiting_auth, &mut auth_id);
            }
            _ = tokio::time::sleep(pace) => {
                let Some(sh) = shared.upgrade() else { return Err(()) };
                let idle = sh.subs.lock().expect("subs").is_empty() && queue.is_empty();
                if idle && last_activity.elapsed() >= IDLE_CLOSE {
                    let _ = write.send(Message::Close(None)).await;
                    return Ok(());
                }
            }
        }
    }
}

fn handle_message(
    url: &str,
    text: &str,
    sh: &Shared,
    queue: &mut VecDeque<String>,
    waiting_auth: &mut Vec<String>,
    auth_id: &mut Option<String>,
) {
    let Ok(Value::Array(a)) = serde_json::from_str::<Value>(text) else { return };
    match a.first().and_then(Value::as_str) {
        Some("EVENT") => {
            let (Some(sub), Some(ev)) = (a.get(1).and_then(Value::as_str), a.get(2)) else { return };
            let Ok(ev) = serde_json::from_value::<Event>(ev.clone()) else { return };
            {
                let mut seen = sh.seen.lock().expect("seen");
                if seen.0.contains(&ev.id) {
                    return;
                }
                if !ev.is_valid() {
                    return;
                }
                seen.0.insert(ev.id.clone());
                seen.1.push_back(ev.id.clone());
                if seen.1.len() > SEEN_KEEP
                    && let Some(old) = seen.1.pop_front()
                {
                    seen.0.remove(&old);
                }
            }
            if let Some(s) = sh.subs.lock().expect("subs").get(sub) {
                let _ = s.tx.send(SubEvent::Event(ev));
            }
        }
        Some("EOSE") => {
            if let Some(s) = a.get(1).and_then(Value::as_str).and_then(|id| sh.subs.lock().expect("subs").get(id).map(|s| s.tx.clone())) {
                let _ = s.send(SubEvent::Done);
            }
        }
        Some("OK") => {
            let id = a.get(1).and_then(Value::as_str).unwrap_or("");
            let ok = a.get(2).and_then(Value::as_bool).unwrap_or(false);
            let reason = a.get(3).and_then(Value::as_str).unwrap_or("").to_string();
            if auth_id.as_deref() == Some(id) {
                // Authenticated: the subscriptions it closed go again.
                if ok {
                    let subs = sh.subs.lock().expect("subs");
                    for sid in waiting_auth.drain(..) {
                        if let Some(s) = subs.get(&sid) {
                            let mut v = vec![json!("REQ"), json!(sid)];
                            v.extend(s.filters.clone());
                            queue.push_back(Value::Array(v).to_string());
                        }
                    }
                }
                return;
            }
            if let Some(tx) = sh.acks.lock().expect("acks").get(id) {
                let _ = tx.send((url.to_string(), ok, reason));
            }
        }
        Some("AUTH") => {
            let Some(challenge) = a.get(1).and_then(Value::as_str) else { return };
            let Some(keys) = sh.signer.lock().expect("signer").clone() else { return };
            let ev = keys.sign(
                22242,
                vec![
                    vec!["relay".into(), url.to_string()],
                    vec!["challenge".into(), challenge.to_string()],
                ],
                String::new(),
            );
            *auth_id = Some(ev.id.clone());
            queue.push_front(json!(["AUTH", ev]).to_string());
        }
        Some("CLOSED") => {
            let sid = a.get(1).and_then(Value::as_str).unwrap_or("").to_string();
            let reason = a.get(2).and_then(Value::as_str).unwrap_or("");
            if reason.starts_with("auth-required") {
                waiting_auth.push(sid);
            } else if let Some(s) = sh.subs.lock().expect("subs").get(&sid) {
                let _ = s.tx.send(SubEvent::Done);
            }
        }
        Some("NOTICE") => {
            let note = a.get(1).and_then(|v| v.as_str()).unwrap_or("");
            tracing::debug!("relay {url}: {note}");
        }
        _ => {}
    }
}

/// Waits for a value from a oneshot with a deadline (helper for callers).
pub async fn within<T>(rx: oneshot::Receiver<T>, wait: Duration) -> Option<T> {
    tokio::time::timeout(wait, rx).await.ok().and_then(Result::ok)
}
