//! Freezing (jarvis4 docs/PLAN-fabric.md, Deyao 2026-11-18). The `freeze` task moves a sandbox one step along:
//!
//! - a `ready` sandbox is paused where it runs (`runsc pause`, its memory and processes kept) and the row goes
//!   `frozen` with its host and address kept, so a thaw there is a start whose create step resumes it (`starts.rs`,
//!   [`crate::fabric::Fabric::resume`]);
//! - a `frozen` sandbox still on its host is evicted: recorded into the bucket (`sandboxd freeze`, sealed under a key
//!   of its own) and deleted there; the row stays `frozen` with no host, and its thaw restores it on any host
//!   ([`crate::fabric::Fabric::thaw`]).
//!
//! When to evict is never this task's decision: the repacking decides it ([`crate::fabric::packing`]) and takes the
//! claim. A host that dies takes its paused sandboxes with it (they are lost as any sandbox of it is: a release, the
//! files kept).

use std::sync::Arc;

use hermit_api::sandbox::{ReleaseReason, SandboxState};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::Core;
use crate::db::{self, Frozen, SandboxRow};
use crate::fabric::CreateError;
use crate::fabric::ec2::Ec2;
use crate::fabric::freezekey::FreezeKey;
use crate::fabric::hosts::{self, HostStatus};
use crate::fabric::sandboxd::{self, Clients};
use crate::sandboxes;
use crate::tasks::{self, Claim};

/// What the task works with (the fabric's own parts).
pub struct Parts {
    pub clients: Clients,
    pub ec2: Option<Arc<dyn Ec2>>,
    pub key: Option<FreezeKey>,
}

impl std::fmt::Debug for Parts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parts").finish_non_exhaustive()
    }
}

/// The two objects of a frozen image: its memory, and its scratch file beside it.
#[must_use]
pub fn objects(object: &str) -> Vec<String> {
    vec![object.to_owned(), format!("{object}.scratch")]
}

/// A new eviction's object for sandbox `id`: `frozen/<sandbox>/<freeze id>`.
#[must_use]
pub fn new_object(id: Uuid) -> String {
    format!("frozen/{id}/{}", Uuid::new_v4().simple())
}

/// What became of one step.
enum Done {
    /// The row moved on (or had nothing to do); the claim is cleared with the row's update.
    Moved,
    /// Not moved, the sandbox as it was (still running, or still paused there): the claim is cleared.
    Kept(String),
    /// Lost after it stopped: the sandbox is gone from its host; its files stay. Released forced.
    Lost(String),
}

/// The `freeze` task of a claim this replica holds.
pub async fn freeze(core: &Arc<Core>, claim: &Claim, parts: &Parts) {
    let row = match db::load(&core.pool, claim.id).await {
        Ok(Some(r)) if r.task.as_deref() == Some("freeze") && r.task_owner.as_deref() == Some(claim.owner.as_str()) => {
            r
        }
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(sandbox = %claim.id, error = %e, "freeze: the row was not read (the takeover resumes it)");
            return;
        }
    };
    let done = match (row.status, row.frozen.as_ref().and_then(|f| f.host.clone())) {
        (SandboxState::Ready, _) => pause(core, claim, parts, &row).await,
        (SandboxState::Frozen, Some(host)) => evict(core, claim, parts, &row, &host).await,
        _ => Ok(Done::Kept("nothing to freeze or evict".into())),
    };
    match done {
        Ok(Done::Moved) => {}
        Ok(Done::Kept(why)) => {
            tracing::info!(sandbox = %row.id, why, "freeze step not taken (claim cleared)");
            let _ = tasks::clear(core, claim).await;
        }
        Ok(Done::Lost(why)) => {
            tracing::warn!(sandbox = %row.id, why, "frozen sandbox lost (released, its files kept)");
            let _ = tasks::clear(core, claim).await;
            let _ = sandboxes::request(core, row.id, ReleaseReason::Vanished, true, Some(db::clip(&why, 200))).await;
        }
        Err(e) => {
            tracing::warn!(sandbox = %row.id, error = %e, "freeze step failed (claim cleared)");
            let _ = tasks::clear(core, claim).await;
        }
    }
}

/// sandboxd's 404 for a sandbox it does not run, as opposed to a host on a sandboxd that predates the route (its 404
/// says nothing of the sandbox: the sandbox is kept as it is, never ended for it).
fn not_here(body: &str) -> bool {
    body.contains("is not running here")
}

fn unreachable(e: impl std::fmt::Display) -> CreateError {
    CreateError::Unreachable(e.to_string())
}

