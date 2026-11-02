//! The janitor (hermit 12): the leader of lock 7300/3 runs a pass every 30 s, and retention (10.6) once a day at
//! 03:00 Hong Kong time. It records intent only on rows another task holds (10.3).

use std::sync::Arc;
use std::time::Duration;

use hermit_api::sandbox::ReleaseReason;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::db::{self, SandboxRow};
use crate::sandboxes;
use crate::supervisor_client;
use crate::tasks::{self, Kind};
use crate::{Core, ms};

/// What one pass did (tests read it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub idle: Vec<Uuid>,
    pub lifetime_soft: Vec<Uuid>,
    pub lifetime_forced: Vec<Uuid>,
    pub unhealthy: Vec<Uuid>,
    pub not_allowed: Vec<Uuid>,
    pub retried: Vec<Uuid>,
    pub stuck_starts: Vec<Uuid>,
    pub stuck_releases: Vec<Uuid>,
}

async fn rows(
    core: &Core,
    sql: &str,
    params: &[&(dyn aviary_pg::ToSql + Sync)],
) -> Result<Vec<SandboxRow>, aviary_pg::Error> {
    let c = core.pool.get().await?;
    c.query(sql, params).await?.iter().map(SandboxRow::from_row).collect()
}

/// One janitor pass (steps 1-7; orphans are the fabric's, step 9 is an operator act, retention runs daily).
///
/// # Errors
/// A database error (the pass stops; the next one starts over).
#[allow(clippy::too_many_lines)] // the steps of hermit 12 in order, one query each
pub async fn pass(core: &Arc<Core>) -> Result<Pass, aviary_pg::Error> {
    let mut out = Pass::default();
    let s = core.cfg.sandbox();
    let idle = i64::from(s.idle_minutes) * 60_000;
    let free = "(task is null or task_until < now())";

    // 1. idle
    for r in rows(
        core,
        &format!(
            "select * from sandboxes where status = 'ready' and not busy and release_pending is null and {free}
               and last_active_at < now() - $1::bigint * interval '1 millisecond'
               and (reserved_until is null or reserved_until < now())"
        ),
        &[&idle],
    )
    .await?
    {
        sandboxes::request(core, r.id, ReleaseReason::Idle, false, None).await?;
        out.idle.push(r.id);
    }
    // 2. lifetime: soft at expires_at when not busy, forced at the hard limit
    for r in rows(
        core,
        "select * from sandboxes where status = 'ready' and hard_expires_at < now()",
        &[],
    )
    .await?
    {
        // a forced or lost release already pending is never replaced by this one
        if r.release_pending
            .as_ref()
            .is_some_and(|p| sandboxes::ReleaseKind::of(p) != sandboxes::ReleaseKind::Soft)
        {
            continue;
        }
        sandboxes::request(core, r.id, ReleaseReason::Lifetime, true, None).await?;
        out.lifetime_forced.push(r.id);
    }
    for r in rows(
        core,
        &format!(
            "select * from sandboxes where status = 'ready' and not busy and release_pending is null and {free}
               and expires_at < now() and hard_expires_at >= now()"
        ),
        &[],
    )
    .await?
    {
        sandboxes::request(core, r.id, ReleaseReason::Lifetime, false, None).await?;
        out.lifetime_soft.push(r.id);
    }
    // 3. silence: only while the gateway is answering and settled
    if core.providers.fabric.gateway_settled() {
        let silence = ms(core.timings.silence);
        for r in rows(
            core,
            &format!(
                "select * from sandboxes where status = 'ready' and {free}
                   and coalesce(last_heartbeat_at, ready_at) < now() - $1::bigint * interval '1 millisecond'"
            ),
            &[&silence],
        )
        .await?
        {
            if silence_check(core, &r).await {
                out.unhealthy.push(r.id);
            }
        }
    }
    // 4. not allowed any more
    if let Some(cat) = core.catalogue.get() {
        for r in rows(
            core,
            "select * from sandboxes where status in ('ready', 'starting', 'paused')",
            &[],
        )
        .await?
        {
            let stronger = r
                .release_pending
                .as_ref()
                .is_some_and(|p| sandboxes::ReleaseKind::of(p) != sandboxes::ReleaseKind::Soft);
            if !cat.allowed(&r.person) && !stronger {
                sandboxes::request(core, r.id, ReleaseReason::NotAllowed, true, None).await?;
                out.not_allowed.push(r.id);
            }
        }
    }
    // 5. pending releases with no task running them
    for r in rows(
        core,
        &format!(
            "select * from sandboxes where status in ('ready', 'releasing') and release_pending is not null and {free}"
        ),
        &[],
    )
    .await?
    {
        if let Some(p) = &r.release_pending {
            // a requested release waits for a report that says idle (3.7.3)
            if p.reason == ReleaseReason::Requested && r.busy {
                continue;
            }
            sandboxes::request(core, r.id, p.reason, p.force, p.note.clone()).await?;
            out.retried.push(r.id);
        }
    }
    // 6. starts past their deadline with no live claim: the start task's own failure path ends them
    for r in rows(
        core,
        &format!("select * from sandboxes where status = 'starting' and start_deadline < now() and {free}"),
        &[],
    )
    .await?
    {
        crate::starts::end_stuck(core, r.id).await?;
        out.stuck_starts.push(r.id);
    }
    // 7. releasing for more than 10 minutes: destroy on every reachable host, then gone `stuck`
    let stuck = ms(core.timings.stuck_release);
    for r in rows(
        core,
        &format!(
            "select s.* from sandboxes s where s.status = 'releasing' and {free}
               and (select max(at) from sandbox_events e where e.sandbox_id = s.id and e.state = 'releasing')
                   < now() - $1::bigint * interval '1 millisecond'"
        ),
        &[&stuck],
    )
    .await?
    {
        if let Some(claim) = tasks::take(core, r.id, Kind::Release).await? {
            let rcode = r
                .release_pending
                .as_ref()
                .map_or(hermit_api::sandbox::ReleaseCode::SandboxReleased, db::Pending::code);
            let mut wide = r.clone();
            wide.host = None;
            sandboxes::destroy_and_gone(
                core,
                &claim,
                &wide,
                ReleaseReason::Stuck,
                None,
                rcode,
                "releasing for more than 10 minutes",
            )
            .await?;
            out.stuck_releases.push(r.id);
        }
    }
    Ok(out)
}

