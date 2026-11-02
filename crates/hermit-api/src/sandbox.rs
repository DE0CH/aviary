//! Sandboxes as magpie sees them (hermit 3.6): the `Sandbox` object, get-or-start, the long-poll, release and the
//! change feed. `ReleaseReason` and `ReleaseCode` are shared with the supervisor's release call (13.13, platform R34).

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::time::Timestamp;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxState {
    Starting,
    Ready,
    Paused,
    Releasing,
    Gone,
}

impl SandboxState {
    /// Every state but `gone`.
    #[must_use]
    pub fn is_live(self) -> bool {
        self != Self::Gone
    }
}

/// Where a start is (only while `starting`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxPhase {
    Capacity,
    Workspace,
    Placing,
    Host,
    Image,
    Creating,
    Supervisor,
}

/// Why a sandbox ended (hermit 13.13): one enum for hermit's feed, the ops page and the supervisor's release call.
/// A value this build does not know deserialises as [`ReleaseReason::Unknown`] (the contract grows additively).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    Idle,
    Lifetime,
    Pool,
    Requested,
    Deleted,
    Offboarded,
    NotAllowed,
    Operator,
    Recycle,
    Unhealthy,
    Vanished,
    HostLost,
    MigrationFailed,
    PoolFull,
    HostsFull,
    NoHostInTime,
    StartFailed,
    StartTimedOut,
    SupervisorTimeout,
    WorkspaceUnavailable,
    Stuck,
    #[serde(other)]
    Unknown,
}

impl ReleaseReason {
    /// Every known value, in the order of hermit 13.13.
    pub const ALL: [Self; 21] = [
        Self::Idle,
        Self::Lifetime,
        Self::Pool,
        Self::Requested,
        Self::Deleted,
        Self::Offboarded,
        Self::NotAllowed,
        Self::Operator,
        Self::Recycle,
        Self::Unhealthy,
        Self::Vanished,
        Self::HostLost,
        Self::MigrationFailed,
        Self::PoolFull,
        Self::HostsFull,
        Self::NoHostInTime,
        Self::StartFailed,
        Self::StartTimedOut,
        Self::SupervisorTimeout,
        Self::WorkspaceUnavailable,
        Self::Stuck,
    ];

    /// The code that goes with the reason (13.13). `stuck` keeps the code it had; `None` for it and for unknown
    /// values.
    #[must_use]
    pub fn code(self) -> Option<ReleaseCode> {
        use ReleaseReason as R;
        match self {
            R::Idle
            | R::Lifetime
            | R::Pool
            | R::Requested
            | R::Deleted
            | R::Offboarded
            | R::NotAllowed
            | R::Operator
            | R::Recycle => Some(ReleaseCode::SandboxReleased),
            R::Unhealthy | R::Vanished | R::HostLost | R::MigrationFailed => Some(ReleaseCode::SandboxLost),
            R::PoolFull | R::HostsFull | R::NoHostInTime => Some(ReleaseCode::SandboxBusy),
            R::StartFailed | R::StartTimedOut | R::SupervisorTimeout | R::WorkspaceUnavailable => {
                Some(ReleaseCode::SandboxFailed)
            }
            R::Stuck | R::Unknown => None,
        }
    }

    /// The wire value (`host_lost`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        use ReleaseReason as R;
        match self {
            R::Idle => "idle",
            R::Lifetime => "lifetime",
            R::Pool => "pool",
            R::Requested => "requested",
            R::Deleted => "deleted",
            R::Offboarded => "offboarded",
            R::NotAllowed => "not_allowed",
            R::Operator => "operator",
            R::Recycle => "recycle",
            R::Unhealthy => "unhealthy",
            R::Vanished => "vanished",
            R::HostLost => "host_lost",
            R::MigrationFailed => "migration_failed",
            R::PoolFull => "pool_full",
            R::HostsFull => "hosts_full",
            R::NoHostInTime => "no_host_in_time",
            R::StartFailed => "start_failed",
            R::StartTimedOut => "start_timed_out",
            R::SupervisorTimeout => "supervisor_timeout",
            R::WorkspaceUnavailable => "workspace_unavailable",
            R::Stuck => "stuck",
            R::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ReleaseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The class of an end (3.6.1): what magpie maps to a turn code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseCode {
    /// An orderly end (idle, lifetime, pool, deleted, offboarded, operator).
    SandboxReleased,
    /// It died (host lost, out of memory, vanished, unhealthy, migration).
    SandboxLost,
    /// No capacity: it never became ready.
    SandboxBusy,
    /// The start failed for another reason.
    SandboxFailed,
}

/// Why and how a sandbox ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub reason: ReleaseReason,
    /// At most 200 characters: the operator's note or the class of a start error.
    pub note: Option<String>,
    pub code: ReleaseCode,
    /// English detail for logs, at most 300 characters, redacted.
    pub message: String,
}

/// The supervisor as its reports describe it (information only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorInfo {
    pub protocol: u32,
    pub version: String,
    pub boot_id: String,
}

/// The sandbox object (3.6.1). It carries no address and no credential of the supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sandbox {
    /// A new id for every new sandbox of a conversation (the "generation").
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub person: String,
    pub state: SandboxState,
    /// Only while `starting`.
    pub phase: Option<SandboxPhase>,
    /// +1 on every change a caller can see (state, phase, busy).
    pub version: u64,
    pub created_at: Timestamp,
    pub ready_at: Option<Timestamp>,
    /// The idle clock (magpie's watchdog reads it).
    pub last_active_at: Timestamp,
    pub expires_at: Timestamp,
    pub released_at: Option<Timestamp>,
    /// From the supervisor's last activity report.
    pub busy: bool,
    /// `sha256:<64 hex>`.
    pub image: Option<String>,
    pub supervisor: Option<SupervisorInfo>,
    pub release: Option<Release>,
}

