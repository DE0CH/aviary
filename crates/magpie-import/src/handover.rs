//! The hand-over loop (cutover 7.5, DESIGN 9.4): `magpie import handover`, the import Deployment's command. Every
//! pass, as the leader: a pass over the sources (sync), pre-verification of what is synced and stable, upload
//! ingestion, a running-set read, the running mirror, the stranded tickets' expiry, and, once the gate is set (for
//! everyone, or for the conversation's person: `allow-handover --person`), the release of each conversation that meets
//! every condition of DESIGN 9.4; warm hints to hermit every minute while any conversation is legacy. A pass starts by
//! settling the fences a previous lock holder left ([`recover`]: a leader killed between fence and release). Nothing is ever released by a deadline: a condition that cannot be shown keeps the
//! conversation legacy, and any doubt inside the fence lifts it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use magpie::legacy::{fence, mirror, tickets};
use magpie_api::conversations::RunState;
use magpie_api::events::NameSource;
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::gate;
use crate::holds::RunningSet;
use crate::kube::{self, Kube};
use crate::schedules::Abort;
use crate::uploads;
use crate::{Error, Importer, Result};

/// A pass every 5 s (or at once when one took longer).
pub const CADENCE: Duration = Duration::from_secs(5);
/// The quiet period (DESIGN 9.4 condition 3).
pub const QUIET: time::Duration = time::Duration::seconds(60);
/// Warm hints to hermit (DESIGN 9.4).
pub const WARM_EVERY: Duration = Duration::from_secs(60);

/// One running-set read and when it started (the database's clock, [`tickets::now`]).
#[derive(Debug, Clone)]
pub struct Read {
    /// Taken right before the read.
    pub started: OffsetDateTime,
    /// The read.
    pub set: RunningSet,
}

/// What a legacy conversation waits for (the status document's `waitingFor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Wait {
    /// The gate (for everyone or for its person) is not set, or a ui report is stale or shows the old path in use.
    Gate,
    /// `hold on`.
    Hold,
    /// Not verified, or its sources changed since.
    Verify,
    /// An old sandbox of it is not `gone`.
    OldSandbox,
    /// A running-set read is incomplete or its schema has a child that did not answer.
    HoldsUnanswered,
    /// Listed running, or fewer than two reads since its last ticket closed.
    Running,
    /// An open ticket.
    Ticket,
    /// Less than [`QUIET`] since its last ticket activity.
    Quiet,
}

/// One legacy conversation's facts for the decision.
#[derive(Debug, Clone)]
pub struct Facts {
    /// Its person's schema (`cc_u…`).
    pub schema: String,
    /// Pre-verified, and its sources unchanged since.
    pub verified: bool,
    /// Its open tickets.
    pub open_tickets: i64,
    /// When its last ticket closed.
    pub last_close: Option<OffsetDateTime>,
    /// Its last ticket activity (issued, delivered or closed).
    pub last_activity: Option<OffsetDateTime>,
    /// An old sandbox row of it that is not `gone`.
    pub old_sandbox: bool,
}

/// DESIGN 9.4's conditions 1-5 for conversation `id` at `now` (the database's clock), given the gate, the hold and
/// the last two running-set reads (oldest first): `Ok` when it may be fenced, else what it waits for.
///
/// # Errors
/// The first condition that fails, as [`Wait`].
pub fn decide(
    id: &str,
    f: &Facts,
    gate_open: bool,
    hold: bool,
    reads: &[Read],
    now: OffsetDateTime,
) -> Result<(), Wait> {
    if hold {
        return Err(Wait::Hold);
    }
    if !gate_open {
        return Err(Wait::Gate);
    }
    if !f.verified {
        return Err(Wait::Verify);
    }
    if f.old_sandbox {
        return Err(Wait::OldSandbox);
    }
    let [a, b] = reads else {
        return Err(Wait::Running);
    };
    for r in [a, b] {
        if !r.set.complete || r.set.schemas.get(&f.schema).is_some_and(|s| !s.ok) {
            return Err(Wait::HoldsUnanswered);
        }
        if r.set.may_run(&f.schema, id) {
            return Err(Wait::Running);
        }
        if f.last_close.is_some_and(|c| r.started <= c) {
            return Err(Wait::Running);
        }
    }
    if f.open_tickets > 0 {
        return Err(Wait::Ticket);
    }
    if f.last_activity.is_some_and(|t| now - t < QUIET) {
        return Err(Wait::Quiet);
    }
    Ok(())
}