/// The silence rule (12 step 3): two health checks 5 s apart; any answer with `ok: true` keeps the sandbox (and
/// logs `supervisor not reporting`); two without an answer end it lost `unhealthy`. Returns whether it was ended.
pub async fn silence_check(core: &Arc<Core>, row: &SandboxRow) -> bool {
    if !core.providers.fabric.gateway_settled() {
        return false;
    }
    if supervisor_client::alive_twice(core, row, core.timings.health_gap).await {
        tracing::warn!(sandbox = %row.id, conversation = %row.conversation_id, "supervisor not reporting");
        return false;
    }
    match sandboxes::request(core, row.id, ReleaseReason::Unhealthy, true, None).await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(sandbox = %row.id, error = %e, "unhealthy release request failed");
            false
        }
    }
}

/// Retention (10.6).
///
/// # Errors
/// A database error.
pub async fn retention(core: &Core) -> Result<u64, aviary_pg::Error> {
    let c = core.pool.get().await?;
    let mut n = 0;
    for sql in [
        "delete from sandboxes where status = 'gone' and released_at < now() - interval '400 days'",
        "delete from sandbox_events where at < now() - interval '7 days'",
        "delete from sandbox_starts where at < now() - interval '30 days'",
        "delete from host_events where at < now() - interval '400 days'",
        "delete from hosts where status = 'gone' and gone_at < now() - interval '400 days'",
        "delete from sandbox_logs where captured_at < now() - interval '14 days'",
        "delete from demand_hints where at < now() - interval '1 hour'",
        "delete from replicas where seen_at < now() - interval '1 day'",
    ] {
        n += c.execute(sql, &[]).await?;
    }
    Ok(n)
}

/// Hong Kong is UTC+8 all year (no daylight saving since 1979).
const HONG_KONG: UtcOffset = match UtcOffset::from_hms(8, 0, 0) {
    Ok(o) => o,
    Err(_) => UtcOffset::UTC,
};

/// Whether retention is due: past 03:00 Hong Kong time on a day it has not run yet.
#[must_use]
pub fn retention_due(last_day: Option<time::Date>, now: OffsetDateTime) -> Option<time::Date> {
    let local = now.to_offset(HONG_KONG);
    (local.hour() >= 3 && last_day != Some(local.date())).then(|| local.date())
}

/// The janitor's loop on the leader (runs until the task is aborted).
pub async fn run(core: Arc<Core>, leader: Arc<aviary_pg::Leader>) {
    let mut last_retention = None;
    loop {
        tokio::time::sleep(core.timings.janitor).await;
        if core.is_draining() || !leader.confirm_key(&db::LOCK_JANITOR).await {
            continue;
        }
        match pass(&core).await {
            Ok(p) if p != Pass::default() => tracing::info!(?p, "janitor pass"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "janitor pass failed"),
        }
        if let Some(day) = retention_due(last_retention, OffsetDateTime::now_utc()) {
            match retention(&core).await {
                Ok(n) => {
                    tracing::info!(rows = n, "retention");
                    last_retention = Some(day);
                }
                Err(e) => tracing::warn!(error = %e, "retention failed"),
            }
        }
    }
}

/// A short pause between leader checks.
pub const LEADER_POLL: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn retention_runs_once_a_day_after_three_hong_kong_time() {
        // 18:59 UTC = 02:59 HKT
        assert_eq!(retention_due(None, datetime!(2026-11-01 18:59 UTC)), None);
        let day = retention_due(None, datetime!(2026-11-01 19:00 UTC)).unwrap_or(time::macros::date!(2000 - 01 - 01));
        assert_eq!(day, time::macros::date!(2026 - 11 - 02));
        assert_eq!(retention_due(Some(day), datetime!(2026-11-02 10:00 UTC)), None);
        assert!(retention_due(Some(day), datetime!(2026-11-02 19:01 UTC)).is_some());
    }
}
