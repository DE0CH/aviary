//! Retries with jittered backoff, only for calls that are safe to repeat (platform 3.9): an idempotent method
//! (`GET`, `HEAD`, `PUT`, `DELETE`, `OPTIONS`), or a call whose body carries the caller's idempotency key
//! ([`Idempotence::Keyed`]). Everything else is sent exactly once.

use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use ring::rand::{SecureRandom, SystemRandom};

/// Whether a call may be repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotence {
    /// Decided by the method: `GET`, `HEAD`, `PUT`, `DELETE` and `OPTIONS` are retried, others are not.
    ByMethod,
    /// The caller states the body carries an idempotency key the callee deduplicates on (a create's
    /// `idempotencyKey`, a turn's `clientMessageId`), so any method may be retried.
    Keyed,
}

impl Idempotence {
    fn allows(self, method: &Method) -> bool {
        match self {
            Self::Keyed => true,
            Self::ByMethod => matches!(
                *method,
                Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
            ),
        }
    }
}

/// The retry schedule: at most `attempts` sends; before retry *n* a random wait in `[0, min(max, base·2ⁿ⁻¹)]` (full
/// jitter), or the answer's `retry-after` when it is longer (capped at `max`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// Sends in total, the first included.
    pub attempts: u32,
    /// The first retry's upper bound.
    pub base: Duration,
    /// The upper bound of any wait.
    pub max: Duration,
}

impl Default for Backoff {
    /// 4 sends, waits up to 100 ms, 200 ms, 400 ms (each at most 5 s).
    fn default() -> Self {
        Self {
            attempts: 4,
            base: Duration::from_millis(100),
            max: Duration::from_secs(5),
        }
    }
}

impl Backoff {
    /// The longest [`send`] can take when each attempt is bounded by `per_attempt` (the call's own timeout): every
    /// attempt, plus a wait of at most `max` before each retry (a jittered wait, or `retry-after` capped at `max`).
    /// With the default schedule and a 10 s call timeout, 55 s. A caller with a total deadline picks `attempts` and
    /// its timeout so this stays inside it.
    #[must_use]
    pub fn worst_case(&self, per_attempt: Duration) -> Duration {
        let attempts = self.attempts.max(1);
        per_attempt
            .saturating_mul(attempts)
            .saturating_add(self.max.saturating_mul(attempts - 1))
    }

    /// The wait before retry `n` (1-based).
    #[must_use]
    pub fn delay(&self, n: u32) -> Duration {
        let shift = n.saturating_sub(1).min(20);
        let cap = self.base.saturating_mul(1 << shift).min(self.max);
        let mut b = [0u8; 8];
        let r = if SystemRandom::new().fill(&mut b).is_ok() {
            u64::from_le_bytes(b)
        } else {
            u64::MAX
        };
        let cap_ns = u64::try_from(cap.as_nanos()).unwrap_or(u64::MAX);
        Duration::from_nanos(if cap_ns == 0 { 0 } else { r % (cap_ns + 1) })
    }
}

/// Whether an answer is worth another try: 429, 502, 503, 504.
fn retryable_status(s: StatusCode) -> bool {
    matches!(s.as_u16(), 429 | 502 | 503 | 504)
}

/// Whether a transport error is worth another try: no connection, a timeout, or the connection failing before an
/// answer.
fn retryable_error(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout() || e.is_request()
}

fn retry_after(r: &reqwest::Response) -> Option<Duration> {
    let secs: u64 = r
        .headers()
        .get(header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs))
}

