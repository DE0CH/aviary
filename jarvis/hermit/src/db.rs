//! hermit-jarvis's rows (migrations/): sessions (one conversation each), sandboxes (one per boot of a session's
//! machine) and the change feed. Every visible change bumps the sandbox's `version` and appends a feed row in the
//! same transaction (hermit 3.6.5).

use std::time::Duration;

use aviary_pg::{Pool, Row, Tx};
use hermit_api::sandbox::{
    Release, ReleaseReason, Sandbox, SandboxEvent, SandboxPhase, SandboxState, SupervisorInfo,
};
use hermit_api::sandbox_surface::PendingRelease;
use hermit_api::time::Timestamp;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

/// The migrations, applied under this advisory lock.
pub const MIGRATION_LOCK: &str = "hermit-jarvis-migrations";

/// Opens the pool and applies the migrations.
///
/// # Errors
/// A bad URL, or a migration that fails.
pub async fn open(url: &str) -> anyhow::Result<Pool> {
    let pool = aviary_pg::connect(url, "hermit-jarvis", 8, Duration::from_secs(10))?;
    let migrations = aviary_pg::migrations!["0001_init.sql", "0002_chat.sql"]?;
    aviary_pg::migrate(
        &pool,
        &aviary_pg::LockKey::name(MIGRATION_LOCK),
        &migrations,
    )
    .await?;
    Ok(pool)
}

/// A session's conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub session_id: String,
    pub conversation_id: Uuid,
    pub person: String,
}

impl SessionRow {
    fn of(r: &Row) -> Self {
        Self {
            session_id: r.get("session_id"),
            conversation_id: r.get("conversation_id"),
            person: r.get("person"),
        }
    }
}

/// # Errors
/// A statement error.
pub async fn session(
    pool: &Pool,
    session_id: &str,
) -> Result<Option<SessionRow>, aviary_pg::Error> {
    Ok(pool
        .get()
        .await?
        .query_opt(
            "select * from sessions where session_id = $1",
            &[&session_id],
        )
        .await?
        .as_ref()
        .map(SessionRow::of))
}

/// # Errors
/// A statement error.
pub async fn session_of_conversation(
    pool: &Pool,
    cid: Uuid,
) -> Result<Option<SessionRow>, aviary_pg::Error> {
    Ok(pool
        .get()
        .await?
        .query_opt("select * from sessions where conversation_id = $1", &[&cid])
        .await?
        .as_ref()
        .map(SessionRow::of))
}

/// Every session whose conversation is still in the chat, with what magpie was last told about it (`open` or
/// `closed`).
///
/// # Errors
/// A statement error.
pub async fn chats(pool: &Pool) -> Result<Vec<(SessionRow, String)>, aviary_pg::Error> {
    Ok(pool
        .get()
        .await?
        .query("select * from sessions where chat <> 'ended'", &[])
        .await?
        .iter()
        .map(|r| (SessionRow::of(r), r.get("chat")))
        .collect())
}

/// Records what magpie was told about a session's conversation.
///
/// # Errors
/// A statement error.
pub async fn set_chat(pool: &Pool, session_id: &str, chat: &str) -> Result<(), aviary_pg::Error> {
    pool.get()
        .await?
        .execute(
            "update sessions set chat = $2 where session_id = $1",
            &[&session_id, &chat],
        )
        .await?;
    Ok(())
}

/// Records a session's conversation (idempotent: magpie's create is too, by the session id).
///
/// # Errors
/// A statement error.
pub async fn put_session(pool: &Pool, s: &SessionRow) -> Result<(), aviary_pg::Error> {
    pool.get()
        .await?
        .execute(
            "insert into sessions (session_id, conversation_id, person) values ($1, $2, $3)
             on conflict (session_id) do nothing",
            &[&s.session_id, &s.conversation_id, &s.person],
        )
        .await?;
    Ok(())
}

/// One sandbox row.
#[derive(Clone, Debug, PartialEq)]
pub struct SandboxRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub session_id: String,
    pub person: String,
    pub machine: Option<String>,
    pub state: SandboxState,
    pub phase: Option<SandboxPhase>,
    pub version: u64,
    pub created_at: OffsetDateTime,
    pub ready_at: Option<OffsetDateTime>,
    pub last_active_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub released_at: Option<OffsetDateTime>,
    pub busy: bool,
    pub image: Option<String>,
    pub supervisor: Option<SupervisorInfo>,
    pub release: Option<Release>,
    pub pending: Option<PendingRelease>,
    pub boot: Value,
}

fn state_of(s: &str) -> SandboxState {
    serde_json::from_value(Value::String(s.to_owned())).unwrap_or(SandboxState::Unknown)
}

fn phase_of(s: Option<String>) -> Option<SandboxPhase> {
    s.and_then(|p| serde_json::from_value(Value::String(p)).ok())
}

fn json_of<T: serde::de::DeserializeOwned>(v: Option<Value>) -> Option<T> {
    v.and_then(|v| serde_json::from_value(v).ok())
}