/// One open conversation in the status document.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenEntry {
    /// The conversation.
    pub conversation: Uuid,
    /// Its person's key.
    pub person_key: String,
    /// What it waits for.
    pub waiting_for: Wait,
}

/// The last pass.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassInfo {
    /// When it started.
    #[serde(with = "time::serde::rfc3339::option")]
    pub at: Option<OffsetDateTime>,
    /// How long it took.
    pub ms: u128,
    /// Conversations the sync changed.
    pub changed: usize,
    /// Released in it.
    pub released: usize,
    /// Released conversations whose sources changed since their release, checked (and their old-side writes
    /// counted) in it.
    pub released_changed: usize,
    /// Its own error, when it stopped.
    pub error: Option<String>,
}

/// The loop's status (`GET :9090/status`, cutover 7.9), beside what [`crate::control::status`] reads.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopStatus {
    /// This process leads.
    pub leader: bool,
    /// The last pass.
    pub pass: PassInfo,
    /// What each open conversation waits for (after the last pass).
    pub open: Vec<OpenEntry>,
    /// The last gate check's sockets: not LU, LU.
    pub old_path_sockets: u64,
    /// LU sockets.
    pub lu_sockets: u64,
    /// ui pods in the last check.
    pub ui_pods: usize,
    /// The last gate check was clean.
    pub reports_fresh: bool,
    /// The last gate check's problems.
    pub gate_problems: Vec<String>,
    /// Old sandboxes not `gone`.
    pub old_live_sandboxes: usize,
    /// Released by this process.
    pub released_total: u64,
    /// The per-person gates' last checks (before the gate for everyone; persons with an open conversation).
    pub person_gates: Vec<PersonGate>,
}

/// One per-person gate's check in the status document.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonGate {
    /// The person's key (never the email).
    pub person_key: String,
    /// Their part of every report was clean.
    pub clean: bool,
    /// The problems, one line per pod.
    pub problems: Vec<String>,
}

/// The loop's state.
pub struct Handover {
    /// The old cluster's API.
    pub kube: Kube,
    /// The switch module's token (`MAGPIE_IMPORT_TOKEN_UI`).
    pub ui_token: String,
    /// The status, shared with the ops route.
    pub status: Arc<RwLock<LoopStatus>>,
    reads: VecDeque<Read>,
    seen: uploads::Seen,
    last_warm: Option<tokio::time::Instant>,
}

impl std::fmt::Debug for Handover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handover")
            .field("reads", &self.reads.len())
            .finish_non_exhaustive()
    }
}

/// One open conversation's row.
struct OpenRow {
    id: Uuid,
    person: String,
    key: String,
    schema: String,
    error: bool,
}

impl Handover {
    /// The loop's state.
    #[must_use]
    pub fn new(kube: Kube, ui_token: String) -> Self {
        Self {
            kube,
            ui_token,
            status: Arc::new(RwLock::new(LoopStatus::default())),
            reads: VecDeque::new(),
            seen: uploads::Seen::new(),
            last_warm: None,
        }
    }

