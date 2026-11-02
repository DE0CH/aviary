//! The sandbox's own routes (hermit 3.7, surface `hermit-sandbox`): the descriptor, secrets and activity. Defined
//! once here and re-exported by `supervisor-api`, so the supervisor and hermit compile against one definition
//! (platform R35; hermit 3.7 is the text of record).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::sandbox::{ReleaseCode, ReleaseReason};
use crate::time::Timestamp;

/// `GET /v1/sandbox/self` (3.7.1): at most 64 KiB, `If-None-Match` honoured.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSelf {
    pub sandbox: SelfSandbox,
    pub control: Control,
    pub claude: Claude,
    pub title: Title,
    /// Further variables every process the agent starts must have; applied unchanged, never interpreted. Keys are
    /// environment variable names (data, not field names).
    pub env: BTreeMap<String, String>,
    /// Boot-environment variables the agent must not inherit.
    pub env_private: Vec<String>,
    pub resources: Resources,
    pub lifecycle: Lifecycle,
    pub secrets: SecretKeys,
    /// `"` + the first 32 hex of SHA-256 over the canonical JSON of every other field + `"` (also the `ETag`).
    pub etag: String,
}

/// The descriptor's size bound.
pub const SELF_MAX_BYTES: usize = 64 * 1024;
/// At most this many `env` + `envPrivate` entries, together at most [`ENV_MAX_BYTES`].
pub const ENV_MAX_ENTRIES: usize = 128;
pub const ENV_MAX_BYTES: usize = 16 * 1024;

impl SandboxSelf {
    /// The etag of every field but `etag`: canonical JSON is `serde_json`'s compact form with object keys sorted.
    #[must_use]
    pub fn compute_etag(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(map) = value.as_object_mut() {
            map.remove("etag");
        }
        let canonical = canonical_json(&value);
        let digest = Sha256::digest(canonical.as_bytes());
        let hex = hex(&digest);
        format!("\"{}\"", &hex[..32])
    }

    /// Sets `etag` from the other fields.
    #[must_use]
    pub fn with_etag(mut self) -> Self {
        self.etag = self.compute_etag();
        self
    }
}

/// Compact JSON with every object's keys sorted (`serde_json`'s map is ordered by key without `preserve_order`; this
/// sorts explicitly so the result does not depend on that feature).
#[must_use]
pub fn canonical_json(value: &serde_json::Value) -> String {
    use serde_json::Value;
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String((*k).clone()).to_string());
                    out.push(':');
                    write(&map[k.as_str()], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfSandbox {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub person: String,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    /// `sha256:<64 hex>`.
    pub image: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    /// Always 7070 (fixed for every implementation).
    pub port: u16,
    /// SHA-256 hex of each control token hermit may present (a list so a rotation can overlap).
    pub token_sha256: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claude {
    /// Written by the supervisor to `/etc/claude-code/CLAUDE.md`.
    pub managed_claude_md: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Title {
    /// The instructions sent with magpie's `title` command.
    pub system: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    /// The memory guard's limit.
    pub memory_bytes: u64,
    pub cpus: u32,
    pub pids: u32,
    pub tmp_bytes: u64,
    pub open_files: u32,
    pub workspace: WorkspaceResources,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResources {
    pub path: String,
    pub quota_bytes: u64,
    pub quota_inodes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lifecycle {
    /// The activity cadence (the supervisor uses `min(value, 15)`).
    pub activity_every_seconds: u32,
    pub idle_minutes: u32,
    pub max_lifetime_hours: u32,
    pub lifetime_grace_minutes: u32,
}

/// `GET /v1/sandbox/secrets` and the descriptor's `secrets` (3.7.2): the keys this sandbox may ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretKeys {
    pub keys: Vec<String>,
}

/// `GET /v1/sandbox/secrets/{key}` 200 (404 `not_granted` otherwise).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretValue {
    pub key: String,
    pub value: String,
}

/// A secret key: `[A-Z][A-Z0-9_]{0,63}`.
#[must_use]
pub fn valid_secret_key(key: &str) -> bool {
    let b = key.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_uppercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// The supervisor's run state as reported (3.7.3; `starting` and `released` from supervisor 15.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityState {
    /// A `start_turn` received and not yet running.
    Starting,
    /// A turn.
    Running,
    /// An interrupt in progress.
    Stopping,
    /// Background tasks or an open ask after the turn.
    Held,
    Idle,
    /// The release sequence has finished: hermit may destroy the sandbox.
    Released,
}

impl ActivityState {
    /// `starting`, `running`, `stopping` and `held` are busy; `idle` and `released` are not.
    #[must_use]
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping | Self::Held)
    }
}

/// `POST /v1/sandbox/activity` body: every `activityEverySeconds` and at once on every change of `state`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityReport {
    pub state: ActivityState,
    pub turn_id: Option<Uuid>,
    /// Open background tasks.
    pub tasks: u32,
    /// Only in `idle`.
    pub idle_since: Option<Timestamp>,
    pub last_output_at: Option<Timestamp>,
    pub boot_id: String,
    /// The `hermit-sandbox` number the supervisor speaks.
    pub protocol: u32,
    pub version: String,
    pub restarts: u32,
    pub rss_bytes: u64,
    pub memory: MemoryUse,
    /// What happened since the last accepted report (at most 32, kept across failed reports).
    #[serde(default)]
    pub events: Vec<ActivityEvent>,
}

/// The most events one report carries.
pub const ACTIVITY_MAX_EVENTS: usize = 32;

impl ActivityReport {
    /// `busy` as hermit derives it from `state`.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.state.is_busy()
    }
}

/// The sandbox's memory in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryUse {
    pub limit: u64,
    pub used: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityEventKind {
    /// The memory guard killed a process (hermit records a `memory-kill` host event).
    MemoryKill,
    /// A Claude Code generation exited (supervisor 15.3).
    GenerationExit,
    #[serde(other)]
    Unknown,
}

/// One entry of `events[]`. Fields beyond `kind` and `at` depend on the kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub kind: ActivityEventKind,
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comm: Option<String>,
    /// Resident memory in bytes of the killed process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityReplyState {
    Ready,
    Releasing,
}

/// 200 answer to a report. A non-null `release` repeats a pending release in case hermit's direct call to :7070 was
/// lost; the supervisor acts on it exactly as on `POST /v1/lifecycle/release` with the same fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityReply {
    pub state: ActivityReplyState,
    pub release: Option<PendingRelease>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRelease {
    pub reason: ReleaseReason,
    pub code: ReleaseCode,
    pub force: bool,
    pub deadline_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_keys() {
        assert!(valid_secret_key("A"));
        assert!(valid_secret_key("GITHUB_TOKEN_2"));
        assert!(!valid_secret_key(""));
        assert!(!valid_secret_key("lower"));
        assert!(!valid_secret_key("_A"));
        assert!(!valid_secret_key(&format!("A{}", "B".repeat(64))));
        assert!(valid_secret_key(&format!("A{}", "B".repeat(63))));
    }

    #[test]
    fn busy_states() {
        use ActivityState as S;
        for (s, busy) in [
            (S::Starting, true),
            (S::Running, true),
            (S::Stopping, true),
            (S::Held, true),
            (S::Idle, false),
            (S::Released, false),
        ] {
            assert_eq!(s.is_busy(), busy, "{s:?}");
        }
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let v = serde_json::json!({"b": [{"z": 1, "a": "x"}], "a": {"d": null, "c": true}});
        assert_eq!(canonical_json(&v), r#"{"a":{"c":true,"d":null},"b":[{"a":"x","z":1}]}"#);
    }
}