/// Sends `request`, retrying per `backoff` when [`Idempotence`] allows it and the failure is transient (a
/// connection error, a timeout, 429, 502, 503, 504). A request whose body cannot be cloned (a stream) is sent once.
/// Returns the last answer (which may be an error status) or the last transport error.
///
/// # Errors
/// The transport error of the last attempt, or a request that could not be built.
pub async fn send(
    request: reqwest::RequestBuilder,
    backoff: Backoff,
    idempotence: Idempotence,
) -> reqwest::Result<reqwest::Response> {
    let (client, request) = request.build_split();
    let request = request?;
    let attempts = if idempotence.allows(request.method()) && request.try_clone().is_some() {
        backoff.attempts.max(1)
    } else {
        1
    };
    let mut attempt = 1;
    loop {
        if attempt >= attempts {
            return client.execute(request).await;
        }
        let Some(copy) = request.try_clone() else {
            return client.execute(request).await;
        };
        match client.execute(copy).await {
            Ok(r) if !retryable_status(r.status()) => return Ok(r),
            Ok(r) => {
                let wait = backoff
                    .delay(attempt)
                    .max(retry_after(&r).unwrap_or_default())
                    .min(backoff.max);
                tracing::warn!(status = r.status().as_u16(), attempt, "call retried");
                tokio::time::sleep(wait).await;
            }
            Err(e) if !retryable_error(&e) => return Err(e),
            Err(e) => {
                tracing::warn!(error = %e, attempt, "call retried");
                tokio::time::sleep(backoff.delay(attempt)).await;
            }
        }
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::any;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    async fn failing(status: u16) -> (String, Arc<AtomicU32>) {
        let hits = Arc::new(AtomicU32::new(0));
        let h = hits.clone();
        let router = Router::new().route(
            "/x",
            any(move || {
                let h = h.clone();
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    StatusCode::from_u16(status).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{addr}/x"), hits)
    }

    fn quick() -> Backoff {
        Backoff {
            attempts: 3,
            base: Duration::from_millis(1),
            max: Duration::from_millis(5),
        }
    }

    fn client() -> reqwest::Client {
        crate::http::client_builder(&crate::version::Build::new("test"), None)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn idempotent_methods_are_retried() {
        let (url, hits) = failing(503).await;
        let r = send(client().get(&url), quick(), Idempotence::ByMethod).await.unwrap();
        assert_eq!(r.status(), 503);
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        let (url, hits) = failing(502).await;
        let _ = send(client().put(&url).body("x"), quick(), Idempotence::ByMethod)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        let (url, hits) = failing(503).await;
        let _ = send(client().delete(&url), quick(), Idempotence::ByMethod)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn post_is_sent_once() {
        let (url, hits) = failing(503).await;
        let r = send(client().post(&url).body("{}"), quick(), Idempotence::ByMethod)
            .await
            .unwrap();
        assert_eq!(r.status(), 503);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let (url, hits) = failing(503).await;
        let _ = send(client().patch(&url).body("{}"), quick(), Idempotence::ByMethod)
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn keyed_post_is_retried() {
        let (url, hits) = failing(503).await;
        let _ = send(
            client().post(&url).body(r#"{"idempotencyKey":"k"}"#),
            quick(),
            Idempotence::Keyed,
        )
        .await
        .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_transient_answers_are_not_retried() {
        for status in [400, 401, 404, 409, 500] {
            let (url, hits) = failing(status).await;
            let r = send(client().get(&url), quick(), Idempotence::ByMethod).await.unwrap();
            assert_eq!(r.status().as_u16(), status);
            assert_eq!(hits.load(Ordering::SeqCst), 1, "status {status}");
        }
    }

    #[tokio::test]
    async fn success_stops_retrying() {
        let (url, hits) = failing(200).await;
        let _ = send(client().get(&url), quick(), Idempotence::ByMethod).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn connection_errors_are_retried_only_when_idempotent() {
        // a port nothing listens on
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x", listener.local_addr().unwrap());
        drop(listener);
        let started = std::time::Instant::now();
        let e = send(client().get(&url), quick(), Idempotence::ByMethod)
            .await
            .unwrap_err();
        assert!(e.is_connect());
        let e = send(client().post(&url), quick(), Idempotence::ByMethod)
            .await
            .unwrap_err();
        assert!(e.is_connect());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn worst_case_counts_attempts_and_waits() {
        // 4 attempts of 10 s, and 3 waits each raised up to 5 s by retry-after
        assert_eq!(
            Backoff::default().worst_case(Duration::from_secs(10)),
            Duration::from_secs(55)
        );
        let once = Backoff {
            attempts: 1,
            ..Backoff::default()
        };
        assert_eq!(once.worst_case(Duration::from_secs(3)), Duration::from_secs(3));
    }

    #[test]
    fn delays_are_bounded() {
        let b = Backoff {
            attempts: 10,
            base: Duration::from_millis(100),
            max: Duration::from_millis(300),
        };
        for _ in 0..200 {
            assert!(b.delay(1) <= Duration::from_millis(100));
            assert!(b.delay(2) <= Duration::from_millis(200));
            assert!(b.delay(5) <= Duration::from_millis(300));
            assert!(b.delay(64) <= Duration::from_millis(300));
        }
    }
}