    /// One pass (the caller has confirmed leadership).
    ///
    /// # Errors
    /// The sync's own failure (the mount, a source read, the magpie database); the lock lost.
    #[allow(clippy::too_many_lines)] // 7.5's steps in order
    pub async fn pass(&mut self, imp: &Importer) -> Result<PassInfo> {
        let at = OffsetDateTime::now_utc();
        let started = std::time::Instant::now();
        let mut info = PassInfo {
            at: Some(at),
            ..PassInfo::default()
        };
        // the fences a killed leader (or reverse hand-over) left, before anything syncs
        let rec = recover(imp, &mut self.seen).await?;
        if rec != Recovered::default() {
            tracing::info!(recovered = ?rec, "fences left by the last lock holder");
        }
        let pass_started = tickets::now(&imp.magpie.pool).await?;
        let report = crate::pass::pass(imp, None).await?;
        info.changed = report.changed + report.created;
        let open = open_rows(imp).await?;
        self.preverify(imp, &open, pass_started).await;
        let mut persons: BTreeMap<&str, &str> = BTreeMap::new();
        for o in &open {
            persons.insert(&o.person, &o.key);
        }
        for (p, k) in &persons {
            if let Err(e) = uploads::person(imp, p, k, &mut self.seen).await {
                tracing::warn!(person_key = %k, error = %e, "the old asset folder was not read");
            }
        }
        // the running sets: the database clock first, so a ticket issued during the read is after `started`
        let read_started = tickets::now(&imp.magpie.pool).await?;
        let set = kube::running_set(&self.kube).await;
        self.reads.push_back(Read {
            started: read_started,
            set,
        });
        while self.reads.len() > 2 {
            self.reads.pop_front();
        }
        let latest = self.reads.back().cloned();
        let facts = facts(imp, &open).await?;
        // the running mirror and the stranded tickets, from the latest read
        if let Some(r) = &latest
            && r.set.complete
        {
            for o in &open {
                if r.set.schemas.get(&o.schema).is_some_and(|s| !s.ok) {
                    continue;
                }
                let running = r.set.may_run(&o.schema, &o.id.to_string());
                let state = if running { RunState::Running } else { RunState::Idle };
                if let Err(e) = mirror::run_state(&imp.magpie.pool, o.id, state).await {
                    tracing::warn!(conversation = %o.id, error = %e, "the running mirror was not written");
                }
                if !running && facts.get(&o.id).is_some_and(|f| f.open_tickets > 0) {
                    match tickets::expire_settled(&imp.magpie.pool, o.id, r.started).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(conversation = %o.id, tickets = n, "settled tickets closed"),
                        Err(e) => tracing::warn!(conversation = %o.id, error = %e, "tickets not expired"),
                    }
                }
            }
        }
        let control = imp
            .magpie
            .pool
            .get()
            .await?
            .query_one("select handover_allowed_at is not null, hold from import_control", &[])
            .await?;
        let (gate_set, hold): (bool, bool) = (control.get(0), control.get(1));
        let check = if gate_set {
            Some(gate::check(&self.kube, &self.ui_token, None).await)
        } else {
            None
        };
        let gate_open = check.as_ref().is_some_and(gate::Check::clean);
        // before the gate for everyone: the per-person gates (cutover 7.9), each person with an open conversation
        // judged on their own part of every report
        let mut person_open: BTreeMap<String, bool> = BTreeMap::new();
        let mut person_gates = Vec::new();
        if !gate_set {
            let gated: Vec<String> = imp
                .magpie
                .pool
                .get()
                .await?
                .query("select person from import_gate_persons order by person", &[])
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect();
            for p in gated.iter().filter(|p| persons.contains_key(p.as_str())) {
                let c = gate::check(&self.kube, &self.ui_token, Some(p)).await;
                person_open.insert(p.clone(), c.clean());
                person_gates.push(PersonGate {
                    person_key: aviary_core::log::person_key(p),
                    clean: c.clean(),
                    problems: c.problems(),
                });
            }
        }
        // the facts again after the expiry (an expired ticket is a close: two more reads must follow it)
        let facts = self::facts(imp, &open).await?;
        let now = tickets::now(&imp.magpie.pool).await?;
        let reads: Vec<Read> = self.reads.iter().cloned().collect();
        let mut waiting = Vec::new();
        let mut released = 0;
        for o in &open {
            let Some(f) = facts.get(&o.id) else { continue };
            let open_for = gate_open || person_open.get(&o.person).copied().unwrap_or(false);
            let verdict = if o.error {
                Err(Wait::Verify)
            } else {
                decide(&o.id.to_string(), f, open_for, hold, &reads, now)
            };
            match verdict {
                Ok(()) => match release(imp, o, &mut self.seen).await {
                    Ok(true) => released += 1,
                    Ok(false) => waiting.push((o, Wait::Verify)),
                    Err(e @ Error::LockLost) => return Err(e),
                    Err(e) => {
                        tracing::warn!(conversation = %o.id, error = %e, "not released");
                        waiting.push((o, Wait::Verify));
                    }
                },
                Err(w) => waiting.push((o, w)),
            }
        }
        if !open.is_empty() && self.last_warm.is_none_or(|t| t.elapsed() >= WARM_EVERY) {
            self.last_warm = Some(tokio::time::Instant::now());
            for p in persons.keys() {
                let body = hermit_api::people::WarmRequest {
                    person: (*p).to_owned(),
                    conversation_id: None,
                };
                if let Err(e) = imp.magpie.hermit.warm(&body).await {
                    tracing::debug!(error = %e, "warm hint not taken");
                }
            }
        }
        // the released conversations the old side wrote since their release (DESIGN 9.9's
        // import_verify_mismatch_total, live on this process's registry)
        info.released_changed = self.check_released(imp).await;
        info.released = released;
        info.ms = started.elapsed().as_millis();
        let mut st = self.status.write().await;
        st.leader = true;
        st.pass = info.clone();
        st.released_total += released as u64;
        st.open = waiting
            .iter()
            .map(|(o, w)| OpenEntry {
                conversation: o.id,
                person_key: o.key.clone(),
                waiting_for: *w,
            })
            .collect();
        st.old_live_sandboxes = facts.values().filter(|f| f.old_sandbox).count();
        st.person_gates = person_gates;
        if let Some(c) = &check {
            st.old_path_sockets = c.old_path();
            st.lu_sockets = c.lu();
            st.ui_pods = c.pods.len();
            st.reports_fresh = c.clean();
            st.gate_problems = c.problems();
        }
        Ok(info)
    }

    /// Every released conversation (but a forced release, whose later writes are the cut-off turn's, cutover 7.10)
    /// whose source files' stats differ from what its release recorded, at stats it was not checked at before
    /// (`import_sources.released_checked`, kept across processes): the released check (`verify`'s, which hashes the files) runs and counts each file the old side wrote in
    /// `magpie_import_verify_mismatch_total`. A stat alone is cheap; a file is hashed only after its stat changed, and
    /// a change is counted once (a later write changes the stats again and is checked again). Answers how many
    /// conversations were checked.
    async fn check_released(&self, imp: &Importer) -> usize {
        let rows = match imp.magpie.pool.get().await {
            Ok(c) => c
                .query(
                    "select conversation_id, user_key, provider_session, files, released_checked from import_sources
                     where released_at is not null and release_reason is distinct from 'forced'",
                    &[],
                )
                .await
                .map_err(Error::from),
            Err(e) => Err(Error::from(e)),
        };
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "released conversations not read: none checked");
                return 0;
            }
        };
        let mut checked = 0;
        for r in &rows {
            let id: Uuid = r.get(0);
            let Ok(files) = serde_json::from_value::<crate::sync::Files>(r.get(3)) else {
                continue;
            };
            let psid = r.get::<_, Option<String>>(2).unwrap_or_default();
            let (recorded, now) = match crate::verify::released_stats(imp, id, r.get(1), &psid, &files) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(conversation = %id, error = %e, "a released conversation's sources not read");
                    continue;
                }
            };
            let before: Option<crate::verify::Stats> = r
                .get::<_, Option<serde_json::Value>>(4)
                .and_then(|v| serde_json::from_value(v).ok());
            if recorded == now || before.as_ref() == Some(&now) {
                continue;
            }
            checked += 1;
            match crate::verify::released_check(imp, id).await {
                Ok(v) if v.mismatches.is_empty() => {
                    tracing::info!(conversation = %id, "a released conversation's sources changed stat, not bytes");
                }
                Ok(v) => tracing::error!(conversation = %id, files = ?v.mismatches,
                                         "the old side wrote a released conversation"),
                Err(e) => {
                    tracing::warn!(conversation = %id, error = %e, "a released conversation not checked");
                    continue;
                }
            }
            let stats = serde_json::to_value(&now).unwrap_or_default();
            let saved = match imp.magpie.pool.get().await {
                Ok(c) => c
                    .execute(
                        "update import_sources set released_checked = $2 where conversation_id = $1",
                        &[&id, &stats],
                    )
                    .await
                    .map(drop)
                    .map_err(Error::from),
                Err(e) => Err(Error::from(e)),
            };
            if let Err(e) = saved {
                tracing::warn!(conversation = %id, error = %e, "a released check's stats not kept (checked again)");
            }
        }
        checked
    }

    /// Verifies every open conversation that is synced and stable but not verified as it is now (cutover 7.5: "if
    /// fingerprints unchanged: (re-)pre-verify if not yet verified"): one this pass found changed waits for a pass that
    /// finds it unchanged; one with an error waits for the sync that clears it.
    async fn preverify(&self, imp: &Importer, open: &[OpenRow], pass_started: OffsetDateTime) {
        let changed: std::collections::BTreeSet<Uuid> = match imp.magpie.pool.get().await {
            Ok(c) => match c
                .query(
                    "select conversation_id from import_sources where last_change_at >= $1",
                    &[&pass_started],
                )
                .await
            {
                Ok(rows) => rows.iter().map(|r| r.get(0)).collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "changed conversations not read: nothing pre-verified");
                    return;
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "changed conversations not read: nothing pre-verified");
                return;
            }
        };
        for o in open.iter().filter(|o| !o.error && !changed.contains(&o.id)) {
            let stable = matches!(crate::verify::changed_since_sync(imp, o.id).await, Ok(c) if c.is_empty());
            let fresh = matches!(crate::verify::changed_since_verify(imp, o.id).await, Ok(c) if c.is_empty());
            if stable && !fresh {
                match crate::verify::verify(imp, o.id).await {
                    Ok(v) if v.ok() => tracing::info!(conversation = %o.id, "pre-verified"),
                    Ok(v) => tracing::warn!(conversation = %o.id, mismatches = v.mismatches.len(), "not verified"),
                    Err(e) => tracing::warn!(conversation = %o.id, error = %e, "verification failed"),
                }
            }
        }
    }
}

