//! The `:7070` API served to hermit (supervisor 15.4, surface `supervisor-control`). Every route needs
//! `Authorization: Bearer <control token>` whose SHA-256 is in the descriptor's `control.tokenSha256` (compared in
//! constant time); otherwise 401 `unauthenticated`. Until the descriptor has been read every call is 503
//! `unavailable` with `retry-after: 1`.

use std::sync::Arc;

use hermit_api::server::{Answer, Ctx};
use supervisor_api::control::{
    self as c, Fuse, Health, Outcome, PrepareAnswer, PrepareRequest, ReleaseAnswer, ReleaseRequest, Resumed, Run,
    StateReport,
};
use supervisor_api::server::ControlServer;
use supervisor_api::{CONTROL, ExecRequest, ExecResult, control_token_matches};

use super::Hermit;
use super::activity::Facts;
use super::exec::Exec;
use crate::lifecycle::Lifecycle;

/// The `:7070` server.
#[derive(Debug, Clone)]
pub struct Control {
    pub hermit: Hermit,
    pub lifecycle: Arc<Lifecycle>,
    pub facts: Facts,
    pub exec: Arc<Exec>,
}

impl Control {
    fn check(&self, ctx: &Ctx) -> Answer<()> {
        let Some(d) = self.hermit.descriptor() else {
            return Err(c::not_ready());
        };
        match ctx.bearer() {
            Some(t) if control_token_matches(t, &d.control.token_sha256) => Ok(()),
            _ => Err(aviary_core::http::ApiError::unauthenticated()),
        }
    }

    /// `GET /v1/health`'s body.
    #[must_use]
    pub fn health_now(&self) -> Health {
        let s = self.lifecycle.reported();
        Health {
            ok: true,
            ready: self.lifecycle.is_ready(),
            protocol: CONTROL.1,
            version: super::version(),
            boot_id: self.facts.boot_id.clone(),
            busy: s.state.is_busy(),
            state: s.state,
        }
    }

    /// `GET /v1/state`'s body.
    #[must_use]
    pub fn state_now(&self) -> StateReport {
        let s = self.lifecycle.reported();
        let fs = self.lifecycle.fs().stats();
        StateReport {
            run: Run {
                state: s.state,
                turn_id: s.turn_id,
                origin: s.origin,
                tasks: self.lifecycle.agent().tasks(),
                asks: self.lifecycle.agent().asks(),
            },
            generation: self.lifecycle.agent().generation(),
            // the filesystem's (it re-takes a lost lease), else the one the start sequence took
            lease_epoch: fs.lease_epoch.or_else(|| self.lifecycle.lease_epoch()),
            fuse: Fuse {
                inflight: fs.inflight,
                blocked: fs.blocked,
                oldest_blocked_ms: fs.oldest_blocked_ms,
            },
            memory: self.facts.memory(&self.hermit),
            supervisor: self.facts.supervisor(),
            magpie_reachable: fs.magpie_reachable,
        }
    }
}

#[allow(clippy::unused_async_trait_impl)] // the trait's methods are async; health and state answer at once
impl ControlServer for Control {
    async fn health(&self, ctx: Ctx) -> Answer<Health> {
        self.check(&ctx)?;
        Ok(self.health_now())
    }

    async fn state(&self, ctx: Ctx) -> Answer<StateReport> {
        self.check(&ctx)?;
        Ok(self.state_now())
    }

    async fn prepare(&self, ctx: Ctx, body: PrepareRequest) -> Answer<PrepareAnswer> {
        self.check(&ctx)?;
        let out = self.lifecycle.prepare(body).await;
        tracing::info!(kind = ?body.kind, answer = ?out, "lifecycle");
        match out {
            Outcome::Done => Ok(PrepareAnswer { ready: true }),
            Outcome::Busy(run) => Err(c::busy(run)),
        }
    }

    async fn resumed(&self, ctx: Ctx, body: Resumed) -> Answer<()> {
        self.check(&ctx)?;
        self.lifecycle.resumed(body.kind).await;
        Ok(())
    }

    async fn release(&self, ctx: Ctx, body: ReleaseRequest) -> Answer<ReleaseAnswer> {
        self.check(&ctx)?;
        match self.lifecycle.release(body).await {
            Outcome::Done => Ok(ReleaseAnswer { released: true }),
            Outcome::Busy(run) => Err(c::busy(run)),
        }
    }

    async fn exec(&self, ctx: Ctx, body: ExecRequest) -> Answer<ExecResult> {
        self.check(&ctx)?;
        let env = self.hermit.descriptor().map(|d| d.env.clone()).unwrap_or_default();
        self.exec.run(body, &env).await
    }
}
