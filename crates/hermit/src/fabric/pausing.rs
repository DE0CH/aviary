//! Freezing in place (jarvis4 docs/PLAN-fabric.md, Deyao 2026-11-18): the `freeze` task pauses a ready sandbox on the
//! host it runs on (`runsc pause`, its memory and processes kept) and leaves the row `frozen` with its host kept, so a
//! thaw there is a start whose create step resumes it (`starts.rs`, [`crate::fabric::Fabric::resume`]). Pushing it to
//! the bucket (`sandboxd freeze`) is eviction, the repacking's decision (`fabric::packing`), never this task's.
//!
//! A host that dies takes its paused sandboxes with it: they come back only through a release (their files, a new
//! sandbox, the transcript), which is what the deployment accepts for a freeze.

use std::sync::Arc;

use hermit_api::sandbox::SandboxState;
use time::OffsetDateTime;

use crate::Core;
use crate::db::{self, Frozen};
use crate::fabric::CreateError;
use crate::fabric::hosts::{self, HostStatus};
use crate::fabric::sandboxd::{self, Clients};
use crate::tasks::{self, Claim};

/// The `freeze` task of a claim this replica holds: pause it where it runs, then record the row frozen.
pub async fn freeze(core: &Arc<Core>, claim: &Claim, clients: &Clients) {
    match steps(core, claim, clients).await {
        Ok(()) => {}
        Err(e) => {
            // the row judges itself again (the claim is cleared: the sandbox stays ready, or the taker tries again)
            tracing::warn!(sandbox = %claim.id, error = %e, "freeze failed (claim cleared)");
            let _ = tasks::clear(core, claim).await;
        }
    }
}

async fn steps(core: &Arc<Core>, claim: &Claim, clients: &Clients) -> Result<(), CreateError> {
    let Some(row) = db::load(&core.pool, claim.id)
        .await
        .map_err(|e| CreateError::Unreachable(e.to_string()))?
    else {
        return Ok(());
    };
    if row.status != SandboxState::Ready || row.task.as_deref() != Some("freeze") {
        // nothing to pause any more (released, or another task took it)
        return Ok(());
    }
    let Some(host) = row.host.clone() else {
        return Ok(());
    };
    let h = hosts::get(&core.pool, &host)
        .await
        .map_err(|e| CreateError::Unreachable(e.to_string()))?
        .filter(|h| h.status != HostStatus::Gone)
        .ok_or_else(|| CreateError::Failed {
            class: "host_gone".into(),
            message: format!("the sandbox's host ({host}) is gone"),
        })?;
    let client = clients.of(&h).map_err(|e| sandboxd::create_error(&e))?;
    client.pause(&row.name).await.map_err(|e| sandboxd::create_error(&e))?;
    // under the claim: frozen where it is, its address and host kept so a thaw resumes it here
    let frozen = Frozen {
        host: Some(host.clone()),
        object: String::new(),
        key: String::new(),
        frozen_at: Some(
            OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        ),
    };
    let json = serde_json::to_value(&frozen).map_err(|e| CreateError::Unreachable(e.to_string()))?;
    core.pool
        .get()
        .await
        .map_err(|e| CreateError::Unreachable(e.to_string()))?
        .execute(
            "update sandboxes set status = 'frozen', phase = null, frozen = $4,
               task = null, task_owner = null, task_until = null
             where id = $1 and task = 'freeze' and task_owner = $2 and status = 'ready'",
            &[&row.id, &claim.owner, &0i64, &json],
        )
        .await
        .map_err(|e| CreateError::Unreachable(e.to_string()))?;
    tracing::info!(sandbox = %row.id, host = %host, "sandbox frozen (paused where it runs)");
    Ok(())
}
