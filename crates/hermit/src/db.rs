//! hermit's database (hermit 10): the pool, the embedded migrations, the `sandboxes` row as the core reads it, and
//! `bump`, the one function through which every visible change of a row becomes a feed event and a wake-up.

use std::net::IpAddr;
use std::time::Duration;

use aviary_pg::{LockKey, Pool, Row, Tx};
use hermit_api::Timestamp;
use hermit_api::sandbox::{Release, ReleaseCode, ReleaseReason, Sandbox, SandboxPhase, SandboxState, SupervisorInfo};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

/// The advisory lock class of every hermit lock (hermit 10.3).
pub const LOCK_CLASS: i32 = 7300;
pub const LOCK_MIGRATIONS: LockKey = LockKey::Pair(LOCK_CLASS, 1);
pub const LOCK_CAPACITY: i32 = 2;
pub const LOCK_JANITOR: LockKey = LockKey::Pair(LOCK_CLASS, 3);
pub const LOCK_SCALER: LockKey = LockKey::Pair(LOCK_CLASS, 4);
pub const LOCK_EVENTS: i32 = 8;

/// The NOTIFY channel of row changes; the payload is `<conversation id>:<sandbox id>` (the listener keys waiters
/// by the part before the colon, so a held `GET` of a conversation wakes on any of its sandboxes).
pub const CHANNEL: &str = "hermit_sandbox";

/// The pool: `pool_size` connections for requests and tasks (10 in production), statement timeout 15 s.
///
/// # Errors
/// A malformed URL.
pub fn connect(url: &str, pod: &str, pool_size: usize) -> Result<Pool, aviary_pg::Error> {
    aviary_pg::connect(url, &format!("hermit:{pod}"), pool_size, Duration::from_secs(15))
}

/// Applies the embedded migrations under lock 7300/1.
///
/// # Errors
/// A migration failure or an edited shipped migration.
pub async fn migrate(pool: &Pool) -> Result<Vec<i32>, aviary_pg::Error> {
    let list = aviary_pg::migrations!["0001_init.sql"]?;
    aviary_pg::migrate(pool, &LOCK_MIGRATIONS, &list).await
}

/// A pending release (`release_pending`, hermit 13.12).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub reason: ReleaseReason,
    pub note: Option<String>,
    pub force: bool,
    /// When a forced release stops waiting for the supervisor.
    pub deadline_at: Timestamp,
    pub asked_at: Timestamp,
    #[serde(default)]
    pub refusals: u32,
}

impl Pending {
    #[must_use]
    pub fn code(&self) -> ReleaseCode {
        self.reason.code().unwrap_or(ReleaseCode::SandboxReleased)
    }
}

/// The `sandboxes` row.
#[derive(Clone, Debug)]
pub struct SandboxRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub person: String,
    pub status: SandboxState,
    pub phase: Option<SandboxPhase>,
    pub version: i32,
    pub name: String,
    pub token_kid: Option<i16>,
    pub slot_claimed: bool,
    pub host: Option<String>,
    pub ip: Option<IpAddr>,
    pub image: Option<String>,
    pub cpu: f32,
    pub memory_bytes: i64,
    pub busy: bool,
    pub busy_turn: Option<String>,
    pub background_tasks: i32,
    pub supervisor_version: Option<String>,
    pub supervisor_protocol: Option<i32>,
    pub boot_id: Option<String>,
    pub task: Option<String>,
    pub task_owner: Option<String>,
    pub task_until: Option<OffsetDateTime>,
    pub migrating_to: Option<String>,
    pub start_deadline: OffsetDateTime,
    pub reserved_until: Option<OffsetDateTime>,
    pub release_pending: Option<Pending>,
    pub created_at: OffsetDateTime,
    pub ready_at: Option<OffsetDateTime>,
    pub last_active_at: OffsetDateTime,
    pub last_heartbeat_at: Option<OffsetDateTime>,
    pub last_busy_at: Option<OffsetDateTime>,
    pub expires_at: OffsetDateTime,
    pub hard_expires_at: OffsetDateTime,
    pub released_at: Option<OffsetDateTime>,
    pub release_reason: Option<String>,
    pub release_note: Option<String>,
    pub release_code: Option<String>,
    pub release_message: Option<String>,
}

fn wire<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(Value::String(s.to_owned())).ok()
}

impl SandboxRow {
    /// Decodes a `select *` row.
    ///
    /// # Errors
    /// A column of an unexpected type.
    pub fn from_row(r: &Row) -> Result<Self, aviary_pg::Error> {
        let status: String = r.try_get("status")?;
        let phase: Option<String> = r.try_get("phase")?;
        let pending: Option<Value> = r.try_get("release_pending")?;
        Ok(Self {
            id: r.try_get("id")?,
            conversation_id: r.try_get("conversation_id")?,
            person: r.try_get("person")?,
            status: wire(&status).unwrap_or(SandboxState::Unknown),
            phase: phase.as_deref().and_then(wire),
            version: r.try_get("version")?,
            name: r.try_get("name")?,
            token_kid: r.try_get("token_kid")?,
            slot_claimed: r.try_get("slot_claimed")?,
            host: r.try_get("host")?,
            ip: r.try_get("ip")?,
            image: r.try_get("image")?,
            cpu: r.try_get("cpu")?,
            memory_bytes: r.try_get("memory_bytes")?,
            busy: r.try_get("busy")?,
            busy_turn: r.try_get("busy_turn")?,
            background_tasks: r.try_get("background_tasks")?,
            supervisor_version: r.try_get("supervisor_version")?,
            supervisor_protocol: r.try_get("supervisor_protocol")?,
            boot_id: r.try_get("boot_id")?,
            task: r.try_get("task")?,
            task_owner: r.try_get("task_owner")?,
            task_until: r.try_get("task_until")?,
            migrating_to: r.try_get("migrating_to")?,
            start_deadline: r.try_get("start_deadline")?,
            reserved_until: r.try_get("reserved_until")?,
            release_pending: pending.and_then(|v| serde_json::from_value(v).ok()),
            created_at: r.try_get("created_at")?,
            ready_at: r.try_get("ready_at")?,
            last_active_at: r.try_get("last_active_at")?,
            last_heartbeat_at: r.try_get("last_heartbeat_at")?,
            last_busy_at: r.try_get("last_busy_at")?,
            expires_at: r.try_get("expires_at")?,
            hard_expires_at: r.try_get("hard_expires_at")?,
            released_at: r.try_get("released_at")?,
            release_reason: r.try_get("release_reason")?,
            release_note: r.try_get("release_note")?,
            release_code: r.try_get("release_code")?,
            release_message: r.try_get("release_message")?,
        })
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        self.status != SandboxState::Gone
    }

