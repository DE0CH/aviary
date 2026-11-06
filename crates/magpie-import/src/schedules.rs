//! Schedules of the old stack (claudecodeui `jarvis/schedules.mjs`; Deyao, 2026-11-06: nobody loses one when moved).
//! A conversation's rows of `cc_shared.schedules` and `cc_shared.schedule_runs` go with it at its release, with the
//! same ids, rule, reading, next time, state and runs, so magpie fires them from then on (a time the hand-over passed
//! is fired within 10 minutes or recorded skipped, as after any outage). Until its release a conversation's schedules
//! are the old stack's: it fires them, and pigeon reads them from the old child.
//!
//! The hand-over of the rows is one step with no window in which both stacks, or neither, may fire one ([`claim`]):
//! a transaction on the old database takes the rows (`for update`, so it waits for a fire the old router is making:
//! that one claims its row the same way and moves `next_at` on in its own transaction), marks them `moved` and stays
//! open around magpie's release transaction, which carries what it read ([`carry`]). magpie's commit is followed by
//! the old one's; a release that does not happen rolls the old one back and the rows are the old stack's as before.
//! A schedule the old router fired less than the quiet period ago means the conversation is in use (its message may
//! be starting a turn the release's checks did not see): the release is left for a later pass ([`Abort::Fired`]).
//! Only an old commit that fails after magpie's committed leaves the rows to [`mark_moved`] (retried; [`mismatches`],
//! the verification of a released conversation, reports a row still not `moved`). The old database is written on
//! [`crate::Importer::cloudcli_schedules`], the one writable connection of the hand-over (every other is read-only).

use aviary_pg::{Pool, Row, Tx};
use magpie::legacy::fence;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::Result;

/// One old schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldSchedule {
    /// `sch_…`, kept.
    pub id: String,
    /// The owner.
    pub person: String,
    /// `once` or `cron`.
    pub kind: String,
    /// The message.
    pub prompt: String,
    /// A one-off's time.
    pub at: Option<OffsetDateTime>,
    /// A repeating rule.
    pub cron: Option<String>,
    /// The zone.
    pub tz: String,
    /// A rule's end.
    pub until: Option<OffsetDateTime>,
    /// The reading.
    pub describe: String,
    /// The next fire.
    pub next_at: Option<OffsetDateTime>,
    /// `active`, `paused`, `done`, `deleted` (`moved` rows are not read).
    pub state: String,
    /// Set at.
    pub created_at: OffsetDateTime,
    /// Set by.
    pub created_by: String,
    /// Its runs: `(due, at, outcome, detail)`.
    pub runs: Vec<(OffsetDateTime, OffsetDateTime, String, Option<String>)>,
}

async fn exists(pool: &Pool) -> Result<bool> {
    Ok(pool
        .get()
        .await?
        .query_one("select to_regclass('cc_shared.schedules') is not null", &[])
        .await?
        .get(0))
}

/// Why a release that took the old schedules did not happen: the old database's transaction rolls back, so they stay
/// the old stack's.
#[derive(Debug)]
pub enum Abort {
    /// The old router fired one of them less than the quiet period ago, or is firing one now (its row stayed locked
    /// past the lock timeout): the conversation is in use.
    Fired,
    /// magpie's release refused.
    NotReleased(fence::NotReleased),
    /// A statement failed.
    Failed(crate::Error),
}

impl From<aviary_pg::Error> for Abort {
    fn from(e: aviary_pg::Error) -> Self {
        Self::Failed(e.into())
    }
}

/// How long [`claim`] waits for a row the old router holds (a fire delivering its message).
pub const LOCK_WAIT: &str = "10s";