async fn open_rows(imp: &Importer) -> Result<Vec<OpenRow>> {
    Ok(imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select conversation_id, person, user_key, source_schema, last_error is not null
             from import_sources where released_at is null order by last_change_at desc",
            &[],
        )
        .await?
        .iter()
        .map(|r| OpenRow {
            id: r.get(0),
            person: r.get(1),
            key: r.get(2),
            schema: r.get(3),
            error: r.get(4),
        })
        .collect())
}

async fn facts(imp: &Importer, open: &[OpenRow]) -> Result<BTreeMap<Uuid, Facts>> {
    let ids: Vec<Uuid> = open.iter().map(|o| o.id).collect();
    let mut out = BTreeMap::new();
    for o in open {
        let verified = matches!(crate::verify::changed_since_verify(imp, o.id).await, Ok(c) if c.is_empty());
        out.insert(
            o.id,
            Facts {
                schema: o.schema.clone(),
                verified,
                open_tickets: 0,
                last_close: None,
                last_activity: None,
                old_sandbox: false,
            },
        );
    }
    for r in imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select conversation_id, count(*) filter (where done_at is null), max(done_at),
                    greatest(max(issued_at), max(delivered_at), max(done_at))
             from legacy_tickets where conversation_id = any($1) group by conversation_id",
            &[&ids],
        )
        .await?
    {
        if let Some(f) = out.get_mut(&r.get::<_, Uuid>(0)) {
            f.open_tickets = r.get(1);
            f.last_close = r.get(2);
            f.last_activity = r.get(3);
        }
    }
    // the old janitor's rows (read-only): any status but `gone` holds the conversation (condition 4)
    for r in imp
        .jarvis
        .get()
        .await?
        .query(
            "select distinct conversation_id from sandboxes where status <> 'gone' and conversation_id = any($1)",
            &[&ids],
        )
        .await?
    {
        if let Some(f) = out.get_mut(&r.get::<_, Uuid>(0)) {
            f.old_sandbox = true;
        }
    }
    Ok(out)
}

