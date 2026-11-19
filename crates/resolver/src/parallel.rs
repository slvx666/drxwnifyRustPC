//! Asks every ready provider at once and picks one (port of `ParallelAudioResolver.kt`, AUTO mode — the default).
//!
//! AUTO: an exact match (≥ 0.95) plays as soon as it is in, unless it is below 120 kbps — then a better-sounding
//! one gets 1.2 s more. A doubtful match never ends the race while a better-ranked source may still have the exact
//! track. With nothing yet, waiting goes on until all answer or 14 s pass. Every match seen is returned as an
//! alternate, best first, so a stream failure can switch provider without searching again.

use crate::provider::{AudioProvider, AudioQuery, ProviderId, ProviderMatch};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub const CONFIDENCE_MARGIN: f64 = 0.15;
pub const STRONG_MATCH: f64 = 0.85;
pub const AUTO_INSTANT_MATCH: f64 = 0.88;
pub const LOW_KBPS: u32 = 120;
pub const AUTO_LOW_GRACE: Duration = Duration::from_millis(800);
pub const AUTO_CONFIDENCE_MARGIN: f64 = 0.05;
pub const SOFT_DEADLINE: Duration = Duration::from_millis(1_200);
pub const HARD_DEADLINE: Duration = Duration::from_millis(6_000);

#[derive(Debug, Clone)]
pub struct Outcome {
    pub winner: Option<ProviderMatch>,
    /// Every match seen (winner included), best first.
    pub matches: Vec<ProviderMatch>,
    pub report: Vec<String>,
    pub elapsed: Duration,
}

/// Comparably confident matches in the user's order, then the weaker ones by confidence (SPEED order).
pub fn preferred(m: &[ProviderMatch], rank: &impl Fn(ProviderId) -> usize) -> Vec<ProviderMatch> {
    let Some(best) = m.iter().map(|x| x.confidence).reduce(f64::max) else {
        return Vec::new();
    };
    let (mut strong, mut weak): (Vec<_>, Vec<_>) = m
        .iter()
        .cloned()
        .partition(|x| x.confidence >= best - CONFIDENCE_MARGIN);
    strong.sort_by_key(|x| rank(x.provider));
    weak.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    strong.extend(weak);
    strong
}

/// The most exact matches first, best quality among them, the user's order breaking ties (AUTO order).
pub fn by_quality(
    m: &[ProviderMatch],
    rank: &impl Fn(ProviderId) -> usize,
    margin: f64,
) -> Vec<ProviderMatch> {
    let Some(best) = m.iter().map(|x| x.confidence).reduce(f64::max) else {
        return Vec::new();
    };
    let (mut exact, mut rest): (Vec<_>, Vec<_>) = m
        .iter()
        .cloned()
        .partition(|x| x.confidence >= best - margin);
    exact.sort_by(|a, b| {
        b.expected_kbps()
            .cmp(&a.expected_kbps())
            .then(rank(a.provider).cmp(&rank(b.provider)))
    });
    rest.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    exact.extend(rest);
    exact
}

enum Answer {
    Found(ProviderMatch, Duration),
    Miss(ProviderId, Duration),
    Failed(ProviderId, Duration, String),
}