/// The old schedules of conversation `id`, taken for its release in `old`, a transaction on the old database that
/// stays open until magpie's release transaction has ended: each row locked (waiting for a fire in flight), its runs
/// read, and marked `moved` (so the old router fires none of them again once `old` commits). `last_fire` is the
/// newest run's time and `now` the old database's clock.
///
/// # Errors
/// [`Abort::Fired`] when a row stayed locked past [`LOCK_WAIT`]; [`Abort::Failed`] for any other statement error.
pub async fn claim(old: &Tx<'_>, id: Uuid) -> std::result::Result<Claimed, Abort> {
    let now: OffsetDateTime = old.query_one("select now()", &[]).await?.get(0);
    let exists: bool = old
        .query_one("select to_regclass('cc_shared.schedules') is not null", &[])
        .await?
        .get(0);
    if !exists {
        return Ok(Claimed {
            schedules: Vec::new(),
            last_fire: None,
            now,
        });
    }
    old.batch_execute(&format!("set local lock_timeout = '{LOCK_WAIT}'"))
        .await?;
    let busy = |e: aviary_pg::Error| {
        if e.code() == Some(&aviary_pg::reexports::tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE) {
            Abort::Fired
        } else {
            Abort::from(e)
        }
    };
    let conversation = id.to_string();
    let rows = old
        .query(
            "select id, person, kind, prompt, at, cron, tz, until, describe, next_at, state, created_at, created_by
             from cc_shared.schedules where conversation_id = $1 and state <> 'moved' order by created_at, id
             for update",
            &[&conversation],
        )
        .await
        .map_err(busy)?;
    let ids: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
    let runs = old
        .query(
            "select schedule_id, due_at, at, outcome, detail from cc_shared.schedule_runs
             where schedule_id = any($1) order by schedule_id, due_at for update",
            &[&ids],
        )
        .await
        .map_err(busy)?;
    old.execute(
        "update cc_shared.schedules set state = 'moved', next_at = null, updated_at = now()
         where conversation_id = $1 and state <> 'moved'",
        &[&conversation],
    )
    .await?;
    let last_fire = runs.iter().map(|r| r.get::<_, OffsetDateTime>(2)).max();
    let schedules = rows.iter().map(|r| schedule_of(r, &runs)).collect();
    Ok(Claimed {
        schedules,
        last_fire,
        now,
    })
}

/// One row of `cc_shared.schedules` with its runs (rows of `schedule_id, due_at, at, outcome, detail`).
fn schedule_of(r: &Row, runs: &[Row]) -> OldSchedule {
    let id: String = r.get(0);
    OldSchedule {
        runs: runs
            .iter()
            .filter(|x| x.get::<_, String>(0) == id)
            .map(|x| (x.get(1), x.get(2), x.get(3), x.get(4)))
            .collect(),
        id,
        person: r.get(1),
        kind: r.get(2),
        prompt: r.get(3),
        at: r.get(4),
        cron: r.get(5),
        tz: r.get(6),
        until: r.get(7),
        describe: r.get(8),
        next_at: r.get(9),
        state: r.get(10),
        created_at: r.get(11),
        created_by: r.get(12),
    }
}

/// What [`claim`] took.
#[derive(Debug, Clone)]
pub struct Claimed {
    /// The rows, as they were before `moved`.
    pub schedules: Vec<OldSchedule>,
    /// The newest run of any of them (a fire), by the old database's clock.
    pub last_fire: Option<OffsetDateTime>,
    /// The old database's clock at the claim.
    pub now: OffsetDateTime,
}

impl Claimed {
    /// One of them fired less than `quiet` ago.
    #[must_use]
    pub fn fired_within(&self, quiet: time::Duration) -> bool {
        self.last_fire.is_some_and(|t| self.now - t < quiet)
    }
}

/// The outcomes magpie keeps (an old outcome it does not know is kept as `failed` with the old word as the detail):
/// the answered states are the old process's record of the answer (magpie has no turn of the run to read it from).
const OUTCOMES: [&str; 9] = [
    "sent",
    "sent_mid_turn",
    "answered",
    "answered_mid_turn",
    "unanswered",
    "skipped_late",
    "skipped_paused",
    "skipped_deleted",
    "failed",
];

