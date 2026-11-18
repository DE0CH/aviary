//! Freezing (jarvis4 docs/PLAN-fabric.md, "Next: freeze and the time machine"): `POST …/sandbox/freeze {until?}` and
//! `…/thaw`. A freeze keeps the sandbox, RAM and processes included, off any host (`frozen`: no host, no slot, its
//! address kept for its restored network); a thaw, a get-or-start, or `until` passing brings the same sandbox back.
//! A freeze is the fabric's resumable row task ([`crate::fabric::Fabric::freeze`]); a thaw is a start whose create
//! step thaws (`starts.rs`: a slot, a host, the same address, then [`crate::fabric::Fabric::thaw`]), so it waits for
//! capacity and a host as any start does. A deployment whose fabric does not freeze answers 501 `not_supported`, as before.

use std::sync::Arc;

use aviary_pg::Tx;
use hermit_api::error::{self, ApiError, codes};
use hermit_api::sandbox::{FreezeSandboxRequest, Sandbox, SandboxState};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::Core;
use crate::db::{SandboxRow, bump};
use crate::sandboxes::OrUnavailable;
use crate::tasks::{self, Claim, Kind};

fn not_supported() -> ApiError {
    error::not_supported()
}

fn no_sandbox() -> ApiError {
    error::not_found(codes::NO_SANDBOX, "no live sandbox for this conversation")
}

fn busy(what: &str) -> ApiError {
    error::api(409, codes::SANDBOX_BUSY, what)
}

/// Inside a transaction holding the row: a frozen row with no task becomes `starting` (phase `capacity`) under this
/// replica's start claim (spawn the start).
///
/// # Errors
/// A database error.
pub async fn begin_thaw(t: &Tx<'_>, core: &Core, row: &SandboxRow) -> Result<Option<SandboxRow>, aviary_pg::Error> {
    if row.status != SandboxState::Frozen || row.task.is_some() {
        return Ok(None);
    }
    if !tasks::take_in(t, core, row.id, Kind::Start).await? {
        return Ok(None);
    }
    t.execute(
        "update sandboxes set status = 'starting', phase = 'capacity', frozen_until = null,
           start_deadline = now() + $2::bigint * interval '1 millisecond'
         where id = $1",
        &[
            &row.id,
            &crate::ms(crate::config::Config::secs(core.cfg.sandbox().start_deadline_seconds)),
        ],
    )
    .await?;
    Ok(Some(bump(t, row.id).await?))
}

/// Frozen sandboxes whose `until` has passed come back (a start each; the janitor calls this every pass). The
/// thaw's own rules apply (a claim, a placement): one already starting is left alone.
///
/// # Errors
/// A database error.
pub async fn wake_due(core: &Arc<Core>) -> Result<Vec<Uuid>, aviary_pg::Error> {
    let c = core.clone();
    let c = &c;
    let rows: Vec<Uuid> = core
        .pool
        .get()
        .await?
        .query(
            "select id from sandboxes where status = 'frozen' and frozen_until < now() and task is null",
            &[],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    let mut woken = Vec::new();
    for id in rows {
        let started = aviary_pg::tx!(&core.pool, |t| -> Result<bool, aviary_pg::Error> {
            let Some(r) = t
                .query_opt(
                    "select * from sandboxes where id = $1 and status = 'frozen' for update",
                    &[&id],
                )
                .await?
            else {
                return Ok(false);
            };
            let row = SandboxRow::from_row(&r)?;
            Ok(begin_thaw(t, c, &row).await?.is_some())
        })
        .await?;
        if started {
            tracing::info!(sandbox = %id, "frozen until passed: thawing");
            tasks::run(core, Claim::adopt(core, id, Kind::Start));
            woken.push(id);
        }
    }
    Ok(woken)
}

/// `POST …/sandbox/freeze`: a ready sandbox gets this replica's freeze claim and is answered as it is (202; it shows
/// `frozen` once the fabric has it). A frozen one only takes the new `until`.
///
/// # Errors
/// 501 `not_supported`, 404 `no_sandbox`, 409 `sandbox_busy` (starting, releasing, or a task under way).
pub async fn freeze(core: &Arc<Core>, cid: Uuid, req: &FreezeSandboxRequest) -> Result<Sandbox, ApiError> {
    if !core.providers.fabric.freezes() {
        return Err(not_supported());
    }
    let until: Option<OffsetDateTime> = req
        .until
        .and_then(|u| OffsetDateTime::from_unix_timestamp_nanos(i128::from(u.unix_millis()) * 1_000_000).ok());
    let c = core.clone();
    let c = &c;
    let r = aviary_pg::tx!(&core.pool, |t| -> Result<
        Result<(SandboxRow, bool), ApiError>,
        aviary_pg::Error,
    > {
        let Some(r) = t
            .query_opt(
                "select * from sandboxes where conversation_id = $1 and status <> 'gone' for update",
                &[&cid],
            )
            .await?
        else {
            return Ok(Err(no_sandbox()));
        };
        let row = SandboxRow::from_row(&r)?;
        match row.status {
            SandboxState::Frozen => {
                t.execute(
                    "update sandboxes set frozen_until = $2 where id = $1",
                    &[&row.id, &until],
                )
                .await?;
                Ok(Ok((bump(t, row.id).await?, false)))
            }
            SandboxState::Ready if row.task.is_none() && row.release_pending.is_none() => {
                if !tasks::take_in(t, c, row.id, Kind::Freeze).await? {
                    return Ok(Err(busy("the sandbox has a task under way")));
                }
                t.execute(
                    "update sandboxes set frozen_until = $2 where id = $1",
                    &[&row.id, &until],
                )
                .await?;
                Ok(Ok((bump(t, row.id).await?, true)))
            }
            _ => Ok(Err(busy("only a ready sandbox with nothing under way freezes"))),
        }
    })
    .await
    .or_503()?;
    let (row, owned) = r?;
    core.live.apply(&row);
    if owned {
        tracing::info!(sandbox = %row.id, conversation = %cid, "sandbox freeze requested");
        tasks::run(core, Claim::adopt(core, row.id, Kind::Freeze));
    }
    Ok(row.to_sandbox())
}

/// `POST …/sandbox/thaw`: a frozen sandbox comes back (202, `starting` until the fabric has it running). One that is
/// not frozen is answered as it is.
///
/// # Errors
/// 501 `not_supported`, 404 `no_sandbox`.
pub async fn thaw(core: &Arc<Core>, cid: Uuid) -> Result<Sandbox, ApiError> {
    if !core.providers.fabric.freezes() {
        return Err(not_supported());
    }
    let c = core.clone();
    let c = &c;
    let r = aviary_pg::tx!(
        &core.pool,
        |t| -> Result<Option<(SandboxRow, bool)>, aviary_pg::Error> {
            let Some(r) = t
                .query_opt(
                    "select * from sandboxes where conversation_id = $1 and status <> 'gone' for update",
                    &[&cid],
                )
                .await?
            else {
                return Ok(None);
            };
            let row = SandboxRow::from_row(&r)?;
            Ok(Some(match begin_thaw(t, c, &row).await? {
                Some(thawing) => (thawing, true),
                None => (row, false),
            }))
        }
    )
    .await
    .or_503()?;
    let (row, owned) = r.ok_or_else(no_sandbox)?;
    core.live.apply(&row);
    if owned {
        tracing::info!(sandbox = %row.id, conversation = %cid, "sandbox thaw requested");
        tasks::run(core, Claim::adopt(core, row.id, Kind::Start));
    }
    Ok(row.to_sandbox())
}
