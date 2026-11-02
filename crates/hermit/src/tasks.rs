//! Row claims and the takeover sweep (hermit 10.3): every long task (start, migration, release) holds a renewable
//! claim on its row (`task`, `task_owner`, `task_until`), so no work depends on the memory of the replica that began
//! it. A claim lapses 15 s after its last renewal; every replica's sweep then takes one lapsed row every 2 s and
//! resumes its task from the row.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// A claim this replica holds, renewed in the background while it lives.
#[derive(Debug)]
pub struct Claim {
    pub id: Uuid,
    pub kind: Kind,
    pub owner: String,
    lost: Arc<AtomicBool>,
    renewer: AbortHandle,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.renewer.abort();
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
        Self {
            id,
            kind,
            owner: core.pod.clone(),
            lost,
            renewer,
        }
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

    /// Stops every task (drain, after the claims were handed back; or a kill).
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

/// Drain step 3: stops this replica's tasks, then hands back every claim it holds (`task_until = now()`, owner
/// cleared so a late write of a stopped task finds nothing); another replica's sweep resumes them. Retried a few
/// times: a claim not handed back lapses on its own 15 s after its last renewal.
///
/// # Errors
/// The last database error.
pub async fn hand_back(core: &Core) -> Result<u64, aviary_pg::Error> {
    // a sweep pass in flight finishes first; the next ones see the drain
    let _pass = core.sweep_lock.lock().await;
    core.tasks.abort_all();
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