/// `PUT /v1/conversations/{cid}/sandbox` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutSandboxRequest {
    pub person: String,
}

/// The answer of get-or-start: 200 for a `ready` sandbox, 202 for one that is starting (or resuming).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PutSandbox {
    /// 200.
    Ready(Sandbox),
    /// 202.
    Starting(Sandbox),
}

impl PutSandbox {
    #[must_use]
    pub fn sandbox(&self) -> &Sandbox {
        match self {
            Self::Ready(s) | Self::Starting(s) => s,
        }
    }

    #[must_use]
    pub fn into_sandbox(self) -> Sandbox {
        match self {
            Self::Ready(s) | Self::Starting(s) => s,
        }
    }

    #[must_use]
    pub fn status(&self) -> u16 {
        match self {
            Self::Ready(_) => 200,
            Self::Starting(_) => 202,
        }
    }
}

/// `GET /v1/conversations/{cid}/sandbox?waitMs=&afterVersion=` (3.6.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxQuery {
    /// 0..=25000; 0 or absent = answer at once.
    #[serde(default)]
    pub wait_ms: Option<u32>,
    #[serde(default)]
    pub after_version: Option<u64>,
}

/// The longest `waitMs` hermit holds a request for.
pub const MAX_WAIT_MS: u32 = 25_000;

/// The reasons magpie may give for a release (3.6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationReleaseReason {
    /// Forced.
    Deleted,
    /// Forced.
    Offboarded,
    /// Soft: done at the next idle point.
    Requested,
}

impl From<ConversationReleaseReason> for ReleaseReason {
    fn from(r: ConversationReleaseReason) -> Self {
        match r {
            ConversationReleaseReason::Deleted => Self::Deleted,
            ConversationReleaseReason::Offboarded => Self::Offboarded,
            ConversationReleaseReason::Requested => Self::Requested,
        }
    }
}

/// `POST /v1/conversations/{cid}/sandbox/release` body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSandboxRequest {
    pub reason: ConversationReleaseReason,
}

/// Its 200 answer (the release itself runs in the background).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSandboxReply {
    /// 0 or 1.
    pub released: u32,
    pub sandbox: Option<Sandbox>,
}

/// `POST …/sandbox/snapshot` 201 (implementations with snapshots; others answer 501).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotReply {
    pub snapshot_id: Uuid,
    pub at: Timestamp,
}

/// The feed cursor: a sequence number or `latest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Seq(u64),
    Latest,
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seq(n) => write!(f, "{n}"),
            Self::Latest => f.write_str("latest"),
        }
    }
}

impl Serialize for Cursor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Cursor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Cursor;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence number or \"latest\"")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Cursor, E> {
                Ok(Cursor::Seq(v))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Cursor, E> {
                if v == "latest" {
                    Ok(Cursor::Latest)
                } else {
                    v.parse().map(Cursor::Seq).map_err(E::custom)
                }
            }
        }
        d.deserialize_any(V)
    }
}

/// `GET /v1/sandbox-events?after=&limit=&waitMs=` (3.6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEventsQuery {
    pub after: Cursor,
    /// 1..=500, default 200.
    #[serde(default)]
    pub limit: Option<u32>,
    /// 0..=25000.
    #[serde(default)]
    pub wait_ms: Option<u32>,
}

/// The feed page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEvents {
    pub head_seq: u64,
    pub events: Vec<SandboxEvent>,
}

/// One visible state or phase change (busy flips included).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEvent {
    pub seq: u64,
    pub at: Timestamp,
    pub sandbox_id: Uuid,
    pub conversation_id: Uuid,
    pub person: String,
    pub state: SandboxState,
    pub phase: Option<SandboxPhase>,
    pub version: u64,
    pub busy: bool,
    pub release: Option<Release>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_match_their_wire_values_and_codes() {
        for r in ReleaseReason::ALL {
            assert_eq!(serde_json::to_value(r).unwrap(), serde_json::Value::from(r.as_str()));
            assert_eq!(serde_json::from_value::<ReleaseReason>(r.as_str().into()).unwrap(), r);
            assert_eq!(r.code().is_none(), r == ReleaseReason::Stuck, "{r}");
        }
        assert_eq!(
            serde_json::from_str::<ReleaseReason>("\"a_future_reason\"").unwrap(),
            ReleaseReason::Unknown
        );
        assert_eq!(ReleaseReason::HostLost.code(), Some(ReleaseCode::SandboxLost));
        assert_eq!(ReleaseReason::PoolFull.code(), Some(ReleaseCode::SandboxBusy));
    }

    #[test]
    fn cursor_forms() {
        assert_eq!(serde_json::from_str::<Cursor>("\"latest\"").unwrap(), Cursor::Latest);
        assert_eq!(serde_json::from_str::<Cursor>("\"98122\"").unwrap(), Cursor::Seq(98122));
        assert_eq!(serde_json::from_str::<Cursor>("7").unwrap(), Cursor::Seq(7));
        assert!(serde_json::from_str::<Cursor>("\"soon\"").is_err());
        assert_eq!(Cursor::Seq(3).to_string(), "3");
    }
}
