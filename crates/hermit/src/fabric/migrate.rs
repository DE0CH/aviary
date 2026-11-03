//! Live migration as a resumable `migrate` task (hermit 13.10). The scaler records the intent with the claim
//! ([`super::scaler::start_migration`]); this task then asks the source for the spec, prepares the target, sends,
//! and waits for the target's restore, every call outside a transaction and every result written only while the
//! claim is this replica's. A replica that takes the claim over resumes from the row: with `migrating_to` set it
//! asks the target where the receive stands (`GET …/receive` without `wait`) and either waits for it, adopts it, or
//! settles by asking both hosts, so a sandbox that finished moving while no replica watched is adopted on the
//! target, never declared vanished from its source, and the target's copy is never swept as an orphan.

use std::sync::Arc;
use std::time::Instant;

use hermit_api::sandbox::{ReleaseCode, ReleaseReason};
use sandboxd_api::{Client, ReceiveResult, SendRequest};
use uuid::Uuid;

use crate::db::{self, SandboxRow};
use crate::fabric::FabricError;
use crate::fabric::gvisor::push_gateway_peers;
use crate::fabric::hosts::{self, HostKind, HostRow, HostStatus};
use crate::fabric::sandboxd::{self, Clients};
use crate::sandboxes;
use crate::tasks::{self, Claim};
use crate::{Core, ms};

/// Where the moving part ended.
#[derive(Debug)]
enum Outcome {
    /// The target restored it (`stats`: runsc's restore statistics).
    Moved(Option<serde_json::Value>),
    /// The source refused (409: it would not converge); it still runs there.
    NotMoved(String),
    /// Unknown: ask both hosts.
    Settle(String),
    /// The claim is not this replica's any more.
    Stop,
}

/// The migrate task of a claim this replica holds; `fresh` when the scaler on this replica just recorded the
/// intent (the task starts at step 2), else a takeover.
pub async fn run(core: Arc<Core>, claim: Claim, fresh: bool, clients: Clients) {
    match steps(&core, &claim, fresh, &clients).await {
        Ok(()) => after(&core, claim.id).await,
        Err(e) => {
            tracing::warn!(sandbox = %claim.id, error = %e, "migration interrupted (resumed by the takeover sweep)");
        }
    }
}

fn live_host(h: Option<HostRow>) -> Option<HostRow> {
    h.filter(|h| h.status != HostStatus::Gone)
}

async fn steps(core: &Arc<Core>, claim: &Claim, fresh: bool, clients: &Clients) -> Result<(), FabricError> {
    let Some(row) = db::load(&core.pool, claim.id).await? else {
        return Ok(());
    };
    if !row.is_live() {
        return Ok(());
    }
    let (Some(from_name), Some(to_name)) = (row.host.clone(), row.migrating_to.clone()) else {
        // no intent recorded: nothing was sent
        tasks::clear(core, claim).await?;
        return Ok(());
    };
    let from = live_host(hosts::get(&core.pool, &from_name).await?);
    let to = live_host(hosts::get(&core.pool, &to_name).await?);
    let reason = start_reason(core, row.id).await;
    let t0 = Instant::now();
    let outcome = if fresh {
        forward(core, claim, &row, from.as_ref(), to.as_ref(), clients).await
    } else {
        resume(claim, &row, to.as_ref(), clients).await
    };
    let data = |extra: serde_json::Value| {
        let mut d = serde_json::json!({"from": from_name, "to": to_name, "reason": reason, "ms": ms(t0.elapsed())});
        if let (Some(d), serde_json::Value::Object(x)) = (d.as_object_mut(), extra) {
            d.extend(x);
        }
        d
    };
    match outcome {
        Outcome::Stop => Ok(()),
        Outcome::Moved(stats) => {
            commit(
                core,
                claim,
                &row,
                &to_name,
                "migration",
                data(stats_data(stats.as_ref())),
                clients,
            )
            .await
        }
        Outcome::NotMoved(why) => {
            let Some(_effect) = claim.guard(core).await? else {
                return Ok(());
            };
            if let Some(t) = &to {
                delete(clients, t, &row.name).await;
            }
            clear_intent(
                core,
                claim,
                &row,
                "migration-skipped",
                data(serde_json::json!({"error": db::clip(&why, 300)})),
            )
            .await
        }
        Outcome::Settle(why) => settle(core, claim, &row, from.as_ref(), to.as_ref(), clients, &why, &data).await,
    }
}

