//! Row claims and the takeover sweep (hermit 10.3): every long task (start, migration, release) holds a renewable
//! claim on its row (`task`, `task_owner`, `task_until`), so no work depends on the memory of the replica that began
//! it. A claim lapses 15 s after its last renewal; every replica's sweep then takes one lapsed row every 2 s and
//! resumes its task from the row.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use aviary_pg::Tx;
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::{Core, ms};

/// The task a claim is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Start,
    Migrate,
    Release,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Migrate => "migrate",
            Self::Release => "release",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "migrate" => Some(Self::Migrate),
            "release" => Some(Self::Release),
            _ => None,
        }
    }
}

/// The live claims of this replica and the gate of their side effects, so the drain's hand-back reaches every
/// running task at once (20.2 step 5: results are written only while this replica holds the claim).
///
/// A task makes a side effect that no single guarded write covers (a sandboxd `DELETE`, a failed start's record)
/// only under [`Claim::guard`]: the gate held for reading, the claim checked in memory and renewed by a guarded
/// write. [`hand_back`] marks every claim lost first, then takes the gate for writing, so it waits for an effect
/// already under way (whose claim was still this replica's) and no effect starts after it.
#[derive(Debug, Default)]
pub struct Claims {
    live: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    next: AtomicU64,
    effects: tokio::sync::RwLock<()>,
}

impl Claims {
    fn register(&self, lost: Arc<AtomicBool>) -> u64 {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        self.live.lock().unwrap_or_else(PoisonError::into_inner).insert(n, lost);
        n
    }

    fn forget(&self, n: u64) {
        self.live.lock().unwrap_or_else(PoisonError::into_inner).remove(&n);
    }

    /// Marks every live claim lost: each task stops at its next step.
    pub fn lose_all(&self) {
        for l in self.live.lock().unwrap_or_else(PoisonError::into_inner).values() {
            l.store(true, Ordering::Release);
        }
    }

    /// How many claims this replica holds in memory.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A claim this replica holds, renewed in the background while it lives.
#[derive(Debug)]
pub struct Claim {
    pub id: Uuid,
    pub kind: Kind,
    pub owner: String,
    lost: Arc<AtomicBool>,
    renewer: AbortHandle,
    claims: Arc<Claims>,
    serial: u64,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.renewer.abort();
        self.claims.forget(self.serial);
    }
}

impl Claim {
    /// Starts renewing a claim the database already records for this replica (after an insert, a take or the
    /// sweep).
    #[must_use]
    pub fn adopt(core: &Arc<Core>, id: Uuid, kind: Kind) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        let (c, l) = (core.clone(), lost.clone());
        let renewer = tokio::spawn(async move {
            loop {
                tokio::time::sleep(c.timings.renew).await;
                let renewed = async {
                    c.pool
                        .get()
                        .await?
                        .execute(
                            "update sandboxes set task_until = now() + $4::bigint * interval '1 millisecond'
                             where id = $1 and task = $2 and task_owner = $3",
                            &[&id, &kind.as_str(), &c.pod, &c.claim_ms()],
                        )
                        .await
                }
                .await;
                match renewed {
                    Ok(0) => {
                        tracing::info!(sandbox = %id, task = kind.as_str(), "claim lost");
                        l.store(true, Ordering::Release);
                        break;
                    }
                    Ok(_) => {}
                    // a database blip: try again at the next renewal (the claim may lapse meanwhile)
                    Err(e) => tracing::warn!(sandbox = %id, error = %e, "claim renewal failed"),
                }
            }
        })
        .abort_handle();
        let serial = core.claims.register(lost.clone());
        Self {
            id,
            kind,
            owner: core.pod.clone(),
            lost,
            renewer,
            claims: core.claims.clone(),
            serial,
        }
    }

    /// The gate for a side effect under this claim (a sandboxd call that ends or replaces a sandbox, a record no
    /// guarded write covers): `None` when the claim is not this replica's any more, else a guard to hold for the
    /// whole effect and its guarded write. The claim is renewed by a guarded write first, so its lease covers the
    /// effect, and [`hand_back`] waits for the guard.
    ///
    /// # Errors
    /// A database error (the effect is not made).
    pub async fn guard<'a>(
        &self,
        core: &'a Core,
    ) -> Result<Option<tokio::sync::RwLockReadGuard<'a, ()>>, aviary_pg::Error> {
        let g = core.claims.effects.read().await;
        if !self.held() {
            return Ok(None);
        }
        let n = core
            .pool
            .get()
            .await?
            .execute(
                "update sandboxes set task_until = now() + $4::bigint * interval '1 millisecond'
                 where id = $1 and task = $2 and task_owner = $3",
                &[&self.id, &self.kind.as_str(), &self.owner, &core.claim_ms()],
            )
            .await?;
        if n == 0 {
            self.mark_lost();
            return Ok(None);
        }
        Ok(Some(g))
    }

    /// `false` once a renewal found the claim taken or cleared: the task stops at its next step.
    #[must_use]
    pub fn held(&self) -> bool {
        !self.lost.load(Ordering::Acquire)
    }

    /// Marks the claim lost (a guarded write found 0 rows).
    pub fn mark_lost(&self) {
        self.lost.store(true, Ordering::Release);
    }
}

