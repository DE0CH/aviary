//! hermit's calls to a sandbox's `:7070` (hermit 3.10) with the control token derived from the row (6.2): health,
//! release and the flush before a workspace clone.

use std::time::Duration;

use hermit_api::sandbox::ReleaseReason;
use supervisor_api::client::ControlClient;
use supervisor_api::control::{Health, Outcome, PrepareRequest, ReleaseRequest};

use crate::Core;
use crate::db::SandboxRow;

/// The client of a row's supervisor; `None` while the row has no address or no loaded key.
#[must_use]
pub fn client(core: &Core, row: &SandboxRow) -> Option<ControlClient> {
    let (host, ip) = (row.host.as_deref()?, row.ip?);
    let kid = u32::try_from(row.token_kid?).ok()?;
    let token = core.keys.control_token(kid, row.id)?;
    let base = core.providers.fabric.control_url(host, ip);
    Some(ControlClient::new(core.http.clone(), base, token).request_id(&uuid::Uuid::now_v7().to_string()))
}

/// What a health check came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Alive {
    /// `ok: true` (ready or not: never unhealthy).
    Answering(Health),
    /// No answer, refused, 401 or `ok: false`.
    Silent,
}

/// `GET /v1/health` (2 s).
pub async fn health(core: &Core, row: &SandboxRow) -> Alive {
    let Some(c) = client(core, row) else {
        return Alive::Silent;
    };
    match c.health().await {
        Ok(h) if h.ok => Alive::Answering(h),
        _ => Alive::Silent,
    }
}

/// Two health checks `gap` apart; `true` when either answered (hermit 12 step 3).
pub async fn alive_twice(core: &Core, row: &SandboxRow, gap: Duration) -> bool {
    if matches!(health(core, row).await, Alive::Answering(_)) {
        return true;
    }
    tokio::time::sleep(gap).await;
    matches!(health(core, row).await, Alive::Answering(_))
}

/// What a release call came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Released {
    Done,
    /// 409 `sandbox_busy` (only for `force: false`): not released.
    Busy(String),
    /// No usable answer.
    Failed(String),
}

/// `POST /v1/lifecycle/release`.
pub async fn release(
    core: &Core,
    row: &SandboxRow,
    reason: ReleaseReason,
    force: bool,
    deadline: Duration,
) -> Released {
    let Some(c) = client(core, row) else {
        return Released::Failed("no address".into());
    };
    let deadline_ms = u32::try_from(deadline.as_millis()).unwrap_or(u32::MAX).max(1000);
    let Some(body) = ReleaseRequest::new(reason, force, deadline_ms) else {
        return Released::Failed(format!("no release call for {reason}"));
    };
    match c.release(&body).await {
        Ok(Outcome::Done) => Released::Done,
        Ok(Outcome::Busy(run)) => Released::Busy(run.state.to_string()),
        Err(e) => Released::Failed(crate::db::message(&e.to_string())),
    }
}

/// `POST /v1/lifecycle/prepare {kind: snapshot}` (a `sync(2)` before a clone, 3.8); `true` when it answered ready.
pub async fn flush(core: &Core, row: &SandboxRow) -> bool {
    let Some(c) = client(core, row) else {
        return false;
    };
    matches!(c.prepare(&PrepareRequest::flush()).await, Ok(Outcome::Done))
}