/// Steps 2-5 of a fresh migration.
async fn forward(
    core: &Core,
    claim: &Claim,
    row: &SandboxRow,
    from: Option<&HostRow>,
    to: Option<&HostRow>,
    clients: &Clients,
) -> Outcome {
    let (Some(from), Some(to)) = (from, to) else {
        return Outcome::Settle("a host is gone".into());
    };
    let (Ok(src), Ok(dst)) = (clients.of(from), clients.of(to)) else {
        return Outcome::Settle("no client".into());
    };
    if !claim.held() {
        return Outcome::Stop;
    }
    // 2. the spec, 3. the receive
    let spec = match src.spec(&row.name).await {
        Ok(s) => s,
        Err(e) => return Outcome::Settle(format!("spec: {e}")),
    };
    if !claim.held() {
        return Outcome::Stop;
    }
    let rx = match dst.receive(&spec).await {
        Ok(r) => r,
        Err(e) => return Outcome::Settle(format!("receive: {e}")),
    };
    if !claim.held() {
        return Outcome::Stop;
    }
    // 4. the send; the cross-cloud limit only between hosts of different kinds
    let m = &core.cfg.file.fabric.migration;
    let req = SendRequest {
        to: format!("{}:{}", rx.address, rx.port),
        downtime_ms: Some(u64::from(m.downtime_ms)),
        max_rounds: Some(u64::from(m.max_rounds)),
        max_downtime_ms: Some(if from.kind == to.kind {
            0
        } else {
            u64::from(m.max_downtime_ms)
        }),
    };
    match src.send(&row.name, &req).await {
        Ok(_) => {}
        Err(e) if sandboxd::status(&e) == Some(409) => return Outcome::NotMoved(e.to_string()),
        // the target may have it all the same: its receive says
        Err(e) => tracing::warn!(sandbox = %row.id, error = %e, "send failed (asking the target)"),
    }
    wait(claim, row, &dst).await
}

/// Step 5: the target's receive.
async fn wait(claim: &Claim, row: &SandboxRow, dst: &Client) -> Outcome {
    if !claim.held() {
        return Outcome::Stop;
    }
    match dst.receive_result(&row.name, sandboxd_api::RECEIVE_WAIT_MS).await {
        Ok(ReceiveResult::Running { stats }) => Outcome::Moved(stats),
        Ok(other) => Outcome::Settle(format!("receive: {other:?}")),
        Err(e) => Outcome::Settle(format!("receive: {e}")),
    }
}

/// A takeover: where does the target's receive stand (13.2 item 11)?
async fn resume(claim: &Claim, row: &SandboxRow, to: Option<&HostRow>, clients: &Clients) -> Outcome {
    let Some(dst) = to.and_then(|t| clients.of(t).ok()) else {
        return Outcome::Settle("the target is gone".into());
    };
    match dst.receive_status(&row.name).await {
        Ok(ReceiveResult::Receiving) => wait(claim, row, &dst).await,
        Ok(ReceiveResult::Running { stats }) => Outcome::Moved(stats),
        Ok(other) => Outcome::Settle(format!("taken over: {other:?}")),
        Err(e) => Outcome::Settle(format!("taken over: {e}")),
    }
}

/// What a host lists of the sandbox in a settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// Running and alive, no send of it in progress.
    Alive,
    /// A source whose send is still in progress (`sending`; `alive`: not yet stopped by its checkpoint): it may still
    /// switch, so it decides nothing until the send ends (sandboxd bounds it: its rsync and checkpoint timeouts).
    Sending { alive: bool },
    /// A receive still in progress (it ends on its own: restored, failed, the relay's 900 s, or a restore past
    /// sandboxd's limit after End).
    Receiving,
    /// Not there, or there but not alive (a source its send's checkpoint stopped lists `alive: false`).
    No,
}

