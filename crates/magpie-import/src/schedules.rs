//! Schedules of the old stack (claudecodeui `jarvis/schedules.mjs`; Deyao, 2026-11-06: nobody loses one when moved).
//! A conversation's rows of `cc_shared.schedules` and `cc_shared.schedule_runs` go with it at its release, in the
//! release's transaction ([`carry`]), with the same ids, rule, reading, next time, state and runs, so magpie fires
//! them from then on (a time the hand-over passed is fired within 10 minutes or recorded skipped, as after any
//! outage). Then the old rows become `moved` ([`mark_moved`]), so the old router never fires them again. Until its
//! release a conversation's schedules are the old stack's: it fires them, and pigeon reads them from the old child.
//! [`mismatches`] is the verification of a released conversation: every old row it had is magpie's.

use aviary_pg::{Pool, Tx};
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

/// The old schedules of conversation `id` (none before the table exists).
///
/// # Errors
/// A statement error.
pub async fn read(pool: &Pool, id: Uuid) -> Result<Vec<OldSchedule>> {
    if !exists(pool).await? {
        return Ok(Vec::new());
    }
    let c = pool.get().await?;
    let rows = c
        .query(
            "select id, person, kind, prompt, at, cron, tz, until, describe, next_at, state, created_at, created_by
             from cc_shared.schedules where conversation_id = $1 and state <> 'moved' order by created_at, id",
            &[&id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let sid: String = r.get(0);
        let runs: Vec<(OffsetDateTime, OffsetDateTime, String, Option<String>)> = c
            .query(
                "select due_at, at, outcome, detail from cc_shared.schedule_runs where schedule_id = $1 order by due_at",
                &[&sid],
            )
            .await?
            .iter()
            .map(|x| (x.get(0), x.get(1), x.get(2), x.get(3)))
            .collect();
        out.push(OldSchedule {
            id: sid,
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
            runs,
        });
    }
    Ok(out)
}

/// The outcomes magpie keeps (an old outcome it does not know is kept as `failed` with the old word as the detail).
const OUTCOMES: [&str; 6] = [
    "sent",
    "sent_mid_turn",
    "skipped_late",
    "skipped_paused",
    "skipped_deleted",
    "failed",
];

/// Writes `schedules` of conversation `id` (owner `person`) into magpie, in the release's transaction: the same
/// rows, replaced if a previous attempt wrote them; runs added once each.
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

/// Marks conversation `id`'s old schedules `moved` (after its release committed): the old router fires none of them
/// again. A failure is logged; the next release attempt or the operator runs it again ([`mismatches`] reports it).
pub async fn mark_moved(pool: &Pool, id: Uuid) {
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
        Ok(0) => {}
        Ok(n) => tracing::info!(conversation = %id, schedules = n, "old schedules marked moved"),
        Err(e) => tracing::warn!(conversation = %id, error = %e, "old schedules not marked moved"),
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
        // magpie's schedule_runs check (migration 0026) has these and the mid-turn delivery's own states
        for o in OUTCOMES {
            assert!(
                [
                    "sent",
                    "pending",
                    "sent_mid_turn",
                    "redeliver",
                    "skipped_late",
                    "skipped_paused",
                    "skipped_deleted",
                    "failed"
                ]
                .contains(&o)
            );
        }
    }
}
