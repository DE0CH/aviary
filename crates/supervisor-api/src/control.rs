//! The `:7070` types (supervisor 15.4, the text of record for this surface; platform R35). Every route needs
//! `Authorization: Bearer <control token>` whose SHA-256 is in the descriptor's `control.tokenSha256`; otherwise 401
//! `unauthenticated`. Until the descriptor has been read every call is 503 `unavailable` with `retry-after: 1`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{ActivityState, PendingRelease, ReleaseCode, ReleaseReason, Timestamp};

/// `GET /v1/health`. `ok: true, ready: false` = alive and starting (never unhealthy); hermit marks a sandbox
/// `ready` only on `ready: true`, and decides "unhealthy" only from two failed checks 5 s apart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub ok: bool,
    /// The descriptor read, the lease held, FUSE mounted, `/workspace` present.
    pub ready: bool,
    pub protocol: u32,
    /// `supervisor-<sha12>`.
    pub version: String,
    pub boot_id: String,
    /// The same value the activity reports carry.
    pub busy: bool,
    pub state: ActivityState,
}

/// Where a turn came from (platform R30).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnOrigin {
    Person,
    Agent,
    Import,
}

/// `GET /v1/state`: a diagnostic view (operator tooling and tests).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateReport {
    pub run: Run,
    /// The running generation (`gen` on the wire).
    #[serde(rename = "gen")]
    pub generation: Option<Generation>,
    pub lease_epoch: Option<u64>,
    pub fuse: Fuse,
    pub memory: Memory,
    pub supervisor: SupervisorFacts,
    pub magpie_reachable: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub state: ActivityState,
    pub turn_id: Option<Uuid>,
    pub origin: Option<TurnOrigin>,
    /// The open background tasks as the supervisor tracks them (implementation detail, for display).
    pub tasks: Vec<Value>,
    /// The open asks (implementation detail, for display).
    pub asks: Vec<Value>,
}

/// The running Claude Code generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Generation {
    pub n: u32,
    pub pid: u32,
    pub started_at: Timestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fuse {
    pub inflight: u32,
    pub blocked: u32,
    pub oldest_blocked_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub limit_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorFacts {
    pub version: String,
    pub boot_id: String,
    pub restarts: u32,
    pub rss_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrepareKind {
    /// A checkpoint that may be restored much later: refused `sandbox_busy` while busy unless forced.
    Pause,
    /// `sync(2)` before a workspace copy: never refused.
    Snapshot,
    /// Live migration: answers at once.
    Migrate,
}

/// `POST /v1/lifecycle/prepare`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareRequest {
    pub kind: PrepareKind,
    pub force: bool,
    pub deadline_ms: u32,
}

impl PrepareRequest {
    /// The flush hermit asks for before a workspace clone (hermit 3.8, DESIGN 12 D8).
    #[must_use]
    pub fn flush() -> Self {
        Self {
            kind: PrepareKind::Snapshot,
            force: false,
            deadline_ms: 10_000,
        }
    }
}

/// 200 of prepare.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareAnswer {
    pub ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResumedKind {
    Pause,
    Migrate,
}

/// `POST /v1/lifecycle/resumed` (204).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resumed {
    pub kind: ResumedKind,
    pub at: Timestamp,
}

/// `POST /v1/lifecycle/release`: `reason` is hermit's [`ReleaseReason`] (DESIGN 12 D5); `code` is
/// `sandbox_released` or `sandbox_lost` (how the supervisor reports a turn it ends: `canceled` or failed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRequest {
    pub reason: ReleaseReason,
    pub code: ReleaseCode,
    pub force: bool,
    pub deadline_ms: u32,
}

/// The default release deadline.
pub const RELEASE_DEADLINE_MS: u32 = 30_000;

impl ReleaseRequest {
    /// The same release as a pending one repeated in an activity reply (hermit 3.7.3), with the time left until its
    /// `deadlineAt` (at least 1 s).
    #[must_use]
    pub fn from_pending(p: &PendingRelease, now: Timestamp) -> Self {
        let left = p.deadline_at.unix_millis().saturating_sub(now.unix_millis()).max(1000);
        Self {
            reason: p.reason,
            code: p.code,
            force: p.force,
            deadline_ms: u32::try_from(left).unwrap_or(u32::MAX),
        }
    }
}

/// 200 of release: the sequence has finished (Claude Code stopped, every accepted FUSE write stored).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAnswer {
    pub released: bool,
}

/// The `run` field of a 409 `sandbox_busy` (an unforced release or pause while busy: nothing done).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub state: ActivityState,
    pub turn_id: Option<Uuid>,
    pub tasks: u32,
}

/// What a release or pause call came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 200.
    Done,
    /// 409 `sandbox_busy`: nothing was done; the turn goes on.
    Busy(RunSummary),
}

/// The 409 `sandbox_busy` answer with its `run`.
#[must_use]
pub fn busy(run: RunSummary) -> aviary_core::http::ApiError {
    let run = serde_json::to_value(run).unwrap_or_default();
    hermit_api::error::api(
        409,
        hermit_api::error::codes::SANDBOX_BUSY,
        "a turn, task or ask is open",
    )
    .with("run", run)
}

/// The 503 before the descriptor has been read.
#[must_use]
pub fn not_ready() -> aviary_core::http::ApiError {
    aviary_core::http::ApiError::unavailable("the descriptor has not been read yet", 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReleaseReason;

    #[test]
    fn from_pending_keeps_the_release() {
        let now = Timestamp::parse("2026-11-02T12:05:06.789Z").unwrap();
        let p = PendingRelease {
            reason: ReleaseReason::Lifetime,
            code: ReleaseCode::SandboxReleased,
            force: false,
            deadline_at: Timestamp::parse("2026-11-02T12:05:36.789Z").unwrap(),
        };
        let r = ReleaseRequest::from_pending(&p, now);
        assert_eq!(
            (r.reason, r.code, r.force, r.deadline_ms),
            (p.reason, p.code, false, 30_000)
        );
        let late = Timestamp::parse("2026-11-02T13:00:00Z").unwrap();
        assert_eq!(ReleaseRequest::from_pending(&p, late).deadline_ms, 1000);
    }

    #[test]
    fn busy_carries_the_run() {
        let e = busy(RunSummary {
            state: ActivityState::Held,
            turn_id: None,
            tasks: 2,
        });
        assert_eq!(e.status().as_u16(), 409);
        assert_eq!(e.body().fields["run"]["tasks"], 2);
        assert_eq!(not_ready().status().as_u16(), 503);
    }
}
