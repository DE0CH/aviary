//! The hermit contract on Jarvis 4 sessions (DE0CH/aviary docs/CONTRACTS.md "The hermit contract").
//!
//! A conversation is one Jarvis 4 session (harness `aviary`). One made in the Jarvis 4 app gets its conversation when
//! its machine first boots (`central::boot`); a conversation begun in the chat gets its session at its first
//! get-or-start, which proposes one through the router (Deyao, 2026-11-18: the chat's New chat; it waits in Deyao's
//! Inbox, `reconcile`). A sandbox is one boot of that machine. Get-or-start answers the live boot, or resumes the paused
//! session through the router and waits for its machine to boot; a release rides on the supervisor's next activity
//! reply (hermit never calls into a machine); the router's session list ends a sandbox whose machine is gone
//! (`reconcile`). People: Deyao alone, an admin. Workspaces live on the session machine (its `~/workspace`), so the
//! workspace routes answer 501, as the contract allows.

use std::time::Duration;

use aviary_core::http::ApiError;
use hermit_api::deployment::Deployment;
use hermit_api::error;
use hermit_api::people::{ExplicitPerson, People, Person, Role, WarmReply, WarmRequest};
use hermit_api::sandbox::{
    Cursor, MAX_WAIT_MS, PutSandbox, PutSandboxRequest, ReleaseCallCode, ReleaseReason,
    ReleaseSandboxReply, ReleaseSandboxRequest, Sandbox, SandboxEvents, SandboxEventsQuery,
    SandboxPhase, SandboxQuery, SandboxState,
};
use hermit_api::sandbox_surface::{
    ActivityReply, ActivityReplyState, ActivityReport, Claude, Control, Lifecycle, PendingRelease,
    Resources, SandboxSelf, SecretKeys, SecretValue, SelfSandbox, Title, WorkspaceResources,
};
use hermit_api::server::{Answer, Ctx, SelfAnswer, Server};
use hermit_api::time::Timestamp;
use hermit_api::token::{TokenKeys, TokenLiveness};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::App;
use crate::db::{self, SandboxRow, SessionRow};

/// What every descriptor says to Claude Code itself: the session's own CLAUDE.md, skills and settings come from its
/// workspace (`claude.projectSettings`), so this is only the frame.
pub const MANAGED_CLAUDE_MD: &str = "This is a Jarvis 4 session of Deyao's, on its own machine. Its instructions are \
the workspace's CLAUDE.md, skills and settings (/workspace), loaded as the project's.\n";

/// The instructions for magpie's title command.
pub const TITLE_SYSTEM: &str = "Write a short title (at most 6 words) for the conversation that begins with the \
user's message below: what the task is, in the user's language, with no quotes and no full stop at the end.";

/// How often a long-poll reads the row again.
const POLL: Duration = Duration::from_millis(250);

fn internal(e: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %e, "hermit-jarvis internal error");
    error::api(
        503,
        "unavailable",
        "hermit-jarvis is unavailable for a moment",
    )
}

/// The deployment hermit-jarvis answers (`/v1/deployment`). Built as JSON, so this crate builds against an aviary
/// whose contract has no `features` yet (it drops the member) and one that has (it takes it): the pin moves to a new
/// aviary only after its replica passed (aviary-pin.yml), and this crate must build on both sides of that.
fn deployment_of(
    settings: &crate::settings::Settings,
    access_team: &str,
    access_aud: &str,
) -> Result<Deployment, serde_json::Error> {
    serde_json::from_value(serde_json::json!({
        "product": {"name": settings.product.name, "locale": settings.product.locale},
        "signIn": {"access": [{"teamDomain": access_team, "audience": access_aud}],
                   "appScheme": settings.sign_in.app_scheme},
        // a new conversation in the chat proposes its own session (Deyao, 2026-11-18: start one and type at once; its
        // first message waits for his approval in the Inbox); in a conversation, the composer's menu picks its model
        // and thinking level from then on (Deyao, 2026-11-12: as on YYC AI); a session ends in the Jarvis 4 app, and
        // its conversation with it (reconcile.rs)
        "conversations": {"peopleCreate": true, "peoplePickModel": true, "peopleDelete": false},
        // the app features the settings file turns on (Deyao, 2026-11-19: the question cards, one deployment first)
        "features": settings.features,
    }))
}