/// Takes the claim `kind` on a row that has no live claim: one statement (10.3).
///
/// # Errors
/// A database error.
pub async fn take(core: &Arc<Core>, id: Uuid, kind: Kind) -> Result<Option<Claim>, aviary_pg::Error> {
    if core.is_draining() {
        return Ok(None);
    }
    let n = core
        .pool
        .get()
        .await?
        .execute(
            "update sandboxes set task = $2, task_owner = $3, task_until = now() + $4::bigint * interval '1 millisecond'
             where id = $1 and status <> 'gone' and (task is null or task_until < now())",
            &[&id, &kind.as_str(), &core.pod, &core.claim_ms()],
        )
        .await?;
    Ok((n == 1).then(|| Claim::adopt(core, id, kind)))
}

/// Takes the claim inside a transaction (the caller adopts it after the commit).
///
/// # Errors
/// A database error.
pub async fn take_in(t: &Tx<'_>, core: &Core, id: Uuid, kind: Kind) -> Result<bool, aviary_pg::Error> {
    if core.is_draining() {
        return Ok(false);
    }
    let n = t
        .execute(
            "update sandboxes set task = $2, task_owner = $3, task_until = now() + $4::bigint * interval '1 millisecond'
             where id = $1 and status <> 'gone' and (task is null or task_until < now())",
            &[&id, &kind.as_str(), &core.pod, &core.claim_ms()],
        )
        .await?;
    Ok(n == 1)
}

/// Clears the claim inside a transaction; 0 rows means it was not ours any more.
///
/// # Errors
/// A database error.
pub async fn clear_in(t: &Tx<'_>, claim: &Claim) -> Result<u64, aviary_pg::Error> {
    t.execute(
        "update sandboxes set task = null, task_owner = null, task_until = null
         where id = $1 and task = $2 and task_owner = $3",
        &[&claim.id, &claim.kind.as_str(), &claim.owner],
    )
    .await
}

/// Clears the claim outside a transaction.
///
/// # Errors
/// A database error.
pub async fn clear(core: &Core, claim: &Claim) -> Result<u64, aviary_pg::Error> {
    core.pool
        .get()
        .await?
        .execute(
            "update sandboxes set task = null, task_owner = null, task_until = null
             where id = $1 and task = $2 and task_owner = $3",
            &[&claim.id, &claim.kind.as_str(), &claim.owner],
        )
        .await
}

/// The tasks running in this replica, so a drain or a kill can stop them.
#[derive(Debug, Default)]
pub struct TaskSet {
    running: Mutex<HashMap<(Uuid, Kind), AbortHandle>>,
}

impl TaskSet {
    /// Spawns `fut` as the task of `(id, kind)` unless one already runs here.
    pub fn spawn<F>(&self, id: Uuid, kind: Kind, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        running.retain(|_, h| !h.is_finished());
        if running.contains_key(&(id, kind)) {
            return;
        }
        let h = tokio::spawn(fut).abort_handle();
        running.insert((id, kind), h);
    }