/// What `host` lists of `name`: `None` when the host does not answer (a gone host lists nothing).
async fn lists(clients: &Clients, host: Option<&HostRow>, name: &str) -> Option<Seen> {
    let Some(h) = host else {
        return Some(Seen::No);
    };
    let l = clients.of(h).ok()?.list().await.ok()?;
    Some(seen(&l.sandboxes, name))
}

fn seen(listed: &[sandboxd_api::Listed], name: &str) -> Seen {
    match listed.iter().find(|s| s.name == name) {
        Some(s) if s.state == sandboxd_api::SandboxState::Running && s.sending => Seen::Sending {
            alive: s.alive != Some(false),
        },
        Some(s) if s.state == sandboxd_api::SandboxState::Running && s.alive != Some(false) => Seen::Alive,
        Some(s) if s.state == sandboxd_api::SandboxState::Receiving => Seen::Receiving,
        _ => Seen::No,
    }
}

/// Step 6: settle by asking both hosts; retried while one does not answer, the source's send or the target's receive
/// is still in progress (and the claim holds). The source decides "kept" only while it lists the sandbox alive with
/// no send of it running: sandboxd lists a source `sending` for its whole send and `alive: false` from the moment its
/// checkpoint stopped it, so a target that may hold the only copy is never deleted on a stale reading, and a send or
/// receive in progress is waited for rather than read as kept or lost. Both end on their own (sandboxd's limits); a
/// migration older than [`crate::Timings::migration_limit`] (from `migration_started_at`) is ended all the same: the
/// target's receive is deleted, then the source decides (alive: kept, even while its send still runs, which can no
/// longer switch to a deleted receive, once the target lists no receive; else lost).
#[allow(clippy::too_many_arguments)]
async fn settle(
    core: &Arc<Core>,
    claim: &Claim,
    row: &SandboxRow,
    from: Option<&HostRow>,
    to: Option<&HostRow>,
    clients: &Clients,
    why: &str,
    data: &(dyn Fn(serde_json::Value) -> serde_json::Value + Sync),
) -> Result<(), FabricError> {
    let to_name = row.migrating_to.clone().unwrap_or_default();
    let mut ended = false;
    loop {
        if !claim.held() {
            return Ok(());
        }
        // past the limit a receive that restored is still adopted (below); any other is ended first
        if !ended && overdue(core, row.id).await && lists(clients, to, &row.name).await != Some(Seen::Alive) {
            let Some(_effect) = claim.guard(core).await? else {
                return Ok(());
            };
            tracing::warn!(sandbox = %row.id, why, "migration past its limit: the target's receive is deleted");
            if let Some(t) = to {
                delete(clients, t, &row.name).await;
            }
            ended = true;
            continue;
        }
        // a source lists no receive of its own sandbox; if it did, that is not the sandbox running there
        let listed = lists(clients, from, &row.name).await;
        // past the limit with its send still running: kept only once the receive it could switch to is gone
        let still_sending = ended && listed == Some(Seen::Sending { alive: true });
        let source = listed.map(|s| match s {
            Seen::Receiving => Seen::No,
            Seen::Sending { alive: true } if ended => Seen::Alive,
            Seen::Sending { alive: false } if ended => Seen::No,
            s => s,
        });
        if source == Some(Seen::Alive) {
            let Some(effect) = claim.guard(core).await? else {
                return Ok(());
            };
            if let Some(t) = to {
                delete(clients, t, &row.name).await;
                // a delete that did not land (or a target that does not answer) leaves a receive the running send
                // could still switch to, and the orphan sweep would then end the only copy: delete again, ask again
                if still_sending && lists(clients, Some(t), &row.name).await != Some(Seen::No) {
                    drop(effect);
                    tracing::warn!(sandbox = %row.id, "the target still lists the receive past the limit: deleted again");
                    tokio::time::sleep(core.timings.release_retry).await;
                    continue;
                }
            }
            tracing::warn!(sandbox = %row.id, why, "migration failed, the sandbox stays on its source");
            return clear_intent(
                core,
                claim,
                row,
                "migration-failed",
                // overdue: a send that still switched loses it on the source, which the vanished check then sees
                data(serde_json::json!({"kept": true, "overdue": ended, "error": db::clip(why, 300)})),
            )
            .await;
        }
        let target = lists(clients, to, &row.name).await.map(|s| match s {
            // a target lists no send of the sandbox it receives
            Seen::Sending { .. } => Seen::No,
            s => s,
        });
        if target == Some(Seen::Alive) {
            return commit(
                core,
                claim,
                row,
                &to_name,
                "migration-adopted",
                data(serde_json::json!({"error": db::clip(why, 300)})),
                clients,
            )
            .await;
        }
        if source == Some(Seen::No) && target == Some(Seen::No) {
            tracing::error!(sandbox = %row.id, why, "migration lost the sandbox");
            hosts::event(
                &core.pool,
                "migration-failed",
                row.host.as_deref(),
                Some(row.id),
                data(serde_json::json!({"kept": false, "overdue": ended, "error": db::clip(why, 300)})),
            )
            .await;
            let latest = db::load(&core.pool, row.id).await?.unwrap_or_else(|| row.clone());
            sandboxes::destroy_and_gone(
                core,
                claim,
                &latest,
                ReleaseReason::MigrationFailed,
                None,
                ReleaseCode::SandboxLost,
                "the sandbox was lost in a live migration",
            )
            .await?;
            return Ok(());
        }
        // past the limit, a receive still listed is one whose delete did not land: delete it again
        if ended
            && target == Some(Seen::Receiving)
            && let Some(t) = to
        {
            let Some(_effect) = claim.guard(core).await? else {
                return Ok(());
            };
            delete(clients, t, &row.name).await;
        }
        // a host that does not answer, a send or a receive still in progress decides nothing: ask again (the
        // host-loss rule may end it meanwhile, and the migration limit bounds it)
        tokio::time::sleep(core.timings.release_retry).await;
    }
}

