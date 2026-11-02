//! The long-poll `GET /v1/conversations/{cid}/sandbox` (3.6.3) and the change feed `GET /v1/sandbox-events`
//! (3.6.5). A held request holds no database connection: it waits on the listener (10.4) and re-reads with one
//! short query when woken, or every second if a notification was missed, and answers at once when the replica
//! starts draining.

use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use hermit_api::Timestamp;
use hermit_api::error::{self, ApiError, codes};
use hermit_api::sandbox::{
    Cursor, MAX_WAIT_MS, Release, Sandbox, SandboxEvent, SandboxEvents, SandboxEventsQuery, SandboxQuery, SandboxState,
};
use prometheus_client::metrics::gauge::Gauge;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::Core;
use crate::db::{self, CHANNEL};
use crate::sandboxes::OrUnavailable;

/// At most this many held requests per replica (23): 503 `unavailable` above.
pub const MAX_HELD: usize = 2000;

static HELD: LazyLock<Gauge> = LazyLock::new(Gauge::default);

struct Held;

impl Held {
    fn enter() -> Result<Self, ApiError> {
        if HELD.inc() >= i64::try_from(MAX_HELD).unwrap_or(i64::MAX) {
            HELD.dec();
            return Err(ApiError::unavailable("too many held requests", 1));
        }
        Ok(Self)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        HELD.dec();
    }
}

/// How many requests this replica holds now (`hermit_held_requests`).
#[must_use]
pub fn held() -> usize {
    usize::try_from(HELD.get()).unwrap_or(0)
}

/// The gauge behind [`held`], for the ops registry.
#[must_use]
pub fn held_gauge() -> Gauge {
    HELD.clone()
}

fn wait_of(ms: Option<u32>) -> Result<Duration, ApiError> {
    let ms = ms.unwrap_or(0);
    if ms > MAX_WAIT_MS {
        return Err(ApiError::bad_request(format!("waitMs is at most {MAX_WAIT_MS}")));
    }
    Ok(Duration::from_millis(u64::from(ms)))
}

/// The sandbox a `GET` answers: the live one, else the most recent gone one that ended less than 10 min ago.
async fn current(core: &Core, cid: Uuid) -> Result<Option<Sandbox>, ApiError> {
    let row = db::latest_of(&core.pool, cid).await.or_503()?;
    Ok(row
        .filter(|r| {
            r.is_live()
                || r.released_at
                    .is_some_and(|at| OffsetDateTime::now_utc() - at < core.timings.gone_visible)
        })
        .map(|r| r.to_sandbox()))
}

/// `GET /v1/conversations/{cid}/sandbox?waitMs=&afterVersion=`.
///
/// # Errors
/// 404 `no_sandbox`, 400 for a `waitMs` over 25 000, 503.
pub async fn get_sandbox(core: &Arc<Core>, cid: Uuid, q: SandboxQuery) -> Result<Sandbox, ApiError> {
    let wait = wait_of(q.wait_ms)?;
    let mut w = core.listener.waiter(CHANNEL, &cid.to_string());
    let first = current(core, cid)
        .await?
        .ok_or_else(|| error::not_found(codes::NO_SANDBOX, "no sandbox for this conversation"))?;
    if wait.is_zero() {
        return Ok(first);
    }
    let after = q.after_version.unwrap_or(first.version);
    // a sandbox on its way (`starting`, or `releasing` towards `gone`) is held until it changes; any other state
    // answers at once (3.6.3 names only `starting`: a `releasing` answer at once would make magpie's loop, which
    // waits for `ready` or `gone`, spin)
    let settled =
        |s: &Sandbox| s.version > after || !matches!(s.state, SandboxState::Starting | SandboxState::Releasing);
    if settled(&first) {
        return Ok(first);
    }
    let _held = Held::enter()?;
    let end = tokio::time::Instant::now() + wait;
    let draining = crate::until_draining(core.draining.clone());
    tokio::pin!(draining);
    let mut last = first;
    loop {
        let tick = end.min(tokio::time::Instant::now() + core.timings.recheck);
        tokio::select! {
            () = w.changed() => {}
            () = tokio::time::sleep_until(tick) => {}
            () = &mut draining => return Ok(last),
        }
        if let Some(s) = current(core, cid).await? {
            last = s;
        }
        if settled(&last) || tokio::time::Instant::now() >= end {
            return Ok(last);
        }
    }
}

