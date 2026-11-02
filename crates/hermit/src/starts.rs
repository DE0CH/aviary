//! The start task (hermit 11.2): any replica can run it, under the row's `start` claim. Each step is idempotent,
//! records its phase with `bump`, and makes its network calls outside any transaction, so a replica that takes the
//! claim over resumes from the row.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use hermit_api::sandbox::{ReleaseCode, ReleaseReason, SandboxPhase, SandboxState};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::{self, SandboxRow, bump, txt};
use crate::fabric::{CreateError, CreateSpec, Launch, Place};
use crate::sandboxes::{self, ReleaseKind};
use crate::supervisor_client::{self, Alive};
use crate::tasks::{self, Claim, Kind};
use crate::tokens::TokenFacts;
use crate::{Core, ms};

/// How a start ends when it does not reach `ready`.
#[derive(Debug)]
struct Fail {
    reason: ReleaseReason,
    note: Option<String>,
    message: String,
}

impl Fail {
    fn new(reason: ReleaseReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            note: None,
            message: message.into(),
        }
    }
}

enum Step {
    /// Go on with the row as it is now.
    Next,
    /// The claim was lost or the row is no longer ours to start: stop quietly.
    Stop,
    /// End the start.
    Fail(Fail),
}

impl From<aviary_pg::Error> for Step {
    fn from(e: aviary_pg::Error) -> Self {
        tracing::warn!(error = %e, "start step: database error (retried)");
        Step::Next
    }
}

/// The start task.
pub async fn run(core: Arc<Core>, claim: Claim) {
    let mut s = Start {
        core: core.clone(),
        claim,
        victims_skipped: HashMap::new(),
        phase_since: tokio::time::Instant::now(),
        phase_seen: None,
        create_tries: 0,
        ip_retry_done: false,
        place_failing_since: None,
    };
    s.run().await;
}

struct Start {
    core: Arc<Core>,
    claim: Claim,
    /// Pool victims that refused, skipped until the instant.
    victims_skipped: HashMap<Uuid, tokio::time::Instant>,
    /// When this task first saw the current phase (the per-phase waits restart after a takeover; the overall
    /// `start_deadline` bounds them).
    phase_since: tokio::time::Instant,
    phase_seen: Option<SandboxPhase>,
    /// Creates that found sandboxd unreachable (3 tries in all, 11.2 step 5).
    create_tries: u32,
    ip_retry_done: bool,
    /// When placement started failing (a fabric error, not "no slot"): bounded by `hostWaitSeconds`.
    place_failing_since: Option<tokio::time::Instant>,
}

/// Tries of a create that finds sandboxd unreachable before the start fails (11.2 step 5).
const CREATE_TRIES: u32 = 3;

impl Start {
    fn id(&self) -> Uuid {
        self.claim.id
    }