/// The migration started longer ago than [`crate::Timings::migration_limit`] (`migration_started_at`); false when
/// that is not known (the database does not answer: asked again on the next round).
async fn overdue(core: &Core, id: Uuid) -> bool {
    let limit = core.timings.migration_limit.as_secs_f64();
    let r = async {
        core.pool
            .get()
            .await?
            .query_opt(
                "select extract(epoch from now() - migration_started_at)::float8 > $2
                   from sandboxes where id = $1 and migration_started_at is not null",
                &[&id, &limit],
            )
            .await
    }
    .await;
    r.ok()
        .flatten()
        .and_then(|r| r.try_get::<_, Option<bool>>(0).ok().flatten())
        .unwrap_or(false)
}

async fn delete(clients: &Clients, h: &HostRow, name: &str) {
    if let Ok(c) = clients.of(h)
        && let Err(e) = c.delete(name).await
        && sandboxd::status(&e) != Some(404)
    {
        tracing::warn!(host = %h.name, sandbox = name, error = %e, "target copy not deleted (the orphan sweep ends it)");
    }
}

/// Steps 5 and 6's adopt: the row moves to `to` in one transaction under the claim (claim cleared, `bump`), then the
/// gateway's peers are pushed.
async fn commit(
    core: &Core,
    claim: &Claim,
    row: &SandboxRow,
    to: &str,
    kind: &str,
    data: serde_json::Value,
    clients: &Clients,
) -> Result<(), FabricError> {
    let (id, owner, to_s, kind_s, from) = (
        row.id,
        claim.owner.clone(),
        to.to_owned(),
        kind.to_owned(),
        row.host.clone(),
    );
    let moved = aviary_pg::tx!(&core.pool, |t| -> Result<Option<SandboxRow>, aviary_pg::Error> {
        let n = t
            .execute(
                "update sandboxes set host = $2, migrating_to = null, migration_started_at = null,
                   task = null, task_owner = null, task_until = null
                 where id = $1 and status <> 'gone' and task = 'migrate' and task_owner = $3",
                &[&id, &to_s, &owner],
            )
            .await?;
        if n == 0 {
            return Ok(None);
        }
        t.execute(
            "insert into host_events (kind, host, sandbox_id, data) values ($1, $2, $3, $4)",
            &[&kind_s, &to_s, &id, &data],
        )
        .await?;
        Ok(Some(db::bump(t, id).await?))
    })
    .await?;
    let Some(r) = moved else {
        claim.mark_lost();
        return Ok(());
    };
    core.live.apply(&r);
    tracing::info!(sandbox = %id, from = ?from, to, kind, "sandbox migrated");
    if let Err(e) = push_gateway_peers(clients, core).await {
        tracing::warn!(sandbox = %id, error = %e, "gateway peers after a migration (the next pass pushes them)");
    }
    Ok(())
}

