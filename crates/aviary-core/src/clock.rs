//! Time as a service's rules read it (Deyao, 2026-11-25: CI is deterministic). Production runs on the real clock; a
//! test holds a manual one and moves it with [`Clock::advance`], so a cache that expires, a cooldown, a poller that
//! lingers, a periodic pass or a hold happens exactly when the test says, never after a sleep that a slow runner can
//! outlast.
//!
//! Only the rules go through the clock: how long an answer stays fresh, when a pass is due, how long something is held
//! or lingers. A wait on real I/O (a statement's deadline, an HTTP timeout, a connect) stays on tokio's real time: it
//! only stops something that hangs.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::sync::Notify;
use tokio::time::Instant;

/// A service's clock: cheap to clone, every clone the same clock.
#[derive(Clone)]
pub struct Clock(Arc<Kind>);

enum Kind {
    Real,
    Manual {
        start: Instant,
        elapsed: Mutex<Duration>,
        moved: Notify,
    },
}

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.0 {
            Kind::Real => f.write_str("Clock::Real"),
            Kind::Manual { elapsed, .. } => write!(f, "Clock::Manual(+{:?})", *lock(elapsed)),
        }
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::real()
    }
}

fn lock(m: &Mutex<Duration>) -> std::sync::MutexGuard<'_, Duration> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Clock {
    /// The real clock (production).
    #[must_use]
    pub fn real() -> Self {
        Self(Arc::new(Kind::Real))
    }

    /// A clock whose instants stand still until [`Clock::advance`] moves them (tests). Its wall-clock time is the
    /// real one moved forward by the same advances: tokens and timestamps come from issuers on real time, so a rule
    /// on a timestamp sees them as they are, and a test moves it past an expiry with [`Clock::advance`].
    #[must_use]
    pub fn manual() -> Self {
        Self(Arc::new(Kind::Manual {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
            moved: Notify::new(),
        }))
    }

    /// Whether this is a manual clock.
    #[must_use]
    pub fn is_manual(&self) -> bool {
        matches!(&*self.0, Kind::Manual { .. })
    }

    /// Now, as an instant.
    #[must_use]
    pub fn now(&self) -> Instant {
        match &*self.0 {
            Kind::Real => Instant::now(),
            Kind::Manual { start, elapsed, .. } => *start + *lock(elapsed),
        }
    }

    /// Now, as wall-clock time (for rules on timestamps, e.g. a token's expiry): on a manual clock the real wall
    /// clock plus every advance so far.
    #[must_use]
    pub fn system_now(&self) -> SystemTime {
        match &*self.0 {
            Kind::Real => SystemTime::now(),
            Kind::Manual { elapsed, .. } => SystemTime::now() + *lock(elapsed),
        }
    }

    /// The time since `since` (zero when `since` is later).
    #[must_use]
    pub fn elapsed(&self, since: Instant) -> Duration {
        self.now().saturating_duration_since(since)
    }

    /// Waits until the clock reads `at`.
    pub async fn sleep_until(&self, at: Instant) {
        match &*self.0 {
            Kind::Real => tokio::time::sleep_until(at).await,
            Kind::Manual { moved, .. } => loop {
                let woken = moved.notified();
                tokio::pin!(woken);
                woken.as_mut().enable();
                if self.now() >= at {
                    return;
                }
                woken.await;
            },
        }
    }

    /// Waits `d` of this clock's time.
    pub async fn sleep(&self, d: Duration) {
        let at = self.now() + d;
        self.sleep_until(at).await;
    }

    /// Moves a manual clock forward by `d`, waking everything whose time has come.
    ///
    /// # Panics
    /// On the real clock: production time is not moved.
    pub fn advance(&self, d: Duration) {
        match &*self.0 {
            Kind::Real => panic!("Clock::advance on the real clock"),
            Kind::Manual { elapsed, moved, .. } => {
                *lock(elapsed) += d;
                moved.notify_waiters();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Clock;
    use std::time::Duration;

    #[tokio::test]
    async fn a_manual_clock_moves_only_when_advanced_and_wakes_what_is_due() {
        let c = Clock::manual();
        let t0 = c.now();
        let w0 = c.system_now();
        tokio::task::yield_now().await;
        assert_eq!(c.now(), t0, "it stands still");
        let a = tokio::spawn({
            let c = c.clone();
            async move { c.sleep(Duration::from_secs(10)).await }
        });
        let b = tokio::spawn({
            let c = c.clone();
            async move { c.sleep(Duration::from_secs(30)).await }
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!a.is_finished() && !b.is_finished());
        c.advance(Duration::from_secs(10));
        a.await.unwrap();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!b.is_finished(), "not due yet");
        c.advance(Duration::from_secs(20));
        b.await.unwrap();
        assert_eq!(c.elapsed(t0), Duration::from_secs(30));
        assert!(c.system_now().duration_since(w0).unwrap() >= Duration::from_secs(30));
    }

    #[tokio::test]
    async fn a_sleep_already_due_returns_at_once() {
        let c = Clock::manual();
        let at = c.now();
        c.advance(Duration::from_secs(1));
        c.sleep_until(at).await;
        c.sleep(Duration::ZERO).await;
    }

    #[test]
    #[should_panic(expected = "real clock")]
    fn the_real_clock_is_never_moved() {
        Clock::real().advance(Duration::from_secs(1));
    }
}