/// The strict release of cutover 7.8 for one conversation that met every condition: fence (no open ticket), then
/// [`finish`].
async fn release(imp: &Importer, o: &OpenRow, seen: &mut uploads::Seen) -> Result<bool> {
    match fence::fence(&imp.magpie.pool, o.id).await? {
        fence::Fence::Fenced(_) => {}
        other => {
            tracing::info!(conversation = %o.id, fence = ?other, "not fenced");
            return Ok(false);
        }
    }
    let fenced = std::time::Instant::now();
    crate::pause(imp.pause_in_fence).await;
    finish(imp, o, seen, fenced).await
}

/// What follows a fence (cutover 7.8): one more ingestion of the person's old asset folder, a re-stat of every source
/// and fingerprint against what verification recorded, then the release transaction (`release_reason = 'idle'`, a
/// provisional title kept as the title model's so it is never retitled). Anything that changed, failed or ran past
/// the fence's lease lifts the fence and leaves the conversation legacy (`Ok(false)` or an error); nothing is synced
/// or cut inside the fence.
async fn finish(imp: &Importer, o: &OpenRow, seen: &mut uploads::Seen, fenced: std::time::Instant) -> Result<bool> {
    let pool = &imp.magpie.pool;
    let inside = async {
        let ing = uploads::person(imp, &o.person, &o.key, seen).await?;
        if !ing.failed.is_empty() {
            return Ok(vec![format!("old assets not ingested: {}", ing.failed.join(", "))]);
        }
        crate::verify::changed_since_verify(imp, o.id).await
    }
    .await;
    let changes = match inside {
        Ok(c) => c,
        Err(e) => vec![format!("the re-stat failed: {e}")],
    };
    if !changes.is_empty() {
        let _ = fence::unfence(pool, o.id).await;
        tracing::info!(conversation = %o.id, changes = ?changes, "changed inside the fence: unfenced");
        return Ok(false);
    }
    // its parked questions go with it (DECISIONS 17); an answered one waits for its turn on the old stack
    let questions = match crate::questions::read(&imp.cloudcli, &o.schema, o.id).await {
        Ok(q) if q.iter().any(|q| q.answer.is_some()) => {
            let _ = fence::unfence(pool, o.id).await;
            tracing::info!(conversation = %o.id, "an answered question waits for its turn: unfenced");
            return Ok(false);
        }
        Ok(q) => q,
        Err(e) => {
            let _ = fence::unfence(pool, o.id).await;
            tracing::warn!(conversation = %o.id, error = %e, "its open questions could not be read: unfenced");
            return Ok(false);
        }
    };
    // its schedules go with it: taken on the old database around magpie's release (schedules::claim), so no fire is
    // made by both stacks or by neither
    let id = o.id;
    let questions = &questions;
    let committed: std::sync::Mutex<Option<fence::Released>> = std::sync::Mutex::new(None);
    let committed_ref = &committed;
    let held = aviary_pg::tx!(&imp.cloudcli_schedules, |old| -> std::result::Result<
        fence::Released,
        Abort,
    > {
        let claimed = crate::schedules::claim(old, id).await?;
        if claimed.fired_within(QUIET) {
            return Err(Abort::Fired);
        }
        let schedules = &claimed.schedules;
        let r = aviary_pg::tx!(pool, |t| -> Result<Result<fence::Released, fence::NotReleased>> {
            imp.check_lock_in(t).await?;
            let provisional: bool = t
                .query_one("select name_source is null from conversations where id = $1", &[&id])
                .await?
                .get(0);
            let r = fence::Release {
                model: None,
                name_source: provisional.then_some(NameSource::Title),
            };
            let out = fence::release_in(t, id, &r).await?;
            if out.is_ok() {
                t.execute(
                    "update import_sources set released_at = now(), release_reason = 'idle' where conversation_id = $1",
                    &[&id],
                )
                .await?;
                crate::questions::carry(t, &o.person, id, questions).await?;
                crate::schedules::carry(t, &o.person, id, schedules).await?;
            }
            Ok(out)
        })
        .await
        .map_err(Abort::Failed)?;
        match r {
            Ok(r) => {
                *committed_ref.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
                Ok(r)
            }
            Err(why) => Err(Abort::NotReleased(why)),
        }
    })
    .await;
    let magpie_released = *committed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match (held, magpie_released) {
        (Ok(r), _) => {
            r.record(&imp.magpie.legacy);
            tracing::info!(conversation = %o.id, held_s = r.held, in_fence_ms = fenced.elapsed().as_millis(), "released");
            Ok(true)
        }
        (Err(e), Some(r)) => {
            // magpie released it; the old database's commit failed, so its rows are marked moved apart
            tracing::warn!(conversation = %o.id, error = ?e, "released, but the old schedules' commit failed");
            crate::schedules::mark_moved(&imp.cloudcli_schedules, id).await;
            r.record(&imp.magpie.legacy);
            Ok(true)
        }
        (Err(Abort::Fired), None) => {
            let _ = fence::unfence(pool, o.id).await;
            tracing::info!(conversation = %o.id, "a schedule of it fired within the quiet period: unfenced");
            Ok(false)
        }
        (Err(Abort::NotReleased(why)), None) => {
            let _ = fence::unfence(pool, o.id).await;
            tracing::warn!(conversation = %o.id, why = ?why, "the release found it changed: unfenced");
            Ok(false)
        }
        (Err(Abort::Failed(e)), None) => {
            let _ = fence::unfence(pool, o.id).await;
            Err(e)
        }
    }
}