async fn client_of(core: &Core, clients: &Clients, host: &str) -> Result<sandboxd_api::Client, CreateError> {
    let h = hosts::get(&core.pool, host)
        .await
        .map_err(unreachable)?
        .filter(|h| h.status != HostStatus::Gone)
        .ok_or_else(|| CreateError::Failed {
            class: "host_gone".into(),
            message: format!("the sandbox's host ({host}) is gone"),
        })?;
    clients.of(&h).map_err(|e| sandboxd::create_error(&e))
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// The row as it is now, to the live view (what magpie's feed and the apps show).
async fn show(core: &Core, id: Uuid) {
    if let Ok(Some(r)) = db::load(&core.pool, id).await {
        core.live.apply(&r);
    }
}

/// Writes the row under the claim (the claim cleared with it); `false` when the claim was not ours any more.
async fn record(core: &Core, claim: &Claim, sql: &str, frozen: &Frozen) -> Result<bool, CreateError> {
    let json = serde_json::to_value(frozen).map_err(unreachable)?;
    let n = core
        .pool
        .get()
        .await
        .map_err(unreachable)?
        .execute(sql, &[&claim.id, &claim.owner, &json])
        .await
        .map_err(unreachable)?;
    Ok(n == 1)
}

async fn pause(core: &Arc<Core>, claim: &Claim, parts: &Parts, row: &SandboxRow) -> Result<Done, CreateError> {
    let Some(host) = row.host.clone() else {
        return Ok(Done::Kept("no host".into()));
    };
    let client = client_of(core, &parts.clients, &host).await?;
    match client.pause(&row.name).await {
        Ok(_) => {}
        Err(sandboxd_api::Error::Status { status: 404, body }) if not_here(&body) => return Ok(Done::Lost(body)),
        Err(e) => return Ok(Done::Kept(e.to_string())),
    }
    let frozen = Frozen {
        host: Some(host.clone()),
        object: String::new(),
        key: String::new(),
        frozen_at: Some(now_rfc3339()),
    };
    let ours = record(
        core,
        claim,
        "update sandboxes set status = 'frozen', phase = null, frozen = $3,
           task = null, task_owner = null, task_until = null
         where id = $1 and task = 'freeze' and task_owner = $2 and status = 'ready'",
        &frozen,
    )
    .await?;
    if !ours {
        // another task took the row meanwhile: what it does next finds the sandbox paused, so resume it
        let _ = client.resume(&row.name).await;
        return Ok(Done::Moved);
    }
    tracing::info!(sandbox = %row.id, host = %host, "sandbox frozen (paused where it runs)");
    show(core, row.id).await;
    Ok(Done::Moved)
}

async fn evict(
    core: &Arc<Core>,
    claim: &Claim,
    parts: &Parts,
    row: &SandboxRow,
    host: &str,
) -> Result<Done, CreateError> {
    let (Some(cfg), Some(key), Some(ec2)) = (&core.cfg.file.fabric.freeze, &parts.key, &parts.ec2) else {
        return Ok(Done::Kept("this fabric does not freeze".into()));
    };
    let client = client_of(core, &parts.clients, host).await?;
    let object = new_object(row.id);
    let creds = ec2
        .object_credentials(&cfg.bucket, &objects(&object), true)
        .await
        .map_err(|e| unreachable(format!("credentials for the frozen image: {e}")))?;
    let (hex, sealed) = key.fresh(row.id).map_err(unreachable)?;
    let req = sandboxd_api::Frozen {
        object: format!("s3://{}/{object}", cfg.bucket),
        region: core.cfg.file.fabric.aws.region.clone(),
        access_key_id: creds.access_key_id,
        secret_access_key: creds.secret_access_key,
        session_token: creds.session_token,
        key: hex,
    };
    match client.freeze(&row.name, &req).await {
        Ok(stats) => {
            tracing::info!(sandbox = %row.id, host, ms = stats.ms, memory_ms = stats.memory_ms, scratch_ms = stats.scratch_ms, "frozen sandbox evicted to the bucket");
        }
        // 409: not recorded, still paused there
        Err(sandboxd_api::Error::Status { status: 409, body }) => return Ok(Done::Kept(body)),
        Err(sandboxd_api::Error::Status { status: 404, body }) if not_here(&body) => return Ok(Done::Lost(body)),
        Err(sandboxd_api::Error::Status { status: 502, body }) => return Ok(Done::Lost(body)),
        // no answer: it may have gone either way; the row stays as it was and the next eviction asks again (a
        // sandbox no longer listed by its host is then found vanished)
        Err(e) => return Ok(Done::Kept(e.to_string())),
    }
    let frozen = Frozen {
        host: None,
        object,
        key: sealed,
        frozen_at: row
            .frozen
            .as_ref()
            .and_then(|f| f.frozen_at.clone())
            .or_else(|| Some(now_rfc3339())),
    };
    record(
        core,
        claim,
        "update sandboxes set host = null, frozen = $3, task = null, task_owner = null, task_until = null
         where id = $1 and task = 'freeze' and task_owner = $2 and status = 'frozen'",
        &frozen,
    )
    .await?;
    show(core, row.id).await;
    if let Err(e) = crate::fabric::gvisor::push_gateway_peers(&parts.clients, core).await {
        tracing::warn!(sandbox = %row.id, error = %e, "gateway peers after an eviction (the next pass pushes them)");
    }
    Ok(Done::Moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sandboxds_own_not_here_is_a_lost_sandbox() {
        assert!(not_here("sbx-1 is not running here"));
        // an older sandboxd without the route answers a bare 404: the sandbox is not ended for that
        assert!(!not_here("not found"));
        assert!(!not_here(""));
    }

    #[test]
    fn a_frozen_image_is_two_objects_under_its_sandbox() {
        let id = Uuid::from_u128(5);
        let o = new_object(id);
        assert!(o.starts_with(&format!("frozen/{id}/")), "{o}");
        assert_ne!(new_object(id), o, "two evictions share an object");
        assert_eq!(objects(&o), vec![o.clone(), format!("{o}.scratch")]);
    }
}