impl SandboxRow {
    fn of(r: &Row) -> Self {
        let version: i64 = r.get("version");
        Self {
            id: r.get("id"),
            conversation_id: r.get("conversation_id"),
            session_id: r.get("session_id"),
            person: r.get("person"),
            machine: r.get("machine"),
            state: state_of(r.get::<_, &str>("state")),
            phase: phase_of(r.get("phase")),
            version: u64::try_from(version).unwrap_or(0),
            created_at: r.get("created_at"),
            ready_at: r.get("ready_at"),
            last_active_at: r.get("last_active_at"),
            expires_at: r.get("expires_at"),
            released_at: r.get("released_at"),
            busy: r.get("busy"),
            image: r.get("image"),
            supervisor: json_of(r.get("supervisor")),
            release: json_of(r.get("release")),
            pending: json_of(r.get("pending")),
            boot: r.get("boot"),
        }
    }

    /// The contract's sandbox object.
    #[must_use]
    pub fn sandbox(&self) -> Sandbox {
        Sandbox {
            id: self.id,
            conversation_id: self.conversation_id,
            person: self.person.clone(),
            state: self.state,
            phase: if self.state == SandboxState::Starting {
                self.phase
            } else {
                None
            },
            version: self.version,
            created_at: Timestamp::from_datetime(self.created_at),
            ready_at: self.ready_at.map(Timestamp::from_datetime),
            last_active_at: Timestamp::from_datetime(self.last_active_at),
            expires_at: Timestamp::from_datetime(self.expires_at),
            released_at: self.released_at.map(Timestamp::from_datetime),
            busy: self.busy,
            image: self.image.clone(),
            supervisor: self.supervisor.clone(),
            release: self.release.clone(),
        }
    }

    fn event(&self, seq: u64, at: OffsetDateTime) -> SandboxEvent {
        SandboxEvent {
            seq,
            at: Timestamp::from_datetime(at),
            sandbox_id: self.id,
            conversation_id: self.conversation_id,
            person: self.person.clone(),
            state: self.state,
            phase: if self.state == SandboxState::Starting {
                self.phase
            } else {
                None
            },
            version: self.version,
            busy: self.busy,
            release: self.release.clone(),
        }
    }
}

/// # Errors
/// A statement error.
pub async fn sandbox(pool: &Pool, id: Uuid) -> Result<Option<SandboxRow>, aviary_pg::Error> {
    Ok(pool
        .get()
        .await?
        .query_opt("select * from sandboxes where id = $1", &[&id])
        .await?
        .as_ref()
        .map(SandboxRow::of))
}

/// The conversation's newest sandbox, live or not.
///
/// # Errors
/// A statement error.
pub async fn newest(pool: &Pool, cid: Uuid) -> Result<Option<SandboxRow>, aviary_pg::Error> {
    Ok(pool
        .get()
        .await?
        .query_opt(
            "select * from sandboxes where conversation_id = $1 order by created_at desc limit 1",
            &[&cid],
        )
        .await?
        .as_ref()
        .map(SandboxRow::of))
}

/// Every sandbox that is not gone.
///
/// # Errors
/// A statement error.
pub async fn live(pool: &Pool) -> Result<Vec<SandboxRow>, aviary_pg::Error> {
    Ok(pool
        .get()
        .await?
        .query("select * from sandboxes where state <> 'gone'", &[])
        .await?
        .iter()
        .map(SandboxRow::of)
        .collect())
}

async fn locked(t: &Tx<'_>, id: Uuid) -> Result<Option<SandboxRow>, aviary_pg::Error> {
    Ok(
        t.query_opt("select * from sandboxes where id = $1 for update", &[&id])
            .await?
            .as_ref()
            .map(SandboxRow::of),
    )
}

async fn locked_live(t: &Tx<'_>, cid: Uuid) -> Result<Option<SandboxRow>, aviary_pg::Error> {
    Ok(t.query_opt(
        "select * from sandboxes where conversation_id = $1 and state <> 'gone' for update",
        &[&cid],
    )
    .await?
    .as_ref()
    .map(SandboxRow::of))
}

/// Writes `row` back with version + 1 and appends its feed row.
async fn store(t: &Tx<'_>, row: &mut SandboxRow) -> Result<(), aviary_pg::Error> {
    row.version += 1;
    let version = i64::try_from(row.version).unwrap_or(i64::MAX);
    let state = row.state.to_string();
    let phase = row.phase.map(|p| p.to_string());
    let supervisor = row
        .supervisor
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let release = row
        .release
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let pending = row
        .pending
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    t.execute(
        "update sandboxes set machine = $2, state = $3, phase = $4, version = $5, ready_at = $6, last_active_at = $7,
                expires_at = $8, released_at = $9, busy = $10, image = $11, supervisor = $12, release = $13,
                pending = $14, boot = $15
         where id = $1",
        &[
            &row.id,
            &row.machine,
            &state,
            &phase,
            &version,
            &row.ready_at,
            &row.last_active_at,
            &row.expires_at,
            &row.released_at,
            &row.busy,
            &row.image,
            &supervisor,
            &release,
            &pending,
            &row.boot,
        ],
    )
    .await?;
    append_event(t, row).await
}