fn wire<T: serde::de::DeserializeOwned>(s: String) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s)).ok()
}

fn event_of(r: &aviary_pg::Row) -> Result<SandboxEvent, aviary_pg::Error> {
    let seq: i64 = r.try_get("seq")?;
    let at: OffsetDateTime = r.try_get("at")?;
    let state: String = r.try_get("state")?;
    let phase: Option<String> = r.try_get("phase")?;
    let version: i32 = r.try_get("version")?;
    let release: Option<serde_json::Value> = r.try_get("release")?;
    Ok(SandboxEvent {
        seq: u64::try_from(seq).unwrap_or(0),
        at: Timestamp::from(at),
        sandbox_id: r.try_get("sandbox_id")?,
        conversation_id: r.try_get("conversation_id")?,
        person: r.try_get("person")?,
        state: wire(state).unwrap_or(SandboxState::Unknown),
        phase: phase.and_then(wire),
        version: u64::try_from(version).unwrap_or(0),
        busy: r.try_get("busy")?,
        release: release.and_then(|v| serde_json::from_value::<Release>(v).ok()),
    })
}

/// The head (the newest seq ever issued, also when retention emptied the table) and the oldest kept seq.
async fn bounds(core: &Core) -> Result<(u64, Option<u64>), ApiError> {
    let c = core.pool.get().await.or_503()?;
    let r = c
        .query_one(
            "select (select max(seq) from sandbox_events), (select min(seq) from sandbox_events),
                    (select case when is_called then last_value else 0 end from sandbox_events_seq_seq)",
            &[],
        )
        .await
        .or_503()?;
    let max: Option<i64> = r.try_get(0).or_503()?;
    let min: Option<i64> = r.try_get(1).or_503()?;
    let last: i64 = r.try_get(2).or_503()?;
    let head = u64::try_from(max.unwrap_or(last)).unwrap_or(0);
    Ok((head, min.and_then(|m| u64::try_from(m).ok())))
}

/// `GET /v1/sandbox-events?after=&limit=&waitMs=`.
///
/// # Errors
/// 410 `cursor_expired` for a cursor older than the kept events, 400 for bad limits, 503.
pub async fn events(core: &Arc<Core>, q: SandboxEventsQuery) -> Result<SandboxEvents, ApiError> {
    let limit = q.limit.unwrap_or(200);
    if !(1..=500).contains(&limit) {
        return Err(ApiError::bad_request("limit is 1..500"));
    }
    let wait = wait_of(q.wait_ms)?;
    let mut w = core.listener.channel_waiter(CHANNEL);
    let (head, oldest) = bounds(core).await?;
    let after = match q.after {
        Cursor::Latest => {
            return Ok(SandboxEvents {
                head_seq: head,
                events: Vec::new(),
            });
        }
        Cursor::Seq(n) => n,
    };
    match oldest {
        Some(min) if after + 1 < min => {
            return Err(ApiError::cursor_expired("the cursor is older than the kept events"));
        }
        None if after < head => return Err(ApiError::cursor_expired("the cursor is older than the kept events")),
        _ => {}
    }
    let after_i = i64::try_from(after).unwrap_or(i64::MAX);
    let read = || async {
        let c = core.pool.get().await.or_503()?;
        let rows = c
            .query(
                "select * from sandbox_events where seq > $1 order by seq limit $2",
                &[&after_i, &i64::from(limit)],
            )
            .await
            .or_503()?;
        rows.iter().map(event_of).collect::<Result<Vec<_>, _>>().or_503()
    };
    let mut events = read().await?;
    if events.is_empty() && !wait.is_zero() {
        let _held = Held::enter()?;
        let end = tokio::time::Instant::now() + wait;
        let draining = crate::until_draining(core.draining.clone());
        tokio::pin!(draining);
        while events.is_empty() && tokio::time::Instant::now() < end {
            let tick = end.min(tokio::time::Instant::now() + core.timings.recheck);
            tokio::select! {
                () = w.changed() => {}
                () = tokio::time::sleep_until(tick) => {}
                () = &mut draining => break,
            }
            events = read().await?;
        }
    }
    let (head, _) = bounds(core).await?;
    Ok(SandboxEvents { head_seq: head, events })
}