pub async fn resolve(
    q: &AudioQuery,
    providers: &[Arc<dyn AudioProvider>],
    order: &[ProviderId],
) -> Outcome {
    let start = Instant::now();
    let rank = |id: ProviderId| {
        order
            .iter()
            .position(|o| *o == id)
            .unwrap_or(order.len() + id as usize)
    };
    let ready: Vec<Arc<dyn AudioProvider>> =
        providers.iter().filter(|p| p.is_ready()).cloned().collect();
    if ready.is_empty() {
        return Outcome {
            winner: None,
            matches: vec![],
            report: vec!["no source is ready".into()],
            elapsed: Duration::ZERO,
        };
    }
    let (tx, mut rx) = mpsc::channel::<Answer>(ready.len());
    let mut handles = Vec::new();
    for p in &ready {
        let (p, tx, q) = (p.clone(), tx.clone(), q.clone());
        handles.push(tokio::spawn(async move {
            let id = p.id();
            let a = match tokio::time::timeout(p.search_timeout(), p.search(&q)).await {
                Err(_) => Answer::Failed(
                    id,
                    start.elapsed(),
                    format!("timeout {:?}", p.search_timeout()),
                ),
                Ok(Ok(Some(m))) => Answer::Found(m, start.elapsed()),
                Ok(Ok(None)) => Answer::Miss(id, start.elapsed()),
                Ok(Err(e)) => Answer::Failed(id, start.elapsed(), e.0),
            };
            let _ = tx.send(a).await;
        }));
    }
    drop(tx);
    let mut pending: Vec<ProviderId> = ready.iter().map(|p| p.id()).collect();
    let mut found: Vec<ProviderMatch> = Vec::new();
    let mut report = Vec::new();
    let ordered = |f: &[ProviderMatch]| by_quality(f, &rank, AUTO_CONFIDENCE_MARGIN);

    let doubtful_while_better_pending = |best: &ProviderMatch, pending: &[ProviderId]| {
        best.confidence < STRONG_MATCH && pending.iter().any(|p| rank(*p) < rank(best.provider))
    };

    let decide_by_speed =
        |found: &[ProviderMatch], pending: &[ProviderId], now: Duration| -> Option<ProviderMatch> {
            let best = preferred(found, &rank).into_iter().next()?;
            let higher_pending = pending.iter().any(|p| rank(*p) < rank(best.provider));
            let could_be_beaten = best.confidence < STRONG_MATCH && !pending.is_empty();
            let deadline = now >= SOFT_DEADLINE && !doubtful_while_better_pending(&best, pending);
            ((!higher_pending && !could_be_beaten) || deadline).then_some(best)
        };

    let decide =
        |found: &[ProviderMatch], pending: &[ProviderId], now: Duration| -> Option<ProviderMatch> {
            if pending.is_empty() {
                return ordered(found).into_iter().next();
            }
            let best = ordered(found).into_iter().next()?;
            if best.confidence < AUTO_INSTANT_MATCH {
                return decide_by_speed(found, pending, now);
            }
            if best.expected_kbps() >= LOW_KBPS
                || now >= AUTO_LOW_GRACE
                || pending.iter().all(|p| p.typical_kbps() < LOW_KBPS)
            {
                return Some(best);
            }
            None
        };

    let wait_until = |found: &[ProviderMatch], pending: &[ProviderId]| -> Duration {
        let Some(best) = ordered(found).into_iter().next() else {
            return HARD_DEADLINE;
        };
        if doubtful_while_better_pending(&best, pending) {
            return HARD_DEADLINE;
        }
        if best.confidence >= AUTO_INSTANT_MATCH {
            AUTO_LOW_GRACE
        } else {
            SOFT_DEADLINE
        }
    };

    let mut winner = decide(&found, &pending, start.elapsed());
    while winner.is_none() && !pending.is_empty() {
        let until = wait_until(&found, &pending);
        let now = start.elapsed();
        if now >= until {
            winner = ordered(&found).into_iter().next();
            break;
        }
        let Ok(Some(a)) = tokio::time::timeout(until - now, rx.recv()).await else {
            continue;
        };
        match a {
            Answer::Found(m, t) => {
                report.push(format!(
                    "{} ✔ {}ms '{} – {}' ({:.2}, ~{} kbps)",
                    m.provider.name(),
                    t.as_millis(),
                    m.artist,
                    m.title,
                    m.confidence,
                    m.expected_kbps()
                ));
                pending.retain(|p| *p != m.provider);
                found.push(m);
            }
            Answer::Miss(id, t) => {
                report.push(format!("{} – no match {}ms", id.name(), t.as_millis()));
                pending.retain(|p| *p != id);
            }
            Answer::Failed(id, t, why) => {
                report.push(format!("{} ✘ {}ms {why}", id.name(), t.as_millis()));
                pending.retain(|p| *p != id);
            }
        }
        winner = decide(&found, &pending, start.elapsed());
    }
    if winner.is_none() {
        winner = ordered(&found).into_iter().next();
    }
    for p in &pending {
        report.push(format!("{} … not waited for", p.name()));
    }
    for h in handles {
        h.abort();
    }
    Outcome {
        winner,
        matches: ordered(&found),
        report,
        elapsed: start.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{AudioStream, ProviderError};

    struct Fake {
        id: ProviderId,
        delay: Duration,
        conf: Option<f64>,
    }

    #[async_trait::async_trait]
    impl AudioProvider for Fake {
        fn id(&self) -> ProviderId {
            self.id
        }
        fn search_timeout(&self) -> Duration {
            Duration::from_secs(5)
        }
        async fn search(&self, _q: &AudioQuery) -> Result<Option<ProviderMatch>, ProviderError> {
            tokio::time::sleep(self.delay).await;
            Ok(self.conf.map(|c| ProviderMatch {
                provider: self.id,
                track_id: "x".into(),
                title: "t".into(),
                artist: "a".into(),
                duration_ms: None,
                confidence: c,
                quality_kbps: None,
            }))
        }
        async fn stream(
            &self,
            _q: &AudioQuery,
            _m: &ProviderMatch,
        ) -> Result<Option<AudioStream>, ProviderError> {
            Ok(None)
        }
    }

    fn q() -> AudioQuery {
        AudioQuery {
            catalog_id: None,
            title: "t".into(),
            artists: vec!["a".into()],
            album: None,
            duration_ms: 0,
            isrc: None,
            excluded: vec![],
        }
    }

    #[tokio::test(start_paused = true)]
    async fn exact_match_plays_without_waiting_for_slower_sources() {
        let p: Vec<Arc<dyn AudioProvider>> = vec![
            Arc::new(Fake {
                id: ProviderId::Vk,
                delay: Duration::from_millis(100),
                conf: Some(0.99),
            }),
            Arc::new(Fake {
                id: ProviderId::Soundcloud,
                delay: Duration::from_secs(4),
                conf: Some(0.99),
            }),
        ];
        let o = resolve(&q(), &p, &crate::provider::DEFAULT_ORDER).await;
        assert_eq!(o.winner.unwrap().provider, ProviderId::Vk);
        assert!(o.elapsed < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn doubtful_match_waits_for_better_ranked_source() {
        // SoundCloud ranks above VK; VK's doubtful 0.7 must not end the race while SoundCloud searches.
        let p: Vec<Arc<dyn AudioProvider>> = vec![
            Arc::new(Fake {
                id: ProviderId::Vk,
                delay: Duration::from_millis(100),
                conf: Some(0.7),
            }),
            Arc::new(Fake {
                id: ProviderId::Soundcloud,
                delay: Duration::from_secs(4),
                conf: Some(0.98),
            }),
        ];
        let o = resolve(&q(), &p, &crate::provider::DEFAULT_ORDER).await;
        assert_eq!(o.winner.unwrap().provider, ProviderId::Soundcloud);
        assert_eq!(o.matches.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_found() {
        let p: Vec<Arc<dyn AudioProvider>> = vec![Arc::new(Fake {
            id: ProviderId::Vk,
            delay: Duration::from_millis(10),
            conf: None,
        })];
        assert!(
            resolve(&q(), &p, &crate::provider::DEFAULT_ORDER)
                .await
                .winner
                .is_none()
        );
    }
}
