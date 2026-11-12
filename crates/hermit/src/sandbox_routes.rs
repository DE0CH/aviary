//! The sandbox's own routes (hermit 3.7), reached by the supervisor through the gateway: the descriptor, secrets by
//! key (YYC grants none, 7) and activity reports. hermit answers only for a live sandbox whose id, conversation and
//! person match the gateway's three headers; the identity check and the descriptor come from the live map, so they
//! need no database connection.

use std::collections::BTreeMap;
use std::sync::Arc;

use hermit_api::error::{self, ApiError, codes};
use hermit_api::sandbox::ReleaseReason;
use hermit_api::sandbox::SandboxState;
use hermit_api::sandbox_surface::{
    ActivityEventKind, ActivityReply, ActivityReplyState, ActivityReport, Claude, Control, Lifecycle, PendingRelease,
    Resources, SandboxSelf, SecretKeys, SecretValue, SelfSandbox, Title, WorkspaceResources, valid_secret_key,
};
use hermit_api::server::{Ctx, SandboxIdentity, SelfAnswer};
use hermit_api::token::TokenKeys;
use time::OffsetDateTime;

use crate::Core;
use crate::db::{self, SandboxRow, bump};
use crate::live::Entry;
use crate::sandboxes::{self, OrUnavailable, ReleaseKind};
use crate::tokens::TokenFacts;

fn no_sandbox() -> ApiError {
    error::not_found(codes::NO_SANDBOX, "no such live sandbox")
}

/// The live entry the gateway's headers name, if they match it.
///
/// # Errors
/// 404 `no_sandbox`.
pub fn identify(core: &Core, ctx: &Ctx) -> Result<(SandboxIdentity, Entry), ApiError> {
    let id = ctx.sandbox().ok_or_else(no_sandbox)?;
    let e = core.live.get(id.id).ok_or_else(no_sandbox)?;
    let person = id.person.trim().to_lowercase();
    let live = matches!(
        e.state,
        SandboxState::Starting | SandboxState::Ready | SandboxState::Paused | SandboxState::Releasing
    );
    if !live || e.conversation_id != id.conversation || e.person != person {
        return Err(no_sandbox());
    }
    Ok((id, e))
}

/// The descriptor of a live entry (3.7.1).
#[must_use]
pub fn descriptor(core: &Core, e: &Entry) -> Option<SandboxSelf> {
    let cfg = &core.cfg;
    let s = cfg.sandbox();
    let kid = u32::try_from(e.token_kid?).ok()?;
    let token = core.keys.sandbox_token(
        kid,
        &TokenFacts {
            sandbox_id: e.id,
            conversation_id: e.conversation_id,
            ip: e.ip,
            expires_at: e.hard_expires_at.unix_seconds(),
            person: &e.person,
        },
    )?;
    let env: BTreeMap<String, String> = s.env.iter().map(|(k, v)| (k.clone(), cfg.fill(v, &token))).collect();
    let d = SandboxSelf {
        sandbox: SelfSandbox {
            id: e.id,
            conversation_id: e.conversation_id,
            person: e.person.clone(),
            created_at: e.created_at,
            expires_at: e.expires_at,
            image: e.image.clone().unwrap_or_else(|| cfg.image_digest.clone()),
        },
        control: Control {
            port: supervisor_api::CONTROL_PORT,
            token_sha256: core.keys.control_hashes(kid, e.id),
        },
        claude: Claude {
            managed_claude_md: cfg.managed_claude_md.clone(),
            managed_skills: cfg.managed_skills.clone(),
            // YYC loads only the managed files (Deyao, 2026-11-11)
            project_settings: false,
            compact_at_images: s.compact_at_images,
        },
        title: Title {
            system: cfg.title_system.clone(),
        },
        env,
        env_private: Vec::new(),
        resources: Resources {
            memory_bytes: cfg.memory_bytes,
            cpus: cfg.cpus(),
            pids: s.pids,
            tmp_bytes: cfg.tmp_bytes,
            open_files: s.open_files,
            workspace: WorkspaceResources {
                path: "/workspace".into(),
                quota_bytes: cfg.workspace_quota_bytes,
                quota_inodes: s.workspace.quota_inodes,
            },
        },
        lifecycle: Lifecycle {
            activity_every_seconds: s.activity_every_seconds,
            idle_minutes: s.idle_minutes,
            max_lifetime_hours: s.max_lifetime_hours,
            lifetime_grace_minutes: s.lifetime_grace_minutes,
        },
        secrets: SecretKeys::default(),
        etag: String::new(),
    };
    Some(d.with_etag())
}

/// `GET /v1/sandbox/self`.
///
/// # Errors
/// 404 `no_sandbox`; 503 when the row's key is not loaded.
pub fn sandbox_self(core: &Core, ctx: &Ctx, if_none_match: Option<&str>) -> Result<SelfAnswer, ApiError> {
    let (_, e) = identify(core, ctx)?;
    let d = descriptor(core, &e).ok_or_else(|| ApiError::unavailable("the sandbox's token key is not loaded", 5))?;
    if if_none_match.is_some_and(|v| v.split(',').any(|t| t.trim() == d.etag || t.trim() == "*")) {
        return Ok(SelfAnswer::NotModified { etag: d.etag });
    }
    Ok(SelfAnswer::Fresh(Box::new(d)))
}

/// `GET /v1/sandbox/secrets`: YYC grants nothing.
///
/// # Errors
/// 404 `no_sandbox`.
pub fn secrets(core: &Core, ctx: &Ctx) -> Result<SecretKeys, ApiError> {
    identify(core, ctx)?;
    Ok(SecretKeys::default())
}