    /// The release a gone row records.
    #[must_use]
    pub fn release(&self) -> Option<Release> {
        if self.status != SandboxState::Gone {
            return None;
        }
        let reason = self
            .release_reason
            .as_deref()
            .and_then(wire)
            .unwrap_or(ReleaseReason::Unknown);
        Some(Release {
            reason,
            note: self.release_note.clone(),
            code: self
                .release_code
                .as_deref()
                .and_then(wire)
                .unwrap_or(ReleaseCode::Unknown),
            message: self.release_message.clone().unwrap_or_default(),
        })
    }

    /// The contract's `Sandbox` (3.6.1): no address, no credential.
    #[must_use]
    pub fn to_sandbox(&self) -> Sandbox {
        let supervisor = match (&self.supervisor_version, self.supervisor_protocol, &self.boot_id) {
            (Some(v), Some(p), Some(b)) => Some(SupervisorInfo {
                protocol: u32::try_from(p).unwrap_or(0),
                version: v.clone(),
                boot_id: b.clone(),
            }),
            _ => None,
        };
        Sandbox {
            id: self.id,
            conversation_id: self.conversation_id,
            person: self.person.clone(),
            state: self.status,
            phase: if self.status == SandboxState::Starting {
                self.phase
            } else {
                None
            },
            version: u64::try_from(self.version).unwrap_or(0),
            created_at: self.created_at.into(),
            ready_at: self.ready_at.map(Into::into),
            last_active_at: self.last_active_at.into(),
            expires_at: self.expires_at.into(),
            released_at: self.released_at.map(Into::into),
            busy: self.busy,
            image: self.image.clone(),
            supervisor,
            release: self.release(),
        }
    }

    /// Whether a claim of another task holds the row now.
    #[must_use]
    pub fn claimed(&self, now: OffsetDateTime) -> bool {
        self.task.is_some() && self.task_until.is_some_and(|u| u > now)
    }
}

/// Text of a `SandboxState`/`SandboxPhase`/`ReleaseReason` for SQL.
#[must_use]
pub fn txt<T: std::fmt::Display>(v: T) -> String {
    v.to_string()
}

/// The row of `id`.
///
/// # Errors
/// A database error.
pub async fn load(pool: &Pool, id: Uuid) -> Result<Option<SandboxRow>, aviary_pg::Error> {
    let c = pool.get().await?;
    c.query_opt("select * from sandboxes where id = $1", &[&id])
        .await?
        .as_ref()
        .map(SandboxRow::from_row)
        .transpose()
}

/// The live row of a conversation, else its latest gone row.
///
/// # Errors
/// A database error.
pub async fn latest_of(pool: &Pool, cid: Uuid) -> Result<Option<SandboxRow>, aviary_pg::Error> {
    let c = pool.get().await?;
    c.query_opt(
        "select * from sandboxes where conversation_id = $1 order by (status <> 'gone') desc, created_at desc limit 1",
        &[&cid],
    )
    .await?
    .as_ref()
    .map(SandboxRow::from_row)
    .transpose()
}

/// Every visible change of a row ends with this, as the last statements of its transaction (hermit 10.4-10.5):
/// `version + 1`, the feed event under lock 7300/8 (so events commit in seq order), and the NOTIFY.
///
/// # Errors
/// A database error (the transaction then rolls back).
pub async fn bump(t: &Tx<'_>, id: Uuid) -> Result<SandboxRow, aviary_pg::Error> {
    let row = t
        .query_one(
            "update sandboxes set version = version + 1 where id = $1 returning *",
            &[&id],
        )
        .await?;
    let row = SandboxRow::from_row(&row)?;
    t.execute(
        "select pg_advisory_xact_lock($1::int4, $2::int4)",
        &[&LOCK_CLASS, &LOCK_EVENTS],
    )
    .await?;
    let release = row.release().and_then(|r| serde_json::to_value(r).ok());
    let phase = if row.status == SandboxState::Starting {
        row.phase.map(txt)
    } else {
        None
    };
    t.execute(
        "insert into sandbox_events (sandbox_id, conversation_id, person, state, phase, version, busy, release)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[
            &row.id,
            &row.conversation_id,
            &row.person,
            &txt(row.status),
            &phase,
            &row.version,
            &row.busy,
            &release,
        ],
    )
    .await?;
    t.notify(CHANNEL, &format!("{}:{}", row.conversation_id, row.id))
        .await?;
    Ok(row)
}

/// A redacted, bounded message (3.6.1: at most 300 characters, no credentials).
#[must_use]
pub fn message(m: &str) -> String {
    clip(&aviary_core::log::redact(m), 300)
}

/// At most `n` characters.
#[must_use]
pub fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