/// Writes `schedules` of conversation `id` (owner `person`, as [`claim`] took them) into magpie, in the release's
/// transaction: the same rows, replaced if a previous attempt wrote them; runs added once each.
///
/// # Errors
/// A statement error.
pub async fn carry(t: &Tx<'_>, person: &str, id: Uuid, schedules: &[OldSchedule]) -> Result<()> {
    for s in schedules {
        let state = if matches!(s.state.as_str(), "active" | "paused" | "done" | "deleted") {
            s.state.as_str()
        } else {
            "done"
        };
        let next = if state == "active" { s.next_at } else { None };
        t.execute(
            "insert into schedules (id, person, conversation_id, kind, prompt, at, cron, tz, until, describe, next_at,
                                    state, created_at, created_by)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             on conflict (id) do update set prompt = excluded.prompt, kind = excluded.kind, at = excluded.at,
               cron = excluded.cron, tz = excluded.tz, until = excluded.until, describe = excluded.describe,
               next_at = excluded.next_at, state = excluded.state, updated_at = now()",
            &[
                &s.id,
                &person,
                &id,
                &s.kind,
                &s.prompt,
                &s.at,
                &s.cron,
                &s.tz,
                &s.until,
                &s.describe,
                &next,
                &state,
                &s.created_at,
                &s.created_by,
            ],
        )
        .await?;
        for (due, at, outcome, detail) in &s.runs {
            let (outcome, detail) = if OUTCOMES.contains(&outcome.as_str()) {
                (outcome.clone(), detail.clone())
            } else {
                ("failed".to_owned(), Some(outcome.clone()))
            };
            t.execute(
                "insert into schedule_runs (schedule_id, due_at, at, outcome, conversation_id, detail)
                 values ($1, $2, $3, $4, $5, $6) on conflict (schedule_id, due_at) do nothing",
                &[&s.id, due, at, &outcome, &id, &detail],
            )
            .await?;
        }
    }
    if !schedules.is_empty() {
        tracing::info!(conversation = %id, schedules = schedules.len(), "schedules carried over");
    }
    Ok(())
}

/// Marks conversation `id`'s old schedules `moved` on `pool` (writable), for a release whose old commit failed after
/// magpie's had committed ([`claim`]): retried 5 times over about 30 s; a failure is logged and [`mismatches`] reports
/// the rows.
pub async fn mark_moved(pool: &Pool, id: Uuid) {
    let mut wait = std::time::Duration::from_secs(2);
    for attempt in 1..=5 {
        let r = async {
            if !exists(pool).await? {
                return Ok::<u64, crate::Error>(0);
            }
            Ok(pool
                .get()
                .await?
                .execute(
                    "update cc_shared.schedules set state = 'moved', next_at = null, updated_at = now()
                     where conversation_id = $1 and state <> 'moved'",
                    &[&id.to_string()],
                )
                .await?)
        }
        .await;
        match r {
            Ok(n) => {
                tracing::info!(conversation = %id, schedules = n, "old schedules marked moved");
                return;
            }
            Err(e) if attempt < 5 => {
                tracing::warn!(conversation = %id, error = %e, attempt, "old schedules not marked moved; retrying");
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
            Err(e) => tracing::error!(conversation = %id, error = %e, "old schedules not marked moved"),
        }
    }
}

/// A released conversation's schedules that did not move: `schedule:<id>` for each old row magpie does not have
/// with the same rule and message, or that the old router may still fire (not `moved`).
///
/// # Errors
/// A statement error.
pub async fn mismatches(imp: &crate::Importer, id: Uuid) -> Result<Vec<String>> {
    if !exists(&imp.cloudcli).await? {
        return Ok(Vec::new());
    }
    let old = imp
        .cloudcli
        .get()
        .await?
        .query(
            "select id, prompt, coalesce(cron, ''), tz, state from cc_shared.schedules where conversation_id = $1",
            &[&id.to_string()],
        )
        .await?;
    let conn = imp.magpie.pool.get().await?;
    let mut out = Vec::new();
    for r in old {
        let sid: String = r.get(0);
        let (prompt, cron, tz, state): (String, String, String, String) = (r.get(1), r.get(2), r.get(3), r.get(4));
        let found = conn
            .query_opt(
                "select prompt, coalesce(cron, ''), tz from schedules where id = $1 and conversation_id = $2",
                &[&sid, &id],
            )
            .await?;
        let same = found.is_some_and(|m| {
            m.get::<_, String>(0) == prompt && m.get::<_, String>(1) == cron && m.get::<_, String>(2) == tz
        });
        if !same || state != "moved" {
            out.push(format!("schedule:{sid}"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::OUTCOMES;

    #[test]
    fn the_carried_outcomes_are_magpies() {
        // magpie's schedule_runs check (its latest form, migration 0027) has every one
        let check = include_str!("../../magpie/src/db/migrations/0027_schedule_answers.sql");
        for o in OUTCOMES {
            assert!(check.contains(&format!("'{o}'")), "{o}");
        }
    }
}
