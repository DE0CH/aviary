//! The in-memory map of every non-gone sandbox (hermit 3.9, 10.4): token liveness and the sandbox surface's
//! identity check are answered from it without a database connection, so a Postgres blip or a busy pool never slows
//! them. It is kept current by the replica's own commits, the `hermit_sandbox` NOTIFY and a full reload every 30 s;
//! a sandbox that went gone stays in a small recent-gone set for 10 minutes.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{PoisonError, RwLock};
use std::time::{Duration, Instant};

use aviary_pg::Pool;
use hermit_api::Timestamp;
use hermit_api::sandbox::SandboxState;
use uuid::Uuid;

use crate::db::SandboxRow;

/// How long a gone sandbox is remembered.
pub const RECENT_GONE: Duration = Duration::from_secs(600);

/// What the map keeps of a row.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub person: String,
    pub state: SandboxState,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub hard_expires_at: Timestamp,
    pub image: Option<String>,
    pub token_kid: Option<i16>,
    pub ip: Option<IpAddr>,
    pub version: i32,
}

impl Entry {
    #[must_use]
    pub fn of(r: &SandboxRow) -> Self {
        Self {
            id: r.id,
            conversation_id: r.conversation_id,
            person: r.person.clone(),
            state: r.status,
            created_at: r.created_at.into(),
            expires_at: r.expires_at.into(),
            hard_expires_at: r.hard_expires_at.into(),
            image: r.image.clone(),
            token_kid: r.token_kid,
            ip: r.ip,
            version: r.version,
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    live: HashMap<Uuid, Entry>,
    gone: HashMap<Uuid, (Entry, Instant)>,
}

/// The map of one replica.
#[derive(Debug, Default)]
pub struct LiveMap {
    inner: RwLock<Inner>,
    loaded: AtomicBool,
}

impl LiveMap {
    /// Whether a full load has happened (readiness, 20.1).
    #[must_use]
    pub fn loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    /// The entry of a sandbox id: live, or recently gone.
    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<Entry> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .live
            .get(&id)
            .cloned()
            .or_else(|| inner.gone.get(&id).map(|(e, _)| e.clone()))
    }

    /// Every live entry.
    #[must_use]
    pub fn live(&self) -> Vec<Entry> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .values()
            .cloned()
            .collect()
    }

    /// Applies one row (after this replica committed a change to it), unless the map already holds a newer version.
    pub fn apply(&self, r: &SandboxRow) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        Self::apply_locked(&mut inner, Entry::of(r));
    }

    fn apply_locked(inner: &mut Inner, e: Entry) {
        if let Some(cur) = inner.live.get(&e.id)
            && cur.version > e.version
        {
            return;
        }
        if e.state == SandboxState::Gone {
            inner.live.remove(&e.id);
            inner.gone.insert(e.id, (e, Instant::now()));
        } else if !inner.gone.contains_key(&e.id) {
            inner.live.insert(e.id, e);
        }
    }

    /// Reloads every non-gone row and the rows gone in the last 10 minutes.
    ///
    /// # Errors
    /// A database error (the map keeps what it had).
    pub async fn reload(&self, pool: &Pool) -> Result<(), aviary_pg::Error> {
        let c = pool.get().await?;
        let rows = c
            .query(
                "select * from sandboxes where status <> 'gone' or released_at > now() - interval '10 minutes'",
                &[],
            )
            .await?;
        let mut live = HashMap::new();
        let mut gone = Vec::new();
        for r in &rows {
            let row = SandboxRow::from_row(r)?;
            let e = Entry::of(&row);
            if e.state == SandboxState::Gone {
                gone.push(e);
            } else {
                live.insert(e.id, e);
            }
        }
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        // a row this replica inserted after the snapshot was read is not in it yet: keep it for a minute
        let fresh: Vec<Entry> = inner
            .live
            .values()
            .filter(|e| {
                !live.contains_key(&e.id) && Timestamp::now().unix_millis() - e.created_at.unix_millis() < 60_000
            })
            .cloned()
            .collect();
        for e in fresh {
            live.insert(e.id, e);
        }
        for e in gone {
            inner.gone.entry(e.id).or_insert((e, now));
        }
        // a change this replica applied after the snapshot was read is newer than it
        for (id, e) in &mut live {
            if let Some(cur) = inner.live.get(id)
                && cur.version > e.version
            {
                *e = cur.clone();
            }
        }
        inner.live = live;
        inner.gone.retain(|_, (_, at)| at.elapsed() < RECENT_GONE);
        let gone_ids: Vec<Uuid> = inner.gone.keys().copied().collect();
        for id in gone_ids {
            inner.live.remove(&id);
        }
        drop(inner);
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }
}