/// The migration did not happen: `migrating_to` cleared with the claim, and its event.
async fn clear_intent(
    core: &Core,
    claim: &Claim,
    row: &SandboxRow,
    kind: &str,
    data: serde_json::Value,
) -> Result<(), FabricError> {
    let (id, owner, kind_s, host) = (row.id, claim.owner.clone(), kind.to_owned(), row.host.clone());
    let n = aviary_pg::tx!(&core.pool, |t| -> Result<u64, aviary_pg::Error> {
        let n = t
            .execute(
                "update sandboxes set migrating_to = null, migration_started_at = null,
                   task = null, task_owner = null, task_until = null
                 where id = $1 and task = 'migrate' and task_owner = $2",
                &[&id, &owner],
            )
            .await?;
        if n > 0 {
            t.execute(
                "insert into host_events (kind, host, sandbox_id, data) values ($1, $2, $3, $4)",
                &[&kind_s, &host, &id, &data],
            )
            .await?;
        }
        Ok(n)
    })
    .await?;
    if n == 0 {
        claim.mark_lost();
    }
    Ok(())
}

/// The reason of the migration's `migration-start` event.
async fn start_reason(core: &Core, id: Uuid) -> String {
    let r = async {
        core.pool
            .get()
            .await?
            .query_opt(
                "select data->>'reason' from host_events where sandbox_id = $1 and kind = 'migration-start'
                 order by id desc limit 1",
                &[&id],
            )
            .await
    }
    .await;
    r.ok()
        .flatten()
        .and_then(|r| r.try_get::<_, Option<String>>(0).ok().flatten())
        .unwrap_or_else(|| "unknown".into())
}

/// The event's figures from runsc's restore statistics.
fn stats_data(stats: Option<&serde_json::Value>) -> serde_json::Value {
    let n = |k: &str| stats.and_then(|s| s.get(k)).and_then(serde_json::Value::as_f64);
    let pause = match (n("final_ms"), n("restore_ms")) {
        (None, None) => None,
        (a, b) => Some((a.unwrap_or(0.0) + b.unwrap_or(0.0)).round()),
    };
    serde_json::json!({
        "final_ms": n("final_ms"), "restore_ms": n("restore_ms"), "rounds": n("rounds"), "bytes": n("bytes"),
        "pause_ms": pause,
    })
}

/// Step 7: a release that became pending during the migration runs now. A `host_lost` asked for the source while
/// the sandbox moved off it is dropped when the sandbox now runs on a live host.
async fn after(core: &Arc<Core>, id: Uuid) {
    let Ok(Some(row)) = db::load(&core.pool, id).await else {
        return;
    };
    if !row.is_live() || row.task.is_some() {
        return;
    }
    let Some(p) = row.release_pending.clone() else {
        return;
    };
    if p.reason == ReleaseReason::HostLost
        && let Some(h) = &row.host
        && let Ok(Some(host)) = hosts::get(&core.pool, h).await
        && host.status != HostStatus::Gone
        && host.kind == HostKind::Aws
    {
        let r = async {
            core.pool
                .get()
                .await?
                .execute(
                    "update sandboxes set release_pending = null where id = $1 and release_pending->>'reason' = 'host_lost'",
                    &[&id],
                )
                .await
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(sandbox = %id, error = %e, "stale host loss not cleared");
        }
        return;
    }
    if let Err(e) = sandboxes::request(core, id, p.reason, p.force, p.note.clone()).await {
        tracing::warn!(sandbox = %id, error = %e, "pending release after a migration (the janitor retries it)");
    }
}
