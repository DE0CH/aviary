//! Fault injection: latency, bursts of 503, holding requests until released, and dropping the connection without
//! an answer. A fault applies to one route (by its name or its path template) or to every route, for a number of
//! requests or until removed. Faults run before the route's handler, so a held or dropped request changed nothing.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aviary_core::http::ApiError;
use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tokio::sync::{Notify, watch};

use crate::serve::ConnKill;

/// What a fault does to a request it matches.
#[derive(Debug, Clone)]
pub enum FaultKind {
    /// Wait this long, then serve the request normally.
    Latency(Duration),
    /// Answer 503 `unavailable` with `retry-after: secs`, without serving the request.
    Unavailable {
        /// The `retry-after` header value.
        retry_after_secs: u64,
    },
    /// Wait until the [`Hold`] is released, then serve the request normally.
    Hold(Hold),
    /// Close the connection without answering (the client sees a connection reset or an incomplete message).
    Drop,
}

/// A gate holding requests until released.
#[derive(Clone, Debug)]
pub struct Hold {
    inner: Arc<HoldInner>,
}

#[derive(Debug)]
struct HoldInner {
    open: watch::Sender<bool>,
    waiting: AtomicUsize,
    changed: Notify,
}

impl Default for Hold {
    fn default() -> Self {
        Self::new()
    }
}

impl Hold {
    /// A closed gate.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HoldInner {
                open: watch::channel(false).0,
                waiting: AtomicUsize::new(0),
                changed: Notify::new(),
            }),
        }
    }

    /// Let every held request (and every later one) through.
    pub fn release(&self) {
        self.inner.open.send_replace(true);
    }

    /// How many requests are waiting at the gate now.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.inner.waiting.load(Ordering::SeqCst)
    }

    /// Wait until at least `n` requests are waiting at the gate, or `timeout` passes; whether they are.
    pub async fn wait_held(&self, n: usize, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = self.inner.changed.notified();
            if self.waiting() >= n {
                return true;
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return self.waiting() >= n;
            }
        }
    }

    async fn pass(&self) {
        let mut rx = self.inner.open.subscribe();
        self.inner.waiting.fetch_add(1, Ordering::SeqCst);
        self.inner.changed.notify_waiters();
        let _ = rx.wait_for(|open| *open).await;
        self.inner.waiting.fetch_sub(1, Ordering::SeqCst);
        self.inner.changed.notify_waiters();
    }
}

/// The handle of one installed fault, to remove it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultId(u64);

#[derive(Debug)]
struct Rule {
    id: FaultId,
    route: Option<String>,
    method: Option<Method>,
    kind: FaultKind,
    remaining: Option<u32>,
}

/// The faults installed on one fake.
#[derive(Debug, Default)]
pub struct Faults {
    rules: Mutex<Vec<Rule>>,
    next: AtomicU64,
    hits: AtomicU64,
}

impl Faults {
    fn lock(&self) -> MutexGuard<'_, Vec<Rule>> {
        self.rules.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn add(&self, route: Option<&str>, method: Option<Method>, kind: FaultKind, times: Option<u32>) -> FaultId {
        let id = FaultId(self.next.fetch_add(1, Ordering::SeqCst));
        self.lock().push(Rule {
            id,
            route: route.map(str::to_owned),
            method,
            kind,
            remaining: times,
        });
        id
    }

    /// `kind` on every request to `route` (a route name or a path template) until removed.
    pub fn on(&self, route: &str, kind: FaultKind) -> FaultId {
        self.add(Some(route), None, kind, None)
    }

    /// `kind` on the next `times` requests to `route`.
    pub fn times(&self, route: &str, kind: FaultKind, times: u32) -> FaultId {
        self.add(Some(route), None, kind, Some(times))
    }

    /// `kind` on the next `times` requests to `route` with `method` (a path template serving several methods).
    pub fn times_method(&self, route: &str, method: Method, kind: FaultKind, times: u32) -> FaultId {
        self.add(Some(route), Some(method), kind, Some(times))
    }

    /// `kind` on every request to every route until removed (an outage).
    pub fn everywhere(&self, kind: FaultKind) -> FaultId {
        self.add(None, None, kind, None)
    }

    /// `kind` on the next `times` requests to any route.
    pub fn next(&self, kind: FaultKind, times: u32) -> FaultId {
        self.add(None, None, kind, Some(times))
    }

    /// Remove one fault.
    pub fn remove(&self, id: FaultId) {
        self.lock().retain(|r| r.id != id);
    }

    /// Remove every fault.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// How many requests a fault has acted on so far.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::SeqCst)
    }

    fn pick(&self, method: &Method, name: Option<&str>, template: &str) -> Option<FaultKind> {
        let mut rules = self.lock();
        let idx = rules.iter().position(|r| {
            let route_ok = r.route.as_deref().is_none_or(|w| Some(w) == name || w == template);
            let method_ok = r.method.as_ref().is_none_or(|m| m == method);
            route_ok && method_ok && r.remaining != Some(0)
        })?;
        let rule = &mut rules[idx];
        let kind = rule.kind.clone();
        if let Some(n) = rule.remaining.as_mut() {
            *n -= 1;
            if *n == 0 {
                rules.remove(idx);
            }
        }
        drop(rules);
        self.hits.fetch_add(1, Ordering::SeqCst);
        Some(kind)
    }
}

/// Maps a matched method and path template to the route's name.
pub type Names = Arc<dyn Fn(&Method, &str) -> Option<&'static str> + Send + Sync>;

#[derive(Clone)]
struct Layer {
    faults: Arc<Faults>,
    names: Names,
}

async fn apply(State(l): State<Layer>, req: Request, next: Next) -> Response {
    let template = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_default();
    let name = (l.names)(req.method(), &template);
    match l.faults.pick(req.method(), name, &template) {
        None => next.run(req).await,
        Some(FaultKind::Latency(d)) => {
            tokio::time::sleep(d).await;
            next.run(req).await
        }
        Some(FaultKind::Unavailable { retry_after_secs }) => {
            ApiError::unavailable("fake: injected unavailability", retry_after_secs).into_response()
        }
        Some(FaultKind::Hold(h)) => {
            h.pass().await;
            next.run(req).await
        }
        Some(FaultKind::Drop) => {
            if let Some(kill) = req.extensions().get::<ConnKill>() {
                kill.kill();
            }
            // never answer: the connection task drops the connection
            std::future::pending::<Response>().await
        }
    }
}

/// `router` with `faults` applied to every route it has (after routing, so the route's template is known).
pub fn layer(router: Router, faults: Arc<Faults>, names: Names) -> Router {
    router.route_layer(axum::middleware::from_fn_with_state(Layer { faults, names }, apply))
}