    async fn run(&mut self) {
        if !self.core.timings.start_pause.is_zero() {
            tokio::time::sleep(self.core.timings.start_pause).await;
        }
        loop {
            if !self.claim.held() {
                return;
            }
            let row = match db::load(&self.core.pool, self.id()).await {
                Ok(Some(r)) => r,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(sandbox = %self.id(), error = %e, "start: cannot read the row (retrying)");
                    tokio::time::sleep(self.core.timings.capacity_retry).await;
                    continue;
                }
            };
            if row.status != SandboxState::Starting
                || row.task.as_deref() != Some(Kind::Start.as_str())
                || row.task_owner.as_deref() != Some(&self.claim.owner)
            {
                return;
            }
            if self.phase_seen != row.phase {
                self.phase_seen = row.phase;
                self.phase_since = tokio::time::Instant::now();
            }
            // a forced or lost release asked meanwhile ends the start (the conversation was deleted, the person left)
            if let Some(p) = &row.release_pending
                && ReleaseKind::of(p) != ReleaseKind::Soft
            {
                let f = Fail {
                    reason: p.reason,
                    note: p.note.clone(),
                    message: format!("released while starting: {}", p.reason),
                };
                let code = p.code();
                self.fail_with(&row, f, code).await;
                return;
            }
            if OffsetDateTime::now_utc() > row.start_deadline {
                self.fail(
                    &row,
                    Fail::new(ReleaseReason::StartTimedOut, "the start crossed its deadline"),
                )
                .await;
                return;
            }
            let step = match row.phase.unwrap_or(SandboxPhase::Capacity) {
                SandboxPhase::Capacity => self.capacity(&row).await,
                SandboxPhase::Workspace => self.workspace(&row).await,
                SandboxPhase::Placing | SandboxPhase::Unknown => self.placing(&row).await,
                SandboxPhase::Host => self.host(&row).await,
                SandboxPhase::Image => self.image(&row).await,
                SandboxPhase::Creating => self.creating(&row).await,
                SandboxPhase::Supervisor => self.supervisor(&row).await,
            };
            match step {
                Step::Next => {}
                Step::Stop => return,
                Step::Fail(f) => {
                    self.fail(&row, f).await;
                    return;
                }
            }
        }
    }

    /// Moves to `phase` with `extra` set (a guarded update plus `bump`); `false` when the claim was lost.
    async fn advance(&self, phase: SandboxPhase, sets: &str, params: Vec<Param>) -> Result<bool, aviary_pg::Error> {
        let (id, owner, phase_s) = (self.id(), self.claim.owner.clone(), txt(phase));
        let sql = format!(
            "update sandboxes set phase = $3{}{sets} where id = $1 and status = 'starting' and task = 'start' and task_owner = $2",
            if sets.is_empty() { "" } else { ", " }
        );
        let row = aviary_pg::tx!(&self.core.pool, |t| -> Result<Option<SandboxRow>, aviary_pg::Error> {
            let mut p: Vec<&(dyn aviary_pg::ToSql + Sync)> = vec![&id, &owner, &phase_s];
            for x in &params {
                p.push(x.as_sql());
            }
            if t.execute(&sql, &p).await? == 0 {
                return Ok(None);
            }
            Ok(Some(bump(t, id).await?))
        })
        .await?;
        if let Some(r) = row {
            self.core.live.apply(&r);
            tracing::info!(sandbox = %id, phase = %phase, "sandbox phase");
            Ok(true)
        } else {
            self.claim.mark_lost();
            Ok(false)
        }
    }

    async fn capacity(&mut self, row: &SandboxRow) -> Step {
        if row.slot_claimed {
            return self.next(SandboxPhase::Workspace, "", vec![]).await;
        }
        let max = i64::from(self.core.cfg.sandbox().max_active);
        let (id, owner) = (self.id(), self.claim.owner.clone());
        let claimed = aviary_pg::tx!(&self.core.pool, |t| -> Result<Option<SandboxRow>, aviary_pg::Error> {
            t.execute(
                "select pg_advisory_xact_lock($1::int4, $2::int4)",
                &[&db::LOCK_CLASS, &db::LOCK_CAPACITY],
            )
            .await?;
            // waiting rows are served oldest first
            let older: i64 = t
                .query_one(
                    "select count(*) from sandboxes s, sandboxes me where me.id = $1 and s.status = 'starting'
                       and not s.slot_claimed and s.id <> me.id and s.created_at < me.created_at",
                    &[&id],
                )
                .await?
                .try_get(0)?;
            let live: i64 = t
                .query_one(
                    "select count(*) from sandboxes where status <> 'gone' and slot_claimed",
                    &[],
                )
                .await?
                .try_get(0)?;
            if older > 0 || live >= max {
                return Ok(None);
            }
            let n = t
                .execute(
                    "update sandboxes set slot_claimed = true, phase = 'workspace'
                     where id = $1 and status = 'starting' and task = 'start' and task_owner = $2",
                    &[&id, &owner],
                )
                .await?;
            if n == 0 {
                return Ok(None);
            }
            Ok(Some(bump(t, id).await?))
        })
        .await;
        match claimed {
            Ok(Some(r)) => {
                self.core.live.apply(&r);
                return Step::Next;
            }
            Ok(None) => {}
            Err(e) => return e.into(),
        }
        let waited = OffsetDateTime::now_utc() - row.created_at;
        if waited >= Duration::from_secs(u64::from(self.core.cfg.sandbox().capacity_wait_seconds)) {
            return Step::Fail(Fail::new(
                ReleaseReason::PoolFull,
                "no capacity: every sandbox is in use",
            ));
        }
        self.ask_victim(row).await;
        tokio::time::sleep(self.core.timings.capacity_retry).await;
        Step::Next
    }

    /// A pool victim (4.2): ready, not busy, no busy report for 90 s, idle for `poolVictimIdleSeconds`, its
    /// reservation passed, no live claim, another conversation, oldest `last_active_at` first.
    async fn ask_victim(&mut self, row: &SandboxRow) {
        let now = tokio::time::Instant::now();
        self.victims_skipped.retain(|_, until| *until > now);
        let skip: Vec<Uuid> = self.victims_skipped.keys().copied().collect();
        let s = self.core.cfg.sandbox();
        let idle = ms(Duration::from_secs(u64::from(s.pool_victim_idle_seconds)));
        let quiet = ms(self.core.timings.victim_quiet);
        let victim = async {
            self.core
                .pool
                .get()
                .await?
                .query_opt(
                    "select id from sandboxes
                     where status = 'ready' and not busy and conversation_id <> $1
                       and (last_busy_at is null or last_busy_at < now() - $2::bigint * interval '1 millisecond')
                       and last_active_at < now() - $3::bigint * interval '1 millisecond'
                       and (reserved_until is null or reserved_until < now())
                       and (task is null or task_until < now())
                       and release_pending is null
                       and not (id = any($4))
                     order by last_active_at limit 1",
                    &[&row.conversation_id, &quiet, &idle, &skip],
                )
                .await
        }
        .await;
        if let Ok(Some(v)) = victim
            && let Ok(vid) = v.try_get::<_, Uuid>(0)
        {
            tracing::info!(sandbox = %vid, for_sandbox = %row.id, "pool victim asked to release");
            self.victims_skipped.insert(vid, now + self.core.timings.victim_skip);
            if let Err(e) = sandboxes::request(&self.core, vid, ReleaseReason::Pool, false, None).await {
                tracing::warn!(sandbox = %vid, error = %e, "pool release request failed");
            }
        }
    }

    async fn workspace(&mut self, row: &SandboxRow) -> Step {
        let ws = &self.core.providers.workspaces;
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(self.core.timings.workspace_retry).await;
                if !self.claim.held() {
                    return Step::Stop;
                }
            }
            match ws.ensure(row.conversation_id).await {
                Ok(_) => return self.next(SandboxPhase::Placing, "", vec![]).await,
                Err(e) => last = e.to_string(),
            }
        }
        let mut f = Fail::new(ReleaseReason::WorkspaceUnavailable, last);
        f.note = Some("workspace".into());
        Step::Fail(f)
    }

    async fn placing(&mut self, row: &SandboxRow) -> Step {
        if row.host.is_some() && row.ip.is_some() {
            return self.next(SandboxPhase::Image, "", vec![]).await;
        }
        match self
            .core
            .providers
            .fabric
            .place(row.id, u64::try_from(row.memory_bytes).unwrap_or(0))
            .await
        {
            Ok(Place::Placed(p)) => {
                self.place_failing_since = None;
                let r = self
                    .advance(
                        SandboxPhase::Image,
                        "host = $4, ip = $5",
                        vec![Param::Text(p.host), Param::Ip(p.ip)],
                    )
                    .await;
                match r {
                    Ok(true) => Step::Next,
                    Ok(false) => Step::Stop,
                    // an address another sandbox took meanwhile (the unique index): place again
                    Err(e) => {
                        tracing::info!(sandbox = %row.id, error = %e, "placement refused (placing again)");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        Step::Next
                    }
                }
            }
            Ok(Place::NoSlot) => {
                self.place_failing_since = None;
                self.next(SandboxPhase::Host, "", vec![]).await
            }
            Err(e) => {
                // a fabric that keeps failing is a failed start after `hostWaitSeconds`, not an 8 min wait for the
                // start deadline
                let since = *self.place_failing_since.get_or_insert_with(tokio::time::Instant::now);
                let wait = Duration::from_secs(u64::from(self.core.cfg.sandbox().host_wait_seconds));
                if since.elapsed() >= wait {
                    let mut f = Fail::new(ReleaseReason::StartFailed, format!("placement failed: {e}"));
                    f.note = Some("placement".into());
                    return Step::Fail(f);
                }
                tracing::warn!(sandbox = %row.id, error = %e, "placement failed (retrying)");
                tokio::time::sleep(self.core.timings.host_poll).await;
                Step::Next
            }
        }
    }

    async fn host(&mut self, row: &SandboxRow) -> Step {
        match self.core.providers.fabric.request_host().await {
            Launch::Full => {
                return Step::Fail(Fail::new(
                    ReleaseReason::HostsFull,
                    "every host is in use and no more can be started",
                ));
            }
            Launch::Off => return Step::Fail(Fail::new(ReleaseReason::HostsFull, "hosts cannot be started now")),
            Launch::Launched | Launch::Planned => {}
        }
        let wait = Duration::from_secs(u64::from(self.core.cfg.sandbox().host_wait_seconds));
        loop {
            if !self.claim.held() {
                return Step::Stop;
            }
            match self
                .core
                .providers
                .fabric
                .place(row.id, u64::try_from(row.memory_bytes).unwrap_or(0))
                .await
            {
                Ok(Place::Placed(_)) => return self.next(SandboxPhase::Placing, "", vec![]).await,
                Ok(Place::NoSlot) | Err(_) => {}
            }
            if self.phase_since.elapsed() >= wait {
                return Step::Fail(Fail::new(ReleaseReason::NoHostInTime, "no host became ready in time"));
            }
            if OffsetDateTime::now_utc() > row.start_deadline {
                return Step::Next;
            }
            tokio::time::sleep(self.core.timings.host_poll).await;
        }
    }

    async fn image(&mut self, _row: &SandboxRow) -> Step {
        // the configured image is pinned by digest: the digest is the pin (13.14)
        let digest = self.core.cfg.image_digest.clone();
        self.next(SandboxPhase::Creating, "image = $4", vec![Param::Text(digest)])
            .await
    }

    /// The boot environment of hermit 5.1 with the derived token.
    fn boot_env(&self, row: &SandboxRow) -> Option<Vec<(String, String)>> {
        let token = sandbox_token(&self.core, row)?;
        let proxy = self.core.cfg.fill(&self.core.cfg.sandbox().proxy, &token);
        let mut env: Vec<(String, String)> = ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"]
            .iter()
            .map(|k| ((*k).to_owned(), proxy.clone()))
            .collect();
        for k in ["NO_PROXY", "no_proxy"] {
            env.push((k.to_owned(), "localhost,127.0.0.1,::1".to_owned()));
        }
        env.push(("HOME".into(), "/home/sandbox".into()));
        Some(env)
    }

    async fn creating(&mut self, row: &SandboxRow) -> Step {
        let (Some(host), Some(ip)) = (row.host.clone(), row.ip) else {
            return self.next(SandboxPhase::Placing, "", vec![]).await;
        };
        let Some(env) = self.boot_env(row) else {
            return Step::Fail(Fail::new(
                ReleaseReason::StartFailed,
                "the row's token key is not loaded",
            ));
        };
        let s = self.core.cfg.sandbox();
        let spec = CreateSpec {
            name: row.name.clone(),
            sandbox_id: row.id,
            host,
            ip,
            image: row.image.clone().unwrap_or_else(|| self.core.cfg.image_digest.clone()),
            image_ref: s.image.clone(),
            env,
            cpu: row.cpu,
            memory_bytes: u64::try_from(row.memory_bytes).unwrap_or(0),
            pids: s.pids,
            open_files: s.open_files,
            workspace: row.conversation_id,
        };
        match self.core.providers.fabric.create(&spec).await {
            Ok(()) => self.next(SandboxPhase::Supervisor, "", vec![]).await,
            Err(CreateError::Busy) => {
                tokio::time::sleep(self.core.timings.busy_retry).await;
                Step::Next
            }
            Err(CreateError::IpInUse(other)) if !self.ip_retry_done => {
                self.ip_retry_done = true;
                tracing::info!(sandbox = %row.id, stale = %other, "address in use by a stale sandbox: destroying it");
                let _ = self.core.providers.fabric.destroy(Some(&spec.host), None, &other).await;
                Step::Next
            }
            Err(CreateError::Unreachable(why)) if self.create_tries + 1 < CREATE_TRIES => {
                self.create_tries += 1;
                tracing::warn!(sandbox = %row.id, error = %why, tries = self.create_tries, "sandboxd unreachable (retrying)");
                tokio::time::sleep(self.core.timings.busy_retry).await;
                Step::Next
            }
            Err(e) => {
                let class = match &e {
                    CreateError::Failed { class, .. } => class.clone(),
                    CreateError::Unreachable(_) => "unreachable".into(),
                    CreateError::IpInUse(_) => "ip_in_use".into(),
                    CreateError::Busy => "busy".into(),
                };
                let mut f = Fail::new(ReleaseReason::StartFailed, e.to_string());
                f.note = Some(db::clip(&class, 200));
                Step::Fail(f)
            }
        }
    }

    async fn supervisor(&mut self, row: &SandboxRow) -> Step {
        let wait = Duration::from_secs(u64::from(self.core.cfg.sandbox().supervisor_wait_seconds));
        loop {
            if !self.claim.held() {
                return Step::Stop;
            }
            if let Alive::Answering(h) = supervisor_client::health(&self.core, row).await
                && h.ready
            {
                return self.ready(row, &h).await;
            }
            if self.phase_since.elapsed() >= wait {
                return Step::Fail(Fail::new(
                    ReleaseReason::SupervisorTimeout,
                    "the supervisor did not become ready in time",
                ));
            }
            if OffsetDateTime::now_utc() > row.start_deadline {
                return Step::Next;
            }
            // a release asked meanwhile is seen by the main loop
            if let Ok(Some(now)) = db::load(&self.core.pool, row.id).await
                && now
                    .release_pending
                    .as_ref()
                    .is_some_and(|p| ReleaseKind::of(p) != ReleaseKind::Soft)
            {
                return Step::Next;
            }
            tokio::time::sleep(self.core.timings.health_poll).await;
        }
    }

    async fn ready(&mut self, row: &SandboxRow, h: &supervisor_api::control::Health) -> Step {
        let (id, owner) = (self.id(), self.claim.owner.clone());
        let (version, protocol, boot) = (
            h.version.clone(),
            i32::try_from(h.protocol).unwrap_or(0),
            h.boot_id.clone(),
        );
        let created = row.created_at;
        let host = row.host.clone();
        let (cid, person) = (row.conversation_id, row.person.clone());
        let ready = aviary_pg::tx!(&self.core.pool, |t| -> Result<Option<SandboxRow>, aviary_pg::Error> {
            let n = t
                .execute(
                    "update sandboxes set status = 'ready', phase = null, ready_at = now(),
                       last_active_at = greatest(last_active_at, now()), supervisor_version = $3,
                       supervisor_protocol = $4, boot_id = $5, task = null, task_owner = null, task_until = null
                     where id = $1 and status = 'starting' and task = 'start' and task_owner = $2",
                    &[&id, &owner, &version, &protocol, &boot],
                )
                .await?;
            if n == 0 {
                return Ok(None);
            }
            let total =
                i32::try_from(((OffsetDateTime::now_utc() - created).whole_milliseconds()).max(0)).unwrap_or(i32::MAX);
            // start_ms = now - the start of step 5 (11.2 step 6): the feed event that recorded `creating` first, so
            // a takeover does not restart it
            t.execute(
                "insert into sandbox_starts (ok, ms, start_ms, sandbox_id, conversation_id, person, host)
                 values (true, $1, (select (extract(epoch from now() - min(at)) * 1000)::int from sandbox_events
                                    where sandbox_id = $2 and phase = 'creating'), $2, $3, $4, $5)",
                &[&total, &id, &cid, &person, &host],
            )
            .await?;
            Ok(Some(bump(t, id).await?))
        })
        .await;
        match ready {
            Ok(Some(r)) => {
                self.core.live.apply(&r);
                tracing::info!(sandbox = %id, conversation = %r.conversation_id, "sandbox ready");
                // a release that became pending meanwhile runs now (11.2 step 6)
                if let Some(p) = &r.release_pending
                    && let Err(e) = sandboxes::request(&self.core, id, p.reason, p.force, p.note.clone()).await
                {
                    tracing::warn!(sandbox = %id, error = %e, "pending release after ready");
                }
                Step::Stop
            }
            Ok(None) => {
                self.claim.mark_lost();
                Step::Stop
            }
            Err(e) => e.into(),
        }
    }

    async fn next(&self, phase: SandboxPhase, sets: &str, params: Vec<Param>) -> Step {
        match self.advance(phase, sets, params).await {
            Ok(true) => Step::Next,
            Ok(false) => Step::Stop,
            Err(e) => e.into(),
        }
    }

    async fn fail(&self, row: &SandboxRow, f: Fail) -> Step {
        let code = f.reason.code().unwrap_or(ReleaseCode::SandboxFailed);
        self.fail_with(row, f, code).await
    }

    /// Ends the start: destroys whatever was created, records the failed start, `gone`.
    async fn fail_with(&self, row: &SandboxRow, f: Fail, code: ReleaseCode) -> Step {
        tracing::info!(sandbox = %row.id, reason = %f.reason, error = %f.message, "sandbox start failed");
        let ms_total = i32::try_from(((OffsetDateTime::now_utc() - row.created_at).whole_milliseconds()).max(0))
            .unwrap_or(i32::MAX);
        let (id, cid, person, host) = (row.id, row.conversation_id, row.person.clone(), row.host.clone());
        let (reason_s, err) = (txt(f.reason), db::message(&f.message));
        if let Ok(c) = self.core.pool.get().await {
            let _ = c
                .execute(
                    "insert into sandbox_starts (ok, ms, sandbox_id, conversation_id, person, host, reason, error)
                     values (false, $1, $2, $3, $4, $5, $6, $7)",
                    &[&ms_total, &id, &cid, &person, &host, &reason_s, &err],
                )
                .await;
        }
        if let Err(e) =
            sandboxes::destroy_and_gone(&self.core, &self.claim, row, f.reason, f.note, code, &f.message).await
        {
            tracing::warn!(sandbox = %id, error = %e, "start failure not recorded (the takeover resumes it)");
        }
        Step::Stop
    }
}

/// The sandbox token of a row (`None` while it has no kid loaded).
#[must_use]
pub fn sandbox_token(core: &Core, row: &SandboxRow) -> Option<String> {
    let kid = u32::try_from(row.token_kid?).ok()?;
    core.keys.sandbox_token(
        kid,
        &TokenFacts {
            sandbox_id: row.id,
            conversation_id: row.conversation_id,
            ip: row.ip,
            expires_at: row.hard_expires_at.unix_timestamp(),
            person: &row.person,
        },
    )
}

/// A typed parameter of [`Start::advance`].
enum Param {
    Text(String),
    Ip(IpAddr),
}

impl Param {
    fn as_sql(&self) -> &(dyn aviary_pg::ToSql + Sync) {
        match self {
            Self::Text(s) => s,
            Self::Ip(i) => i,
        }
    }
}

/// Re-runs a stuck start's failure path when its claim lapsed past the deadline (janitor step 6).
///
/// # Errors
/// A database error.
pub async fn end_stuck(core: &Arc<Core>, id: Uuid) -> Result<(), aviary_pg::Error> {
    if let Some(claim) = tasks::take(core, id, Kind::Start).await? {
        tasks::run(core, claim);
    }
    Ok(())
}