fn ts(t: OffsetDateTime) -> Timestamp {
    Timestamp::from_datetime(t)
}

impl Server for App {
    async fn deployment(&self, _ctx: Ctx) -> Answer<Deployment> {
        deployment_of(&self.settings, &self.access_team, &self.access_aud).map_err(internal)
    }

    async fn person(&self, _ctx: Ctx, email: String) -> Answer<Person> {
        let email = hermit_api::people::normalise_email(&email)
            .ok_or_else(|| error::api(400, "bad_email", "not an email address"))?;
        let role = (email == self.owner).then_some(Role::Admin);
        Ok(Person {
            email,
            allowed: role.is_some(),
            role,
        })
    }

    async fn people(&self, _ctx: Ctx) -> Answer<People> {
        Ok(People {
            generation: hermit_api::token::sha256_hex(&self.owner),
            loaded_at: Timestamp::now(),
            explicit: vec![ExplicitPerson {
                email: self.owner.clone(),
                role: Role::Admin,
            }],
            domains: vec![],
        })
    }

    async fn warm(&self, _ctx: Ctx, _body: WarmRequest) -> Answer<WarmReply> {
        Ok(WarmReply { hinted: false })
    }

    async fn put_sandbox(
        &self,
        _ctx: Ctx,
        cid: Uuid,
        body: PutSandboxRequest,
    ) -> Answer<PutSandbox> {
        let Some(session) = db::session_of_conversation(&self.pool, cid)
            .await
            .map_err(internal)?
        else {
            return self.propose(cid, &body.person).await;
        };
        if body.person.trim().to_lowercase() != session.person {
            return Err(error::api(
                409,
                "person_mismatch",
                "the conversation is another person's",
            ));
        }
        if let Some(row) = db::newest(&self.pool, cid).await.map_err(internal)?
            && row.state != SandboxState::Gone
        {
            // a use of a session waiting in the time machine (a message, an answer) thaws it: the router's resume
            self.thaw_if_frozen(&session.session_id);
            return Ok(answer(row.sandbox()));
        }
        // the session's machine is not up: resume it, and wait for its boot (central::boot adopts this row)
        let row = db::start(
            &self.pool,
            &session,
            None,
            SandboxPhase::Host,
            crate::app::LIFETIME,
            ReleaseReason::Vanished,
        )
        .await
        .map_err(internal)?;
        let app = self.clone();
        let sid = session.session_id.clone();
        let id = row.id;
        tokio::spawn(async move {
            if let Err(e) = app.jarvis.resume(&sid).await {
                tracing::warn!(session = %sid, error = %e, "resume refused");
                let msg = format!("the session could not be resumed: {e}");
                let _ = db::update(&app.pool, id, |r| {
                    if r.state != SandboxState::Starting {
                        return false;
                    }
                    db::end(r, ReleaseReason::StartFailed, &msg);
                    true
                })
                .await;
            }
        });
        Ok(PutSandbox::Starting(row.sandbox()))
    }

