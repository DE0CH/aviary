//! The revocation view of one sandbox at a gateway (3.11.2 step 3, hermit 6.3): hermit's liveness answer, cached
//! for [`TTL`](crate::limits::LIVENESS_TTL) and refreshed in the background; a hermit problem (refused, timed out,
//! 5xx) keeps the last answer whatever its age, and with no answer yet the request is admitted on the strength of
//! the verified token. **A hermit problem never turns a valid token into 401, 403, 407 or 5xx.**

use std::time::{Duration, Instant};

use crate::TokenLiveness;
use crate::limits::LIVENESS_TTL;

/// What one lookup of `GET /v1/sandbox-tokens/{sandboxId}` came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// 200.
    Answer(TokenLiveness),
    /// 404 `unknown_sandbox`: definitive, the same as `live: false`.
    Unknown,
    /// Refused, timed out, 5xx or anything else: not an answer.
    Failed,
}

/// The cached view of one sandbox id.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// The last definitive answer and when it came.
    last: Option<(bool, Instant)>,
}

impl View {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a lookup's outcome; a failure changes nothing.
    pub fn update(&mut self, lookup: &Lookup, now: Instant) {
        match lookup {
            Lookup::Answer(a) => self.last = Some((a.live, now)),
            Lookup::Unknown => self.last = Some((false, now)),
            Lookup::Failed => {}
        }
    }

    /// Whether a request whose token verified is admitted now.
    #[must_use]
    pub fn admits(&self) -> bool {
        self.last.is_none_or(|(live, _)| live)
    }

    /// Whether a background refresh is due (no answer yet, or the last one older than the TTL).
    #[must_use]
    pub fn refresh_due(&self, now: Instant) -> bool {
        self.last
            .is_none_or(|(_, at)| now.saturating_duration_since(at) >= LIVENESS_TTL)
    }

    /// The age of the last definitive answer.
    #[must_use]
    pub fn age(&self, now: Instant) -> Option<Duration> {
        self.last.map(|(_, at)| now.saturating_duration_since(at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermit_api::sandbox::SandboxState;

    fn answer(live: bool) -> Lookup {
        Lookup::Answer(TokenLiveness {
            sandbox_id: uuid::Uuid::from_u128(1),
            conversation_id: uuid::Uuid::from_u128(2),
            person: "staff@example.org".into(),
            state: if live { SandboxState::Ready } else { SandboxState::Gone },
            live,
            expires_at: hermit_api::Timestamp::now(),
        })
    }

    #[test]
    fn never_answered_admits() {
        let v = View::new();
        assert!(v.admits());
        assert!(v.refresh_due(Instant::now()));
    }

    #[test]
    fn failures_keep_the_last_answer_forever() {
        let t0 = Instant::now();
        let mut v = View::new();
        v.update(&answer(true), t0);
        assert!(v.admits());
        assert!(!v.refresh_due(t0 + Duration::from_secs(29)));
        assert!(v.refresh_due(t0 + Duration::from_secs(30)));
        for i in 0..100 {
            v.update(&Lookup::Failed, t0 + Duration::from_secs(i * 60));
        }
        assert!(v.admits(), "a hermit outage never refuses a valid token");
        v.update(&Lookup::Failed, t0);
        let mut never = View::new();
        never.update(&Lookup::Failed, t0);
        assert!(never.admits());
    }

    #[test]
    fn revocation() {
        let t0 = Instant::now();
        let mut v = View::new();
        v.update(&answer(true), t0);
        v.update(&answer(false), t0 + Duration::from_secs(30));
        assert!(!v.admits());
        v.update(&Lookup::Failed, t0 + Duration::from_secs(90));
        assert!(!v.admits(), "a refusal is kept while hermit fails too");
        let mut u = View::new();
        u.update(&Lookup::Unknown, t0);
        assert!(!u.admits());
    }
}