    /// Whether a task of `(id, kind)` runs here.
    #[must_use]
    pub fn running(&self, id: Uuid, kind: Kind) -> bool {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(id, kind))
            .is_some_and(|h| !h.is_finished())
    }

    /// Waits up to `max` for every task to end by itself (drain step 5: the supervisor and sandboxd calls already
    /// sent finish; a task whose claim was handed back writes nothing and stops at its next guarded write or
    /// renewal), then stops the rest. Returns how many were stopped.
    pub async fn settle(&self, max: std::time::Duration) -> usize {
        let end = tokio::time::Instant::now() + max;
        while !self.is_empty() && tokio::time::Instant::now() < end {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let left = self.len();
        self.abort_all();
        left
    }

    /// Stops every task (the end of a drain, or a kill).
    pub fn abort_all(&self) {
        for (_, h) in self.running.lock().unwrap_or_else(PoisonError::into_inner).drain() {
            h.abort();
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        running.retain(|_, h| !h.is_finished());
        running.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Runs the task of a claim this replica holds.
pub fn run(core: &Arc<Core>, claim: Claim) {
    let (id, kind) = (claim.id, claim.kind);
    let c = core.clone();
    match kind {
        Kind::Start => core.tasks.spawn(id, kind, crate::starts::run(c, claim)),
        Kind::Release => core.tasks.spawn(id, kind, crate::sandboxes::run_release(c, claim)),
        Kind::Migrate => core.tasks.spawn(id, kind, async move {
            // live migration is the fabric's task (hermit 13.10, WP-19): until it exists, a migrate claim is
            // cleared so the row is judged again by its state
            tracing::warn!(sandbox = %claim.id, "migrate task without a fabric: claim cleared");
            let _ = clear(&c, &claim).await;
        }),
    }
}

/// One pass of the takeover sweep: claims one row whose claim lapsed and resumes its task.
///
/// # Errors
/// A database error.
pub async fn sweep_once(core: &Arc<Core>) -> Result<Option<(Uuid, Kind)>, aviary_pg::Error> {
    let _pass = core.sweep_lock.lock().await;
    if core.is_draining() {
        return Ok(None);
    }
    let row = core
        .pool
        .get()
        .await?
        .query_opt(
            "update sandboxes set task_owner = $1, task_until = now() + $2::bigint * interval '1 millisecond'
             where id = (select id from sandboxes where task is not null and task_until < now() and status <> 'gone'
                         order by task_until limit 1 for update skip locked)
             returning id, task",
            &[&core.pod, &core.claim_ms()],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id: Uuid = row.try_get(0)?;
    let task: String = row.try_get(1)?;
    let Some(kind) = Kind::parse(&task) else {
        return Ok(None);
    };
    tracing::info!(sandbox = %id, task = kind.as_str(), "task taken over");
    run(core, Claim::adopt(core, id, kind));
    Ok(Some((id, kind)))
}

/// Drain step 3: hands back every claim this replica holds (`task_until = now()`, owner cleared so a later write of
/// one of its tasks finds nothing); another replica's sweep resumes them. The tasks are not stopped here: a call
/// they already sent finishes (step 5, [`TaskSet::settle`]) and its result is dropped, and the new owner repeats
/// the idempotent step. Every live claim is marked lost first, so no task starts a side effect after this, and an
/// effect already under [`Claim::guard`] finishes before the claims move. Retried a few times: a claim not handed
/// back lapses on its own 15 s after its last renewal.
///
/// # Errors
/// The last database error.
pub async fn hand_back(core: &Core) -> Result<u64, aviary_pg::Error> {
    // a sweep pass in flight finishes first; the next ones see the drain
    let _pass = core.sweep_lock.lock().await;
    core.claims.lose_all();
    let _effects = core.claims.effects.write().await;
    let mut last = None;
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let res = async {
            core.pool
                .get()
                .await?
                .execute(
                    "update sandboxes set task_owner = null, task_until = now() where task_owner = $1",
                    &[&core.pod],
                )
                .await
        }
        .await;
        match res {
            Ok(n) => return Ok(n),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or(aviary_pg::Error::Closed))
}

/// `$n * interval '1 millisecond'` argument of a duration.
#[must_use]
pub fn millis(d: std::time::Duration) -> i64 {
    ms(d)
}