    async fn get_sandbox(&self, _ctx: Ctx, cid: Uuid, q: SandboxQuery) -> Answer<Sandbox> {
        let wait = Duration::from_millis(u64::from(q.wait_ms.unwrap_or(0).min(MAX_WAIT_MS)));
        let until = tokio::time::Instant::now() + wait;
        loop {
            let row = db::newest(&self.pool, cid).await.map_err(internal)?;
            let Some(row) = row else {
                return Err(error::not_found(
                    "no_sandbox",
                    "no sandbox for this conversation",
                ));
            };
            let recent = row.state != SandboxState::Gone
                || row
                    .released_at
                    .is_some_and(|t| OffsetDateTime::now_utc() - t < time::Duration::minutes(10));
            if !recent {
                return Err(error::not_found(
                    "no_sandbox",
                    "no sandbox for this conversation",
                ));
            }
            // hermit 3.6.3: hold until the version passes afterVersion or the state leaves `starting`
            let done = row.state != SandboxState::Starting
                || q.after_version.is_some_and(|v| row.version > v);
            if done || tokio::time::Instant::now() >= until {
                return Ok(row.sandbox());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn release_sandbox(
        &self,
        _ctx: Ctx,
        cid: Uuid,
        body: ReleaseSandboxRequest,
    ) -> Answer<ReleaseSandboxReply> {
        let Some(row) = db::newest(&self.pool, cid).await.map_err(internal)? else {
            return Ok(ReleaseSandboxReply {
                released: 0,
                sandbox: None,
            });
        };
        if row.state == SandboxState::Gone {
            return Ok(ReleaseSandboxReply {
                released: 0,
                sandbox: Some(row.sandbox()),
            });
        }
        let reason = ReleaseReason::from(body.reason);
        let force = reason != ReleaseReason::Requested;
        let row = db::update(&self.pool, row.id, |r| {
            if r.state == SandboxState::Gone {
                return false;
            }
            if r.machine.is_none() {
                // a resume nobody booted yet: nothing to stop
                db::end(r, reason, "released before the machine booted");
                return true;
            }
            r.state = SandboxState::Releasing;
            r.pending = Some(PendingRelease {
                reason,
                code: ReleaseCallCode::SandboxReleased,
                force,
                deadline_at: ts(OffsetDateTime::now_utc() + time::Duration::seconds(30)),
            });
            true
        })
        .await
        .map_err(internal)?;
        Ok(ReleaseSandboxReply {
            released: 1,
            sandbox: row.map(|r| r.sandbox()),
        })
    }

    async fn sandbox_events(&self, _ctx: Ctx, q: SandboxEventsQuery) -> Answer<SandboxEvents> {
        let limit = q.limit.unwrap_or(100).clamp(1, 500);
        let wait = Duration::from_millis(u64::from(q.wait_ms.unwrap_or(0).min(MAX_WAIT_MS)));
        let until = tokio::time::Instant::now() + wait;
        let after = match q.after {
            Cursor::Latest => None,
            Cursor::Seq(n) => Some(n),
        };
        loop {
            let (head, events) = db::events(&self.pool, after, limit)
                .await
                .map_err(internal)?;
            if after.is_none() || !events.is_empty() || tokio::time::Instant::now() >= until {
                return Ok(SandboxEvents {
                    head_seq: head,
                    events,
                });
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn delete_workspace(&self, _ctx: Ctx, _wid: Uuid) -> Answer<()> {
        // the workspace is the session machine's own ~/workspace, kept by Jarvis 4 with the session
        Ok(())
    }

    async fn token_keys(&self, _ctx: Ctx) -> Answer<TokenKeys> {
        Ok(self.keys.published.clone())
    }

    async fn token_liveness(&self, _ctx: Ctx, sandbox_id: Uuid) -> Answer<TokenLiveness> {
        let row = db::sandbox(&self.pool, sandbox_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| error::not_found("unknown_sandbox", "no such sandbox"))?;
        Ok(liveness(&row, &self.owner))
    }

    async fn sandbox_self(&self, ctx: Ctx, if_none_match: Option<String>) -> Answer<SelfAnswer> {
        let row = self.own(&ctx).await?;
        let d = descriptor(self, &row);
        if if_none_match.as_deref() == Some(d.etag.as_str()) {
            return Ok(SelfAnswer::NotModified { etag: d.etag });
        }
        Ok(SelfAnswer::Fresh(Box::new(d)))
    }

    async fn sandbox_secrets(&self, ctx: Ctx) -> Answer<SecretKeys> {
        self.own(&ctx).await?;
        // a session's secrets stay on its machine (session-secret); none travel through hermit
        Ok(SecretKeys::default())
    }

    async fn sandbox_secret(&self, ctx: Ctx, _key: String) -> Answer<SecretValue> {
        self.own(&ctx).await?;
        Err(error::not_found(
            "not_granted",
            "no secret is granted through hermit",
        ))
    }

    async fn sandbox_activity(&self, ctx: Ctx, body: ActivityReport) -> Answer<ActivityReply> {
        let row = self.own(&ctx).await?;
        let busy = body.busy();
        let released = body.state == hermit_api::sandbox_surface::ActivityState::Released;
        let info = hermit_api::sandbox::SupervisorInfo {
            protocol: body.protocol,
            version: body.version.clone(),
            boot_id: body.boot_id.clone(),
        };
        let row = db::update(&self.pool, row.id, |r| {
            if r.state == SandboxState::Gone {
                return false;
            }
            let now = OffsetDateTime::now_utc();
            let mut changed = false;
            if released {
                let reason = r
                    .pending
                    .as_ref()
                    .map_or(ReleaseReason::Requested, |p| p.reason);
                db::end(r, reason, "the supervisor finished its release");
                return true;
            }
            if busy || r.busy != busy {
                r.last_active_at = now;
            }
            if r.busy != busy {
                r.busy = busy;
                changed = true;
            }
            if r.supervisor.as_ref() != Some(&info) {
                r.supervisor = Some(info.clone());
                changed = true;
            }
            changed
        })
        .await
        .map_err(internal)?
        .ok_or_else(|| error::not_found("no_sandbox", "no such sandbox"))?;
        Ok(ActivityReply {
            state: if row.state == SandboxState::Releasing {
                ActivityReplyState::Releasing
            } else {
                ActivityReplyState::Ready
            },
            release: row.pending.clone(),
        })
    }

    async fn ops_page(&self, _ctx: Ctx) -> Answer<String> {
        Ok("<!doctype html><meta charset=utf-8><title>hermit-jarvis</title><p>hermit-jarvis: Jarvis 4's sessions as \
            aviary sandboxes. <a href=snapshot>snapshot</a>"
            .to_owned())
    }

    async fn ops_snapshot(&self, _ctx: Ctx, _q: hermit_api::ops::SnapshotQuery) -> Answer<Value> {
        let rows = db::live(&self.pool).await.map_err(internal)?;
        Ok(serde_json::json!({
            "health": {"state": "ok"},
            "sandboxes": rows.iter().map(|r| serde_json::json!({
                "id": r.id, "session": r.session_id, "machine": r.machine, "state": r.state.as_str(),
                "busy": r.busy, "lastActiveAt": ts(r.last_active_at),
            })).collect::<Vec<_>>(),
        }))
    }
}

fn answer(s: Sandbox) -> PutSandbox {
    if s.state == SandboxState::Ready {
        PutSandbox::Ready(s)
    } else {
        PutSandbox::Starting(s)
    }
}

/// Liveness for the gateway (hermit 3.9): live while not gone and the person is the owner.
#[must_use]
pub fn liveness(row: &SandboxRow, owner: &str) -> TokenLiveness {
    TokenLiveness {
        sandbox_id: row.id,
        conversation_id: row.conversation_id,
        person: row.person.clone(),
        state: row.state,
        live: row.state != SandboxState::Gone && row.person == owner,
        expires_at: ts(row.expires_at),
    }
}

impl App {
    /// When the router lists `session` as frozen (the time machine), resumes it, which thaws it; in the background:
    /// the use (a turn, an answer) is answered with the live sandbox and goes in once the supervisor is back.
    fn thaw_if_frozen(&self, session: &str) {
        let (app, sid) = (self.clone(), session.to_owned());
        tokio::spawn(async move {
            let frozen = match app.jarvis.sessions().await {
                Ok(list) => list
                    .iter()
                    .any(|r| r.id == sid && r.state == crate::jarvis::FROZEN),
                Err(e) => {
                    tracing::warn!(session = %sid, error = %e, "the session list for a use was not read");
                    return;
                }
            };
            if frozen {
                tracing::info!(session = %sid, "a use of a frozen session: thawing it");
                if let Err(e) = app.jarvis.resume(&sid).await {
                    tracing::warn!(session = %sid, error = %e, "the thaw was refused");
                }
            }
        });
    }

    /// The first get-or-start of a conversation begun in the chat: its session is proposed through the router (the
    /// defaults, idempotent by the conversation), recorded, and its start waits in the phase the router's state gives
    /// (`approval` until Deyao approves it in the Inbox; `reconcile` moves it on, or ends it when the session is
    /// denied). Only the owner's conversation gets one.
    async fn propose(&self, cid: Uuid, person: &str) -> Answer<PutSandbox> {
        let person = person.trim().to_lowercase();
        if person != self.owner {
            return Err(error::not_allowed());
        }
        let remote = match self.jarvis.propose(&format!("chat:{cid}")).await {
            Ok(Ok(r)) => r,
            Ok(Err(refused)) => {
                tracing::warn!(conversation = %cid, refused, "the router refused the new session");
                return Err(error::not_allowed());
            }
            Err(e) => return Err(internal(e)),
        };
        db::put_session(
            &self.pool,
            &SessionRow {
                session_id: remote.id.clone(),
                conversation_id: cid,
                person,
            },
        )
        .await
        .map_err(internal)?;
        // another replica's get-or-start of the same conversation may have started it already: that start is the one
        if let Some(row) = db::newest(&self.pool, cid).await.map_err(internal)?
            && row.state != SandboxState::Gone
        {
            return Ok(answer(row.sandbox()));
        }
        let session = db::session_of_conversation(&self.pool, cid)
            .await
            .map_err(internal)?
            .ok_or_else(error::not_allowed)?;
        let row = db::start(
            &self.pool,
            &session,
            None,
            crate::reconcile::start_phase(&remote.state),
            crate::app::LIFETIME,
            ReleaseReason::Vanished,
        )
        .await
        .map_err(internal)?;
        tracing::info!(conversation = %cid, session = %remote.id, state = %remote.state, "a new session proposed for the chat");
        Ok(PutSandbox::Starting(row.sandbox()))
    }

    /// The calling sandbox's live row, from the gateway's identity headers (404 `no_sandbox` otherwise).
    async fn own(&self, ctx: &Ctx) -> Answer<SandboxRow> {
        let id = ctx
            .sandbox()
            .ok_or_else(|| error::not_found("no_sandbox", "no sandbox identity"))?;
        let row = db::sandbox(&self.pool, id.id)
            .await
            .map_err(internal)?
            .filter(|r| {
                r.conversation_id == id.conversation
                    && r.person == id.person
                    && r.state != SandboxState::Gone
            })
            .ok_or_else(|| error::not_found("no_sandbox", "no such live sandbox"))?;
        Ok(row)
    }
}

/// The descriptor's control hashes: this hermit's for the sandbox, and on a fabric machine the fabric hermit's too
/// (its boot recorded them: src/fabric.rs).
fn control_hashes(app: &App, row: &SandboxRow) -> Vec<String> {
    let mut out = app.keys.control_hashes(row.id);
    if let Some(list) = row.boot.get("fabricControl").and_then(Value::as_array) {
        out.extend(list.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    out
}

/// The descriptor of one boot (hermit 3.7.1).
#[must_use]
pub fn descriptor(app: &App, row: &SandboxRow) -> SandboxSelf {
    let boot = &row.boot;
    let mut env: std::collections::BTreeMap<String, String> = boot
        .get("env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|(k, _)| crate::wire::BOOT_ENV_NAMES.contains(&k.as_str()))
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    // Claude Code and every process it starts reach the fixed names and the internet through the machine's gateway
    // (supervisor 6.2: the boot environment is not passed on)
    if let Some(proxy) = boot.get("proxy").and_then(Value::as_str) {
        for name in gateway_api::credential::PROXY_VARS {
            env.insert(name.to_owned(), proxy.to_owned());
        }
    }
    let memory = boot
        .get("memoryBytes")
        .and_then(Value::as_u64)
        .unwrap_or(4 << 30);
    let cpus = boot
        .get("cpus")
        .and_then(Value::as_u64)
        .and_then(|c| u32::try_from(c).ok())
        .unwrap_or(2);
    SandboxSelf {
        sandbox: SelfSandbox {
            id: row.id,
            conversation_id: row.conversation_id,
            person: row.person.clone(),
            created_at: ts(row.created_at),
            expires_at: ts(row.expires_at),
            image: row
                .image
                .clone()
                .unwrap_or_else(|| format!("sha256:{}", "0".repeat(64))),
        },
        control: Control {
            port: supervisor_api::CONTROL_PORT,
            token_sha256: control_hashes(app, row),
        },
        claude: Claude {
            managed_claude_md: MANAGED_CLAUDE_MD.to_owned(),
            managed_skills: std::collections::BTreeMap::new(),
            project_settings: true,
            // Claude Code compacts by itself at its token limit; Anthropic takes any number of images
            compact_at_images: None,
        },
        title: Title {
            system: TITLE_SYSTEM.to_owned(),
        },
        env,
        env_private: Vec::new(),
        resources: Resources {
            memory_bytes: memory,
            cpus,
            pids: 32_768,
            tmp_bytes: 16 << 30,
            open_files: 65_536,
            workspace: WorkspaceResources {
                path: "/workspace".into(),
                quota_bytes: 0,
                quota_inodes: 0,
            },
        },
        lifecycle: Lifecycle {
            activity_every_seconds: 15,
            // Jarvis 4 pauses an idle session itself (router/autopilot.go); hermit never ends one for idling
            idle_minutes: 30 * 24 * 60,
            max_lifetime_hours: 30 * 24,
            lifetime_grace_minutes: 60,
        },
        secrets: SecretKeys::default(),
        etag: String::new(),
    }
    .with_etag()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests
mod tests {
    use super::*;

    /// The deployment as built from the settings: the product, sign-in and conversations; the settings' features
    /// reach it (pigeon reads `features.personCards`) where the contract has them.
    #[test]
    fn the_deployment_comes_from_the_settings() {
        let mut settings =
            crate::settings::Settings::parse(include_str!("../tests/settings.json")).unwrap();
        let v = serde_json::to_value(deployment_of(&settings, "team.example.com", "aud").unwrap())
            .unwrap();
        assert_eq!(v["product"]["name"], settings.product.name.as_str());
        assert_eq!(v["signIn"]["access"][0]["audience"], "aud");
        assert_eq!(
            (
                &v["conversations"]["peopleCreate"],
                &v["conversations"]["peopleDelete"]
            ),
            (&serde_json::json!(true), &serde_json::json!(false))
        );
        assert!(
            v.get("features")
                .is_none_or(|f| f.get("personCards").is_none()),
            "off by default: {v}"
        );
        settings.features.person_cards = true;
        let v = serde_json::to_value(deployment_of(&settings, "team.example.com", "aud").unwrap())
            .unwrap();
        // an aviary whose contract has `features` keeps it; one without drops the member
        if let Some(f) = v.get("features") {
            assert_eq!(f["personCards"], true, "{v}");
        }
    }
}
