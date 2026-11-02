//! hermit's `hermit_api::Server`: every contract route of hermit 3 bound to the core. The operator page, the
//! snapshot and the admin routes (3.11-3.12, 16, 18) are WP-20's and answer 501 `not_supported` until then.

// the trait's methods are async by contract; some of hermit's answers come from memory
#![allow(clippy::unused_async_trait_impl)]

use std::sync::Arc;
use std::time::Duration;

use hermit_api::deployment::Deployment;
use hermit_api::error::{self, ApiError, codes};
use hermit_api::people::{People, Person, WarmReply, WarmRequest};
use hermit_api::sandbox::{
    PutSandbox, PutSandboxRequest, ReleaseReason, ReleaseSandboxReply, ReleaseSandboxRequest, Sandbox, SandboxEvents,
    SandboxEventsQuery, SandboxQuery, SandboxState,
};
use hermit_api::sandbox_surface::{ActivityReply, ActivityReport, SecretKeys, SecretValue};
use hermit_api::server::{Ctx, SelfAnswer, Server};
use hermit_api::token::{TokenKeys, TokenLiveness};
use hermit_api::workspaces::{
    ByteRange, CloneRequest, Entries, EntriesQuery, FileContent, FilesQuery, PutWorkspace, Workspace, valid_path,
};
use uuid::Uuid;

use crate::sandboxes::OrUnavailable;
use crate::{Core, db, deployment, feed, people, sandbox_routes, sandboxes, supervisor_client, warm};

/// One replica's contract server.
#[derive(Clone, Debug)]
pub struct Hermit(pub Arc<Core>);

fn bad_path() -> ApiError {
    error::api(400, codes::BAD_PATH, "a relative path without . or .. components")
}

impl Hermit {
    fn core(&self) -> &Arc<Core> {
        &self.0
    }

    /// Releases a conversation's live sandbox (forced, `deleted`) and waits until it is gone (3.8 delete).
    async fn release_and_wait(&self, cid: Uuid) -> Result<bool, ApiError> {
        let core = self.core();
        let Some(row) = db::latest_of(&core.pool, cid)
            .await
            .or_503()?
            .filter(db::SandboxRow::is_live)
        else {
            return Ok(true);
        };
        sandboxes::request(core, row.id, ReleaseReason::Deleted, true, None)
            .await
            .or_503()?;
        let mut w = core.listener.waiter(db::CHANNEL, &cid.to_string());
        let end = tokio::time::Instant::now() + core.timings.delete_wait;
        loop {
            if db::load(&core.pool, row.id)
                .await
                .or_503()?
                .is_none_or(|r| !r.is_live())
            {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= end {
                return Ok(false);
            }
            let _ = tokio::time::timeout(core.timings.recheck, w.changed()).await;
        }
    }
}

impl Server for Hermit {
    async fn deployment(&self, _ctx: Ctx) -> Result<Deployment, ApiError> {
        Ok(deployment::of(&self.core().cfg))
    }

    async fn person(&self, _ctx: Ctx, email: String) -> Result<Person, ApiError> {
        people::person(&self.core().catalogue, &email)
    }

    async fn people(&self, _ctx: Ctx) -> Result<People, ApiError> {
        people::people(&self.core().catalogue)
    }

    async fn warm(&self, _ctx: Ctx, body: WarmRequest) -> Result<WarmReply, ApiError> {
        warm::warm(self.core(), &body).await
    }

    async fn put_sandbox(&self, _ctx: Ctx, cid: Uuid, body: PutSandboxRequest) -> Result<PutSandbox, ApiError> {
        sandboxes::put(self.core(), cid, &body).await
    }

    async fn get_sandbox(&self, _ctx: Ctx, cid: Uuid, q: SandboxQuery) -> Result<Sandbox, ApiError> {
        feed::get_sandbox(self.core(), cid, q).await
    }

    async fn release_sandbox(
        &self,
        _ctx: Ctx,
        cid: Uuid,
        body: ReleaseSandboxRequest,
    ) -> Result<ReleaseSandboxReply, ApiError> {
        sandboxes::release_conversation(self.core(), cid, body).await
    }

    // pause, resume and snapshot: YYC sandboxes are disposable (3.6.4): 501 `not_supported`, the trait's default

    async fn sandbox_events(&self, _ctx: Ctx, q: SandboxEventsQuery) -> Result<SandboxEvents, ApiError> {
        feed::events(self.core(), q).await
    }