/// What [`recover`] found.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recovered {
    /// Live hand-over fences re-stated and released.
    pub released: usize,
    /// Live hand-over fences lifted (a change, a failure).
    pub unfenced: usize,
    /// Lapsed fences cleared.
    pub lapsed: usize,
    /// Reverse fences of a reverse hand-over that ended without lifting them, lifted (the conversation stays owned).
    pub reverse_lifted: usize,
}

/// The fences a previous holder of the leader lock left (cutover 7.5: the leader killed between fence and release;
/// a reverse hand-over killed between its fence and its reopen), at the start of a pass: this process holds the lock,
/// so no other fence-taker runs. A live hand-over fence is re-stated and released, or lifted on any doubt
/// ([`finish`]); a lapsed one is cleared; a reverse fence is lifted (nothing was reopened: the conversation stays
/// owned, and the export's record, if it wrote one, is what `verify` checks).
///
/// # Errors
/// A statement error; the lock lost.
pub async fn recover(imp: &Importer, seen: &mut uploads::Seen) -> Result<Recovered> {
    let pool = &imp.magpie.pool;
    let mut out = Recovered::default();
    let hold: bool = pool
        .get()
        .await?
        .query_one("select hold from import_control", &[])
        .await?
        .get(0);
    let rows = pool
        .get()
        .await?
        .query(
            "select c.id, s.person, s.user_key, s.source_schema, s.last_error is not null,
                    c.legacy_fence_at > clock_timestamp() - make_interval(secs => $1)
             from conversations c join import_sources s on s.conversation_id = c.id
             where c.migrating and c.legacy_fence_at is not null and s.released_at is null",
            &[&magpie::limits::FENCE_LEASE.as_secs_f64()],
        )
        .await?;
    for r in &rows {
        let fenced = std::time::Instant::now();
        let o = OpenRow {
            id: r.get(0),
            person: r.get(1),
            key: r.get(2),
            schema: r.get(3),
            error: r.get(4),
        };
        if !r.get::<_, bool>(5) {
            if fence::lapse(pool, o.id).await? {
                out.lapsed += 1;
            }
            continue;
        }
        tracing::info!(conversation = %o.id, "a fence the last leader left: re-stat");
        if o.error || hold {
            fence::unfence(pool, o.id).await?;
            out.unfenced += 1;
            continue;
        }
        match finish(imp, &o, seen, fenced).await {
            Ok(true) => out.released += 1,
            Ok(false) => out.unfenced += 1,
            Err(e @ Error::LockLost) => return Err(e),
            Err(e) => {
                tracing::warn!(conversation = %o.id, error = %e, "a left fence was not released");
                out.unfenced += 1;
            }
        }
    }
    let owned: Vec<Uuid> = pool
        .get()
        .await?
        .query(
            "select id from conversations where not migrating and legacy_fence_at is not null",
            &[],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    for id in owned {
        if fence::unfence_reverse(pool, id).await? {
            tracing::info!(conversation = %id, "a reverse fence the last holder left: lifted");
            out.reverse_lifted += 1;
        }
    }
    Ok(out)
}

/// `allow-handover [--person <email>]`: sets the gate when every ui report is fresh and shows nothing but LU
/// (cutover 7.9): for everyone, or, with `person`, for that person only, judged on their part of every report
/// ([`gate::judge`]'s person mode; stored in `import_gate_persons`). The check, and whether the gate is set now.
///
/// # Errors
/// A statement error.
pub async fn allow(imp: &Importer, kube: &Kube, ui_token: &str, person: Option<&str>) -> Result<(gate::Check, bool)> {
    let c = gate::check(kube, ui_token, person).await;
    if !c.clean() {
        return Ok((c, false));
    }
    let conn = imp.magpie.pool.get().await?;
    match person {
        None => {
            conn.execute(
                "update import_control set handover_allowed_at = coalesce(handover_allowed_at, now())",
                &[],
            )
            .await?;
        }
        Some(p) => {
            conn.execute(
                "insert into import_gate_persons (person) values ($1) on conflict (person) do nothing",
                &[&p],
            )
            .await?;
        }
    }
    Ok((c, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::holds::{Child, RunningSet};

    fn read(at: OffsetDateTime, children: Vec<Child>) -> Read {
        Read {
            started: at,
            set: RunningSet::of(vec![Ok(children)]),
        }
    }

    fn child(ok: bool, sessions: &[&str]) -> Child {
        Child {
            schema: "cc_a".into(),
            port: 3001,
            ok,
            sessions: sessions.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn facts() -> Facts {
        Facts {
            schema: "cc_a".into(),
            verified: true,
            open_tickets: 0,
            last_close: None,
            last_activity: None,
            old_sandbox: false,
        }
    }

    #[test]
    fn released_only_when_every_condition_holds() {
        let t = OffsetDateTime::now_utc();
        let s = time::Duration::seconds;
        let idle = [
            read(t - s(10), vec![child(true, &["y"])]),
            read(t - s(5), vec![child(true, &["y"])]),
        ];
        assert_eq!(decide("x", &facts(), true, false, &idle, t), Ok(()));
        assert_eq!(decide("x", &facts(), false, false, &idle, t), Err(Wait::Gate));
        assert_eq!(decide("x", &facts(), true, true, &idle, t), Err(Wait::Hold));
        assert_eq!(
            decide(
                "x",
                &Facts {
                    verified: false,
                    ..facts()
                },
                true,
                false,
                &idle,
                t
            ),
            Err(Wait::Verify)
        );
        assert_eq!(
            decide(
                "x",
                &Facts {
                    old_sandbox: true,
                    ..facts()
                },
                true,
                false,
                &idle,
                t
            ),
            Err(Wait::OldSandbox)
        );
        // a sibling of the same person runs: x still goes; y never does
        assert_eq!(decide("y", &facts(), true, false, &idle, t), Err(Wait::Running));
        // one read only
        assert_eq!(decide("x", &facts(), true, false, &idle[1..], t), Err(Wait::Running));
        // a child that did not answer holds the whole schema
        let silent = [
            read(t - s(10), vec![child(false, &[])]),
            read(t - s(5), vec![child(true, &[])]),
        ];
        assert_eq!(
            decide("x", &facts(), true, false, &silent, t),
            Err(Wait::HoldsUnanswered)
        );
        // a ticket closed after the first read started: two reads after it are needed
        let closed = Facts {
            last_close: Some(t - s(7)),
            last_activity: Some(t - s(70)),
            ..facts()
        };
        assert_eq!(decide("x", &closed, true, false, &idle, t), Err(Wait::Running));
        // open ticket; then quiet
        assert_eq!(
            decide(
                "x",
                &Facts {
                    open_tickets: 1,
                    ..facts()
                },
                true,
                false,
                &idle,
                t
            ),
            Err(Wait::Ticket)
        );
        let recent = Facts {
            last_close: Some(t - s(20)),
            last_activity: Some(t - s(20)),
            ..facts()
        };
        let after = [read(t - s(10), vec![]), read(t - s(5), vec![])];
        assert_eq!(decide("x", &recent, true, false, &after, t), Err(Wait::Quiet));
        let quiet = Facts {
            last_close: Some(t - s(61)),
            last_activity: Some(t - s(61)),
            ..facts()
        };
        assert_eq!(decide("x", &quiet, true, false, &after, t), Ok(()));
        // an incomplete read
        let spoiled = [
            read(t - s(10), vec![]),
            Read {
                started: t - s(5),
                set: RunningSet::of(vec![Err(Error::Source("exec".into()))]),
            },
        ];
        assert_eq!(
            decide("x", &facts(), true, false, &spoiled, t),
            Err(Wait::HoldsUnanswered)
        );
    }
}