async fn append_event(t: &Tx<'_>, row: &SandboxRow) -> Result<(), aviary_pg::Error> {
    let body = serde_json::to_value(row.event(0, OffsetDateTime::now_utc())).unwrap_or(Value::Null);
    t.execute("insert into sandbox_events (body) values ($1)", &[&body])
        .await?;
    Ok(())
}

/// A new sandbox for a session's conversation, `starting` in `phase` (gone first: any live one, as `reason`).
///
/// # Errors
/// A statement error.
pub async fn start(
    pool: &Pool,
    s: &SessionRow,
    machine: Option<&str>,
    phase: SandboxPhase,
    lifetime: Duration,
    replaces: ReleaseReason,
) -> Result<SandboxRow, aviary_pg::Error> {
    // borrowed: the transaction may run more than once (aviary_pg::tx!)
    let machine = &machine.map(str::to_owned);
    aviary_pg::tx!(pool, |t| {
        if let Some(mut old) = locked_live(t, s.conversation_id).await? {
            end(&mut old, replaces, "a new boot of the session's machine");
            store(t, &mut old).await?;
        }
        let now = OffsetDateTime::now_utc();
        let mut row = SandboxRow {
            id: Uuid::now_v7(),
            conversation_id: s.conversation_id,
            session_id: s.session_id.clone(),
            person: s.person.clone(),
            machine: machine.clone(),
            state: SandboxState::Starting,
            phase: Some(phase),
            version: 0,
            created_at: now,
            ready_at: None,
            last_active_at: now,
            expires_at: now + lifetime,
            released_at: None,
            busy: false,
            image: None,
            supervisor: None,
            release: None,
            pending: None,
            boot: Value::Object(serde_json::Map::new()),
        };
        t.execute(
            "insert into sandboxes (id, conversation_id, session_id, person, machine, state, phase, version, created_at,
                                    last_active_at, expires_at)
             values ($1, $2, $3, $4, $5, 'starting', $6, 0, $7, $7, $8)",
            &[
                &row.id,
                &row.conversation_id,
                &row.session_id,
                &row.person,
                &row.machine,
                &phase.to_string(),
                &now,
                &row.expires_at,
            ],
        )
        .await?;
        store(t, &mut row).await?;
        Ok(row)
    })
    .await
}

/// Ends `row` as `reason` (no write).
pub fn end(row: &mut SandboxRow, reason: ReleaseReason, message: &str) {
    let now = OffsetDateTime::now_utc();
    row.state = SandboxState::Gone;
    row.phase = None;
    row.busy = false;
    row.released_at = Some(now);
    row.pending = None;
    row.release = Some(Release {
        reason,
        note: None,
        code: reason
            .code()
            .unwrap_or(hermit_api::sandbox::ReleaseCode::SandboxReleased),
        message: message.chars().take(300).collect(),
    });
}

/// Changes one sandbox under its row lock; `f` answers whether it changed anything visible (then the version moves
/// and the feed gets a row). Answers the row as it is after.
///
/// # Errors
/// A statement error; `Ok(None)` for no such sandbox.
pub async fn update<F>(pool: &Pool, id: Uuid, f: F) -> Result<Option<SandboxRow>, aviary_pg::Error>
where
    F: Fn(&mut SandboxRow) -> bool + Send + Sync,
{
    let f = &f;
    aviary_pg::tx!(pool, |t| {
        let Some(mut row) = locked(t, id).await? else {
            return Ok(None);
        };
        if f(&mut row) {
            store(t, &mut row).await?;
        }
        Ok(Some(row))
    })
    .await
}

/// The feed after `after` (at most `limit`), and its head.
///
/// # Errors
/// A statement error.
pub async fn events(
    pool: &Pool,
    after: Option<u64>,
    limit: u32,
) -> Result<(u64, Vec<SandboxEvent>), aviary_pg::Error> {
    let c = pool.get().await?;
    let head: i64 = c
        .query_one("select coalesce(max(seq), 0) from sandbox_events", &[])
        .await?
        .get(0);
    let head = u64::try_from(head).unwrap_or(0);
    let Some(after) = after else {
        return Ok((head, Vec::new()));
    };
    let after = i64::try_from(after).unwrap_or(i64::MAX);
    let rows = c
        .query(
            "select seq, at, body from sandbox_events where seq > $1 order by seq limit $2",
            &[&after, &i64::from(limit)],
        )
        .await?;
    let events = rows
        .iter()
        .filter_map(|r| {
            let seq: i64 = r.get("seq");
            let mut e: SandboxEvent = serde_json::from_value(r.get("body")).ok()?;
            e.seq = u64::try_from(seq).ok()?;
            e.at = Timestamp::from_datetime(r.get("at"));
            Some(e)
        })
        .collect();
    Ok((head, events))
}