    async fn put_workspace(&self, _ctx: Ctx, wid: Uuid) -> Result<PutWorkspace, ApiError> {
        let (w, made) = self.core().providers.workspaces.ensure(wid).await?;
        Ok(if made {
            PutWorkspace::Created(w)
        } else {
            PutWorkspace::Existed(w)
        })
    }

    async fn get_workspace(&self, _ctx: Ctx, wid: Uuid) -> Result<Workspace, ApiError> {
        self.core()
            .providers
            .workspaces
            .get(wid)
            .await?
            .ok_or_else(|| error::not_found(codes::NO_WORKSPACE, "no such workspace"))
    }

    async fn delete_workspace(&self, _ctx: Ctx, wid: Uuid) -> Result<(), ApiError> {
        // the conversation's live sandbox goes first (3.8), at most 60 s
        if !self.release_and_wait(wid).await? {
            tracing::warn!(workspace = %wid, "workspace delete refused: its sandbox is still releasing");
            return Err(error::api(
                503,
                codes::WORKSPACE_UNAVAILABLE,
                "the workspace's sandbox is still releasing",
            )
            .retry_after(5));
        }
        Ok(self.core().providers.workspaces.delete(wid).await?)
    }

    async fn workspace_entries(&self, _ctx: Ctx, wid: Uuid, q: EntriesQuery) -> Result<Entries, ApiError> {
        let path = q.path.unwrap_or_default();
        if !path.is_empty() && !valid_path(&path) {
            return Err(bad_path());
        }
        Ok(self
            .core()
            .providers
            .workspaces
            .entries(wid, &path, q.cursor.as_deref())
            .await?)
    }

    async fn workspace_file(
        &self,
        _ctx: Ctx,
        wid: Uuid,
        q: FilesQuery,
        range: Option<ByteRange>,
    ) -> Result<FileContent, ApiError> {
        if q.path.is_empty() {
            return Err(error::api(409, codes::NOT_A_FILE, "the root is a directory"));
        }
        if !valid_path(&q.path) {
            return Err(bad_path());
        }
        Ok(self.core().providers.workspaces.file(wid, &q.path, range).await?)
    }

    async fn clone_workspace(&self, _ctx: Ctx, wid: Uuid, body: CloneRequest) -> Result<Workspace, ApiError> {
        let core = self.core();
        // a ready sandbox of the source flushes first (3.8, DESIGN 12 D8); a failed or slow flush is logged and the
        // clone goes ahead
        if let Some(row) = db::latest_of(&core.pool, body.from).await.or_503()?
            && row.status == SandboxState::Ready
            && !tokio::time::timeout(Duration::from_secs(15), supervisor_client::flush(core, &row))
                .await
                .unwrap_or(false)
        {
            tracing::warn!(sandbox = %row.id, workspace = %body.from, "flush before clone failed (cloning anyway)");
        }
        Ok(core.providers.workspaces.clone_workspace(wid, body.from).await?)
    }

    async fn token_keys(&self, _ctx: Ctx) -> Result<TokenKeys, ApiError> {
        Ok(sandbox_routes::token_keys(self.core()))
    }

    async fn token_liveness(&self, _ctx: Ctx, sandbox_id: Uuid) -> Result<TokenLiveness, ApiError> {
        let core = self.core();
        let e = core
            .live
            .get(sandbox_id)
            .ok_or_else(|| error::not_found(codes::UNKNOWN_SANDBOX, "no such sandbox"))?;
        let live = e.state != SandboxState::Gone && core.catalogue.allowed(&e.person);
        Ok(TokenLiveness {
            sandbox_id: e.id,
            conversation_id: e.conversation_id,
            person: e.person,
            state: e.state,
            live,
            expires_at: e.hard_expires_at,
        })
    }

    async fn sandbox_self(&self, ctx: Ctx, if_none_match: Option<String>) -> Result<SelfAnswer, ApiError> {
        sandbox_routes::sandbox_self(self.core(), &ctx, if_none_match.as_deref())
    }

    async fn sandbox_secrets(&self, ctx: Ctx) -> Result<SecretKeys, ApiError> {
        sandbox_routes::secrets(self.core(), &ctx)
    }

    async fn sandbox_secret(&self, ctx: Ctx, key: String) -> Result<SecretValue, ApiError> {
        sandbox_routes::secret(self.core(), &ctx, &key)
    }

    async fn sandbox_activity(&self, ctx: Ctx, body: ActivityReport) -> Result<ActivityReply, ApiError> {
        sandbox_routes::activity(self.core(), &ctx, &body).await
    }
}