/// `GET /v1/sandbox/secrets/{key}`: every key is 404 `not_granted` (no secret value enters a YYC sandbox).
///
/// # Errors
/// 400 for a malformed key, 404 `no_sandbox` or `not_granted`.
pub fn secret(core: &Core, ctx: &Ctx, key: &str) -> Result<SecretValue, ApiError> {
    identify(core, ctx)?;
    if !valid_secret_key(key) {
        return Err(ApiError::bad_request("a key is [A-Z][A-Z0-9_]{0,63}"));
    }
    Err(error::not_found(
        codes::NOT_GRANTED,
        "this key is not granted to the sandbox",
    ))
}

/// The token keys (3.9), from memory.
#[must_use]
pub fn token_keys(core: &Core) -> TokenKeys {
    core.keys.published().clone()
}

/// `POST /v1/sandbox/activity` (3.7.3): one row update; a `busy` flip also writes an event; `memory_kill` events
/// become `host_events`. The reply repeats a pending release.
///
/// # Errors
/// 404 `no_sandbox`, 503.
#[allow(clippy::too_many_lines)] // one report: the row update, its events, the pending release
pub async fn activity(core: &Arc<Core>, ctx: &Ctx, body: &ActivityReport) -> Result<ActivityReply, ApiError> {
    let (ident, _) = identify(core, ctx)?;
    let id = ident.id;
    let busy = body.busy();
    let turn = body.turn_id.map(|t| t.to_string());
    let tasks = i32::try_from(body.tasks).unwrap_or(i32::MAX);
    let boot = db::clip(&body.boot_id, 100);
    let version = db::clip(&body.version, 100);
    let protocol = i32::try_from(body.protocol).unwrap_or(0);
    let idle_since: Option<OffsetDateTime> = body.idle_since.map(hermit_api::Timestamp::datetime);
    let kills: Vec<serde_json::Value> = body
        .events
        .iter()
        .take(hermit_api::sandbox_surface::ACTIVITY_MAX_EVENTS)
        .filter(|e| e.kind == ActivityEventKind::MemoryKill)
        .map(|e| serde_json::json!({"pid": e.pid, "comm": e.comm, "rss": e.rss, "at": e.at}))
        .collect();
    // borrowed: the transaction may run more than once (aviary_pg::tx!)
    let (turn, boot, version, kills) = (&turn, &boot, &version, &kills);
    let row = aviary_pg::tx!(
        &core.pool,
        |t| -> Result<Option<(SandboxRow, bool)>, aviary_pg::Error> {
            let Some(r) = t
                .query_opt(
                    "select * from sandboxes where id = $1 and status <> 'gone' for update",
                    &[&id],
                )
                .await?
            else {
                return Ok(None);
            };
            let before = SandboxRow::from_row(&r)?;
            let now = OffsetDateTime::now_utc();
            let last_active = if busy {
                before.last_active_at.max(now)
            } else if before.busy {
                sandboxes::idle_from(before.last_active_at, idle_since.map(Into::into), now)
            } else {
                before.last_active_at
            };
            let r = t
                .query_one(
                    "update sandboxes set last_heartbeat_at = now(), busy = $2, busy_turn = $3, background_tasks = $4,
                   boot_id = $5, supervisor_version = $6, supervisor_protocol = $7,
                   last_busy_at = case when $2 then now() else last_busy_at end, last_active_at = $8
                 where id = $1 returning *",
                    &[&id, &busy, &turn, &tasks, &boot, &version, &protocol, &last_active],
                )
                .await?;
            let mut row = SandboxRow::from_row(&r)?;
            for k in kills {
                t.execute(
                    "insert into host_events (kind, host, sandbox_id, data) values ('memory-kill', $1, $2, $3)",
                    &[&row.host, &id, k],
                )
                .await?;
            }
            let flipped = before.busy != busy;
            if flipped {
                row = bump(t, id).await?;
            }
            Ok(Some((row, flipped)))
        }
    )
    .await
    .or_503()?;
    let Some((row, flipped)) = row else {
        return Err(no_sandbox());
    };
    if flipped {
        core.live.apply(&row);
    }
    let pending = row.release_pending.clone();
    // the supervisor finished a release it took from a reply, or a requested release meets an idle point: run it
    let released = body.state == hermit_api::sandbox_surface::ActivityState::Released;
    let due = pending
        .as_ref()
        .is_some_and(|p| !busy && ReleaseKind::of(p) == ReleaseKind::Soft)
        || pending.is_some() && released;
    if due && !row.claimed(OffsetDateTime::now_utc()) {
        if let Some(p) = &pending
            && let Err(e) = sandboxes::request(core, id, p.reason, p.force, p.note.clone()).await
        {
            tracing::warn!(sandbox = %id, error = %e, "pending release after a report");
        }
    } else if released && pending.is_none() && !row.claimed(OffsetDateTime::now_utc()) {
        // a release the supervisor ended on its own (a pending release this replica no longer has)
        let _ = sandboxes::request(core, id, ReleaseReason::Requested, false, None).await;
    }
    Ok(ActivityReply {
        state: if row.status == SandboxState::Releasing {
            ActivityReplyState::Releasing
        } else {
            ActivityReplyState::Ready
        },
        // a release a PUT cancelled is not offered to the supervisor
        release: pending.filter(|p| !p.cancelled).and_then(|p| {
            Some(PendingRelease {
                reason: p.reason,
                code: p.reason.call_code()?,
                force: p.force,
                deadline_at: p.deadline_at,
            })
        }),
    })
}
