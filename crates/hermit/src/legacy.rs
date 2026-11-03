//! The switch's one-off commands (hermit 15.4, 22.3; cutover 8.1), deleted after it: `hermit import-jarvis`
//! ([`import_jarvis`]: the old stack's sandbox history and share workspaces), `hermit jfs-owner` ([`fix_owner`]:
//! everything under `ws/` to uid/gid 1000) and `hermit legacy-cleanup` ([`cleanup_plan`], [`cleanup_apply`]: the old
//! folders off the volume, 30 days after the switch). None of them serves; each runs once per run of its `Job` and
//! exits.
//!
//! **import-jarvis** reads the old `jarvis` database (a read-only role) and writes hermit's own:
//! - `sandboxes` rows with an owner, `status = 'gone'`, released in the last [`SANDBOX_DAYS`] days, become `sandboxes`
//!   rows with `imported = true` (same id, `person` = the lowercased owner, `name` = the old pod name, `host`, `ip`,
//!   the timestamps and `release_reason` as they are, `release_code` from [`old_release_code`], `token_kid` null).
//!   The owner-null rows of the old UI's sandboxes are not read (A13.F11).
//! - `ops_events` of kind `sandbox_start` of the last [`STARTS_DAYS`] days become `sandbox_starts` (`source_id` = the
//!   old id, `person` the started sandbox's owner, `error` redacted and clipped to 300 characters). `view_as` is
//!   magpie's import; `budget` rows are dropped.
//! - every `vm_events` row becomes a `host_events` row (`source_id`), kinds kept.
//! - `vm_hosts` with `status = 'gone'` become `hosts` (kind `hub` → `gateway`); the live old hosts stay `web`'s.
//! - every old share's frozen folder `users/<key>/shares/<id>/ws` is cloned (`juicefs clone`, then the quota) into
//!   `ws/<uuidv5(SHARE_NS, "share:" + id)>`, the id magpie's import gives the share; a target already there is skipped.
//!
//! Every `at` keeps its source value. Each copy is idempotent (sandboxes and hosts by their own key, the event tables
//! by the unique `source_id`, shares by the target folder), so a run that is cut short or repeated changes nothing it
//! already did. `--delta` narrows a run to what can be new since the last: event rows above the highest `source_id`
//! copied less [`ID_LOOKBACK`] (a row whose id was handed out before a neighbour's but committed after it is still
//! read), sandboxes released after the latest imported release less [`RELEASE_LOOKBACK`]; hosts (a few dozen) and the
//! share folders are always listed whole.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use aviary_pg::Pool;
use aviary_pg::reexports::tokio_postgres::types::FromSqlOwned;
use hermit_api::workspaces::SHARE_NS;
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::fabric::Fut;
use crate::workspaces::{JfsCli, JuiceFs};

/// Old sandboxes released in the last this many days are imported (hermit's own retention keeps 400 days, 10.6).
pub const SANDBOX_DAYS: i32 = 400;
/// Old start events of the last this many days are imported.
pub const STARTS_DAYS: i32 = 30;
/// `--delta` re-reads this many event ids below the highest one copied.
pub const ID_LOOKBACK: i64 = 1000;
/// `--delta` re-reads sandboxes released up to this long before the latest imported release.
pub const RELEASE_LOOKBACK: Duration = Duration::from_hours(1);
/// The old fabric gave every sandbox one CPU and 1 GiB (its `sandbox.cpu` and `sandbox.memory`); the old rows do not
/// record it, and hermit's rows require it.
pub const OLD_CPU: f32 = 1.0;
pub const OLD_MEMORY_BYTES: i64 = 1 << 30;
/// The sandbox user (15.4).
pub const SANDBOX_UID: u32 = 1000;

/// Why a command stopped.
#[derive(Debug, thiserror::Error)]
pub enum LegacyError {
    #[error("database: {0}")]
    Db(#[from] aviary_pg::Error),
    #[error("{0}")]
    Other(String),
}

/// Rows read and rows written of one copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Count {
    pub read: u64,
    pub copied: u64,
}

/// What one share pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ShareCount {
    /// Share folders found (`users/<key>/shares/<id>/ws`).
    pub found: u64,
    pub cloned: u64,
    /// The target was already there.
    pub skipped: u64,
    /// Folders whose clone failed (the next run retries them), as `<id>: <error>`.
    pub failed: Vec<String>,
}

/// What one `import-jarvis` run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub delta: bool,
    pub sandboxes: Count,
    pub sandbox_starts: Count,
    pub host_events: Count,
    pub hosts: Count,
    /// `None` when the run had no workspace store (`--no-shares`).
    pub shares: Option<ShareCount>,
}

/// The release code of an old row's `release_reason` (13.13's codes for the old strings): the old start failures
/// for lack of room are `sandbox_busy`, other start failures `sandbox_failed`, the deaths `sandbox_lost`, `stuck`
/// none (13.13 keeps a stuck row's code), everything else (idle, pool, deleted, the operator's and the tests'
/// reasons) `sandbox_released`.
#[must_use]
pub fn old_release_code(reason: Option<&str>) -> Option<&'static str> {
    let r = reason?.trim();
    if let Some(why) = r.strip_prefix("start failed") {
        let busy = [
            "no sandbox host became available",
            "all sandbox hosts are full",
            "exceeded quota",
        ];
        return Some(if busy.iter().any(|b| why.contains(b)) {
            "sandbox_busy"
        } else {
            "sandbox_failed"
        });
    }
    match r {
        "stuck" => None,
        "unhealthy" | "host lost" | "host_lost" | "sandbox vanished" | "vanished" | "migration failed"
        | "migration_failed" => Some("sandbox_lost"),
        _ if r.starts_with("lost") => Some("sandbox_lost"),
        _ => Some("sandbox_released"),
    }
}

/// An old text address as an `inet` value; anything that does not parse is null.
fn inet(s: Option<String>) -> Option<IpAddr> {
    s.and_then(|v| v.trim().parse().ok())
}

/// The workspace id of an old share (magpie 12's convention; the share id is the old folder name).
#[must_use]
pub fn share_workspace(share_id: &str) -> Uuid {
    Uuid::new_v5(&SHARE_NS, format!("share:{share_id}").as_bytes())
}

/// A folder name the import accepts as an old user key or share id (base64url and hex only: nothing that could
/// name another path).
fn plain_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Every old share folder under `root` (the mount): `(share id, path within the volume)`, sorted.
///
/// # Errors
/// `users/` exists but cannot be read.
pub fn old_shares(root: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let users = root.join("users");
    let mut out = Vec::new();
    let keys = match std::fs::read_dir(&users) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for key in keys {
        let key = key?;
        let Some(k) = key.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !plain_name(&k) {
            continue;
        }
        let Ok(shares) = std::fs::read_dir(key.path().join("shares")) else {
            continue;
        };
        for share in shares {
            let share = share?;
            let Some(id) = share.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !plain_name(&id) {
                continue;
            }
            let rel = PathBuf::from("users").join(&k).join("shares").join(&id).join("ws");
            if std::fs::symlink_metadata(root.join(&rel)).is_ok_and(|m| m.is_dir()) {
                out.push((id, rel));
            }
        }
    }
    out.sort();
    Ok(out)
}

async fn max_source_id(hermit: &aviary_pg::Conn, table: &str) -> Result<i64, LegacyError> {
    let r = hermit
        .query_one(&format!("select coalesce(max(source_id), 0) as m from {table}"), &[])
        .await?;
    get(&r, "m")
}

fn get<T: FromSqlOwned>(r: &aviary_pg::Row, c: &str) -> Result<T, LegacyError> {
    r.try_get::<_, T>(c).map_err(|e| LegacyError::Db(e.into()))
}

/// One `import-jarvis` run: `source` is the old database, `hermit` hermit's own, `store` the workspace store for the
/// share clones (`None` skips them). Copies in the order of the module header; a failed share clone is reported and
/// does not stop the run.
///
/// # Errors
/// A database failure (nothing after it is copied; the next run picks up where this one stopped).
pub async fn import_jarvis(
    hermit: &Pool,
    source: &Pool,
    store: Option<&JuiceFs>,
    root: &Path,
    delta: bool,
) -> Result<ImportReport, LegacyError> {
    let mut report = ImportReport {
        delta,
        ..ImportReport::default()
    };
    report.sandboxes = import_sandboxes(hermit, source, delta).await?;
    tracing::info!(
        table = "sandboxes",
        read = report.sandboxes.read,
        copied = report.sandboxes.copied,
        delta,
        "import-jarvis"
    );
    report.sandbox_starts = import_starts(hermit, source, delta).await?;
    tracing::info!(
        table = "sandbox_starts",
        read = report.sandbox_starts.read,
        copied = report.sandbox_starts.copied,
        delta,
        "import-jarvis"
    );
    report.host_events = import_host_events(hermit, source, delta).await?;
    tracing::info!(
        table = "host_events",
        read = report.host_events.read,
        copied = report.host_events.copied,
        delta,
        "import-jarvis"
    );
    report.hosts = import_hosts(hermit, source).await?;
    tracing::info!(
        table = "hosts",
        read = report.hosts.read,
        copied = report.hosts.copied,
        delta,
        "import-jarvis"
    );
    if let Some(store) = store {
        let s = import_shares(hermit, store, root).await?;
        tracing::info!(
            found = s.found,
            cloned = s.cloned,
            skipped = s.skipped,
            failed = s.failed.len(),
            "import-jarvis shares"
        );
        report.shares = Some(s);
    }
    Ok(report)
}

async fn import_sandboxes(hermit: &Pool, source: &Pool, delta: bool) -> Result<Count, LegacyError> {
    let (hermit, source) = (hermit.get().await?, source.get().await?);
    let since: Option<OffsetDateTime> = if delta {
        let r = hermit
            .query_one("select max(released_at) from sandboxes where imported", &[])
            .await?;
        get::<Option<OffsetDateTime>>(&r, "max")?.map(|t| t - RELEASE_LOOKBACK)
    } else {
        None
    };
    let rows = source
        .query(
            "select id, conversation_id, lower(owner) as person, pod, host, ip, created_at, last_active_at, expires_at,
                    released_at, release_reason
               from sandboxes
              where status = 'gone' and owner is not null
                and coalesce(released_at, last_active_at) > now() - make_interval(days => $1)
                and ($2::timestamptz is null or coalesce(released_at, last_active_at) > $2)
              order by coalesce(released_at, last_active_at), id",
            &[&SANDBOX_DAYS, &since],
        )
        .await?;
    let mut n = Count::default();
    for r in &rows {
        n.read += 1;
        let reason: Option<String> = get(r, "release_reason")?;
        let code = old_release_code(reason.as_deref());
        let created: OffsetDateTime = get(r, "created_at")?;
        let expires: OffsetDateTime = get(r, "expires_at")?;
        let ip = inet(get(r, "ip")?);
        n.copied += hermit
            .execute(
                "insert into sandboxes (id, conversation_id, person, status, name, token_kid, host, ip, cpu, memory_bytes,
                                        start_deadline, created_at, last_active_at, expires_at, hard_expires_at,
                                        released_at, release_reason, release_code, imported)
                 values ($1, $2, $3, 'gone', $4, null, $5, $6, $7, $8, $9, $9, $10, $11, $11, $12, $13, $14, true)
                 on conflict do nothing",
                &[
                    &get::<Uuid>(r, "id")?,
                    &get::<Uuid>(r, "conversation_id")?,
                    &get::<String>(r, "person")?,
                    &get::<String>(r, "pod")?,
                    &get::<Option<String>>(r, "host")?,
                    &ip,
                    &OLD_CPU,
                    &OLD_MEMORY_BYTES,
                    &created,
                    &get::<OffsetDateTime>(r, "last_active_at")?,
                    &expires,
                    &get::<Option<OffsetDateTime>>(r, "released_at")?,
                    &reason,
                    &code,
                ],
            )
            .await?;
    }
    Ok(n)
}

async fn import_starts(hermit: &Pool, source: &Pool, delta: bool) -> Result<Count, LegacyError> {
    let (hermit, source) = (hermit.get().await?, source.get().await?);
    let after = if delta {
        (max_source_id(&hermit, "sandbox_starts").await? - ID_LOOKBACK).max(0)
    } else {
        0
    };
    let rows = source
        .query(
            "select o.id, o.at, coalesce(o.ok, false) as ok, coalesce(o.ms, 0) as ms,
                    case when jsonb_typeof(o.detail->'startMs') = 'number'
                         then least((o.detail->>'startMs')::numeric, 2147483647)::int end as start_ms,
                    o.sandbox_id, o.conversation_id, lower(s.owner) as person, o.host,
                    o.detail->>'reason' as reason, o.detail->>'error' as error
               from ops_events o left join sandboxes s on s.id = o.sandbox_id
              where o.kind = 'sandbox_start' and o.at > now() - make_interval(days => $1) and o.id > $2
              order by o.id",
            &[&STARTS_DAYS, &after],
        )
        .await?;
    let mut n = Count::default();
    for r in &rows {
        n.read += 1;
        let error = get::<Option<String>>(r, "error")?.map(|e| crate::db::clip(&aviary_core::log::redact(&e), 300));
        n.copied += hermit
            .execute(
                "insert into sandbox_starts (at, ok, ms, start_ms, sandbox_id, conversation_id, person, host, reason,
                                             error, source_id)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) on conflict (source_id) do nothing",
                &[
                    &get::<OffsetDateTime>(r, "at")?,
                    &get::<bool>(r, "ok")?,
                    &get::<i32>(r, "ms")?,
                    &get::<Option<i32>>(r, "start_ms")?,
                    &get::<Option<Uuid>>(r, "sandbox_id")?,
                    &get::<Option<Uuid>>(r, "conversation_id")?,
                    &get::<Option<String>>(r, "person")?,
                    &get::<Option<String>>(r, "host")?,
                    &get::<Option<String>>(r, "reason")?,
                    &error,
                    &get::<i64>(r, "id")?,
                ],
            )
            .await?;
    }
    Ok(n)
}

async fn import_host_events(hermit: &Pool, source: &Pool, delta: bool) -> Result<Count, LegacyError> {
    let (hermit, source) = (hermit.get().await?, source.get().await?);
    let after = if delta {
        (max_source_id(&hermit, "host_events").await? - ID_LOOKBACK).max(0)
    } else {
        0
    };
    let rows = source
        .query(
            "select id, at, kind, host, sandbox_id, data from vm_events where id > $1 order by id",
            &[&after],
        )
        .await?;
    let mut n = Count::default();
    for r in &rows {
        n.read += 1;
        n.copied += hermit
            .execute(
                "insert into host_events (at, kind, host, sandbox_id, data, source_id) values ($1, $2, $3, $4, $5, $6)
                 on conflict (source_id) do nothing",
                &[
                    &get::<OffsetDateTime>(r, "at")?,
                    &get::<String>(r, "kind")?,
                    &get::<Option<String>>(r, "host")?,
                    &get::<Option<Uuid>>(r, "sandbox_id")?,
                    &get::<Value>(r, "data")?,
                    &get::<i64>(r, "id")?,
                ],
            )
            .await?;
    }
    Ok(n)
}

async fn import_hosts(hermit: &Pool, source: &Pool) -> Result<Count, LegacyError> {
    let (hermit, source) = (hermit.get().await?, source.get().await?);
    let rows = source
        .query(
            "select name, case when kind = 'hub' then 'gateway' else kind end as kind, url, wg_ip, wg_public_key,
                    instance_id, private_ip, max_sandboxes, mem_total, mem_reserve, cpus, info, created_at,
                    last_seen_at, empty_since, gone_at, gone_reason
               from vm_hosts where status = 'gone' order by name",
            &[],
        )
        .await?;
    let mut n = Count::default();
    for r in &rows {
        n.read += 1;
        let name: String = get(r, "name")?;
        let Some(wg_ip) = inet(get(r, "wg_ip")?) else {
            tracing::warn!(host = %name, "old host with an unreadable wg_ip not imported");
            continue;
        };
        let max: Option<i32> = get(r, "max_sandboxes")?;
        n.copied += hermit
            .execute(
                "insert into hosts (name, kind, url, wg_ip, wg_public_key, instance_id, private_ip, status, max_sandboxes,
                                    mem_total, mem_reserve, cpus, info, created_at, last_seen_at, empty_since, gone_at,
                                    gone_reason)
                 values ($1, $2, $3, $4, $5, $6, $7, 'gone', $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
                 on conflict (name) do nothing",
                &[
                    &name,
                    &get::<String>(r, "kind")?,
                    &get::<String>(r, "url")?,
                    &wg_ip,
                    &get::<Option<String>>(r, "wg_public_key")?,
                    &get::<Option<String>>(r, "instance_id")?,
                    &inet(get(r, "private_ip")?),
                    &max.map(|m| m.max(0)),
                    &get::<Option<i64>>(r, "mem_total")?,
                    &get::<Option<i64>>(r, "mem_reserve")?,
                    &get::<Option<i32>>(r, "cpus")?,
                    &get::<Option<Value>>(r, "info")?,
                    &get::<OffsetDateTime>(r, "created_at")?,
                    &get::<Option<OffsetDateTime>>(r, "last_seen_at")?,
                    &get::<Option<OffsetDateTime>>(r, "empty_since")?,
                    &get::<Option<OffsetDateTime>>(r, "gone_at")?,
                    &get::<Option<String>>(r, "gone_reason")?,
                ],
            )
            .await?;
    }
    Ok(n)
}

async fn import_shares(hermit: &Pool, store: &JuiceFs, root: &Path) -> Result<ShareCount, LegacyError> {
    let r = root.to_owned();
    let shares = tokio::task::spawn_blocking(move || old_shares(&r))
        .await
        .map_err(|e| LegacyError::Other(e.to_string()))?
        .map_err(|e| LegacyError::Other(format!("users/: {e}")))?;
    let mut n = ShareCount::default();
    for (id, rel) in shares {
        n.found += 1;
        let wid = share_workspace(&id);
        match store.import_clone(wid, &rel).await {
            Ok(Some(w)) => {
                crate::db::workspace_created(hermit, wid, w.created_at, true).await;
                tracing::info!(share = %id, workspace = %wid, "share workspace cloned");
                n.cloned += 1;
            }
            Ok(None) => n.skipped += 1,
            Err(e) => {
                tracing::warn!(share = %id, workspace = %wid, error = %e, "share workspace not cloned");
                n.failed.push(format!("{id}: {e}"));
            }
        }
    }
    Ok(n)
}

// ---- jfs-owner (15.4) ----

/// What one `jfs-owner` run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OwnerReport {
    /// Entries looked at (`ws/` itself included).
    pub scanned: u64,
    /// Entries whose owner was changed.
    pub changed: u64,
    /// Entries that could not be read or changed (at most the first 20, as `<path>: <error>`), and how many.
    pub errors: Vec<String>,
    pub error_count: u64,
}

struct Walk {
    dev: u64,
    uid: u32,
    gid: u32,
    scanned: AtomicU64,
    changed: AtomicU64,
    error_count: AtomicU64,
    errors: Mutex<Vec<String>>,
}

impl Walk {
    fn fail(&self, p: &Path, e: &std::io::Error) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
        let mut v = self.errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if v.len() < 20 {
            v.push(format!("{}: {e}", p.display()));
        }
    }

    /// Owns `p` (never following a symlink) when it is not owned already; returns its metadata.
    fn own(&self, p: &Path) -> Option<std::fs::Metadata> {
        self.scanned.fetch_add(1, Ordering::Relaxed);
        let m = match std::fs::symlink_metadata(p) {
            Ok(m) => m,
            Err(e) => {
                self.fail(p, &e);
                return None;
            }
        };
        if m.uid() != self.uid || m.gid() != self.gid {
            match std::os::unix::fs::lchown(p, Some(self.uid), Some(self.gid)) {
                Ok(()) => {
                    self.changed.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => self.fail(p, &e),
            }
        }
        Some(m)
    }

    /// The tree under `p`, depth first, on one filesystem.
    fn tree(&self, p: &Path) {
        let Some(m) = self.own(p) else { return };
        if !m.is_dir() || m.dev() != self.dev {
            return;
        }
        let mut stack = vec![p.to_owned()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    self.fail(&dir, &e);
                    continue;
                }
            };
            for e in entries {
                let e = match e {
                    Ok(e) => e,
                    Err(err) => {
                        self.fail(&dir, &err);
                        continue;
                    }
                };
                let path = e.path();
                if let Some(m) = self.own(&path)
                    && m.is_dir()
                    && m.dev() == self.dev
                {
                    stack.push(path);
                }
            }
        }
    }
}

/// `chown 1000:1000 <root>/ws && chown -R 1000:1000 <root>/ws/*` (15.4), done as one walk that never follows a
/// symlink (`lchown`), stays on the volume, changes only entries not owned already, and runs the top-level folders
/// on `threads` threads. Needs root and a mount without squashing (Job `hermit-jfs-owner`).
///
/// # Errors
/// `<root>/ws` cannot be read.
pub fn fix_owner(root: &Path, uid: u32, gid: u32, threads: usize) -> std::io::Result<OwnerReport> {
    let ws = root.join("ws");
    let dev = std::fs::symlink_metadata(&ws)?.dev();
    let walk = Walk {
        dev,
        uid,
        gid,
        scanned: AtomicU64::new(0),
        changed: AtomicU64::new(0),
        error_count: AtomicU64::new(0),
        errors: Mutex::new(Vec::new()),
    };
    walk.own(&ws);
    let tops: Vec<PathBuf> = std::fs::read_dir(&ws)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = tops.get(i) else { break };
                    walk.tree(p);
                }
            });
        }
    });
    Ok(OwnerReport {
        scanned: walk.scanned.into_inner(),
        changed: walk.changed.into_inner(),
        errors: walk
            .errors
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        error_count: walk.error_count.into_inner(),
    })
}

// ---- legacy-cleanup (22.3 step 6) ----

/// Which workspace ids magpie still references (`POST /v1/hermit/workspaces/exists`, magpie 9.7).
pub trait Referenced: Send + Sync {
    /// The ones among `ids` (at most [`EXISTS_BATCH`]) magpie names.
    fn exists<'a>(&'a self, ids: &'a [Uuid]) -> Fut<'a, Result<Vec<Uuid>, String>>;
}

/// The ids of one `workspaces/exists` call.
pub const EXISTS_BATCH: usize = 1000;

impl Referenced for magpie_api::client::Client {
    fn exists<'a>(&'a self, ids: &'a [Uuid]) -> Fut<'a, Result<Vec<Uuid>, String>> {
        Box::pin(async move {
            let req = magpie_api::ops::WorkspacesExistsRequest { ids: ids.to_vec() };
            self.service(None)
                .workspaces_exists(&req)
                .await
                .map(|r| r.exists)
                .map_err(|e| format!("magpie workspaces/exists: {e}"))
        })
    }
}

/// What `legacy-cleanup` removes, and what it keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPlan {
    /// `users` and `sent` (those present), relative to the mount: magpie's import holds their contents.
    pub trees: Vec<String>,
    /// Empty `ws/<id>` folders that magpie does not reference and no live sandbox uses.
    pub empty_workspaces: Vec<Uuid>,
    /// Empty folders kept because magpie references them.
    pub kept_referenced: u64,
    /// Empty folders kept because a sandbox that is not gone has that workspace.
    pub kept_live: u64,
    /// Workspace folders with content (never removed here).
    pub kept_with_content: u64,
}

/// The `ws/<uuid>` folders under `root` and whether each is empty (other names, such as a clone's temporary tree,
/// are not workspaces and are left alone).
fn workspace_folders(root: &Path) -> std::io::Result<Vec<(Uuid, bool)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root.join("ws"))? {
        let e = e?;
        let Some(id) = e.file_name().to_str().and_then(|n| Uuid::try_parse(n).ok()) else {
            continue;
        };
        if id.to_string() != e.file_name().to_string_lossy() {
            continue;
        }
        let m = std::fs::symlink_metadata(e.path())?;
        if !m.is_dir() {
            continue;
        }
        let empty = std::fs::read_dir(e.path())?.next().is_none();
        out.push((id, empty));
    }
    out.sort();
    Ok(out)
}

/// The plan of one `legacy-cleanup`: reads the volume, hermit's live sandboxes and asks magpie about every empty
/// workspace folder. Removes nothing.
///
/// # Errors
/// The volume or the database cannot be read, or magpie does not answer: then nothing is to be removed.
pub async fn cleanup_plan(root: &Path, hermit: &Pool, magpie: &dyn Referenced) -> Result<CleanupPlan, LegacyError> {
    let r = root.to_owned();
    let (folders, trees) = tokio::task::spawn_blocking(move || {
        let trees: Vec<String> = ["users", "sent"]
            .into_iter()
            .filter(|t| std::fs::symlink_metadata(r.join(t)).is_ok_and(|m| m.is_dir()))
            .map(str::to_owned)
            .collect();
        workspace_folders(&r).map(|f| (f, trees))
    })
    .await
    .map_err(|e| LegacyError::Other(e.to_string()))?
    .map_err(|e| LegacyError::Other(format!("ws/: {e}")))?;
    let live: BTreeSet<Uuid> = hermit
        .get()
        .await?
        .query("select conversation_id from sandboxes where status <> 'gone'", &[])
        .await?
        .iter()
        .map(|r| r.try_get::<_, Uuid>(0))
        .collect::<Result<_, _>>()
        .map_err(aviary_pg::Error::from)?;
    let mut plan = CleanupPlan {
        trees,
        ..CleanupPlan::default()
    };
    let mut empty = Vec::new();
    for (id, is_empty) in folders {
        if !is_empty {
            plan.kept_with_content += 1;
        } else if live.contains(&id) {
            plan.kept_live += 1;
        } else {
            empty.push(id);
        }
    }
    for chunk in empty.chunks(EXISTS_BATCH) {
        let named: BTreeSet<Uuid> = magpie
            .exists(chunk)
            .await
            .map_err(LegacyError::Other)?
            .into_iter()
            .collect();
        for id in chunk {
            if named.contains(id) {
                plan.kept_referenced += 1;
            } else {
                plan.empty_workspaces.push(*id);
            }
        }
    }
    Ok(plan)
}

/// What [`cleanup_apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupDone {
    pub trees_removed: Vec<String>,
    pub workspaces_removed: u64,
    /// Folders that had content by the time of the removal (kept), and failures, as `<path>: <error>`.
    pub kept_or_failed: Vec<String>,
}

/// Carries out `plan`: `juicefs rmr` of each tree, then each empty folder with `rmdir` (which leaves a folder that
/// got content since the plan), its quota and its `workspaces` row.
///
/// # Errors
/// A database failure.
pub async fn cleanup_apply(
    root: &Path,
    cli: &dyn JfsCli,
    hermit: &Pool,
    plan: &CleanupPlan,
) -> Result<CleanupDone, LegacyError> {
    let mut done = CleanupDone::default();
    for t in &plan.trees {
        match cli.rmr(&root.join(t)).await {
            Ok(()) => done.trees_removed.push(t.clone()),
            Err(e) => done.kept_or_failed.push(format!("{t}: {e}")),
        }
    }
    for id in &plan.empty_workspaces {
        let dir = root.join("ws").join(id.to_string());
        let d = dir.clone();
        match tokio::task::spawn_blocking(move || std::fs::remove_dir(&d)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                done.kept_or_failed.push(format!("ws/{id}: {e}"));
                continue;
            }
            Err(e) => {
                done.kept_or_failed.push(format!("ws/{id}: {e}"));
                continue;
            }
        }
        if let Err(e) = cli.quota_delete(&format!("/ws/{id}")).await {
            tracing::info!(workspace = %id, error = %e, "quota delete after the cleanup's rmdir");
        }
        hermit
            .get()
            .await?
            .execute("delete from workspaces where id = $1", &[id])
            .await?;
        done.workspaces_removed += 1;
    }
    Ok(done)
}

/// What one `legacy-cleanup` run decided.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Cleanup {
    /// Without `--confirm`: the plan, and nothing removed.
    Refused {
        plan: CleanupPlan,
    },
    Done {
        plan: CleanupPlan,
        done: CleanupDone,
    },
}

/// `hermit legacy-cleanup [--confirm]`: the plan ([`cleanup_plan`]), then, only with `confirm`, [`cleanup_apply`].
///
/// # Errors
/// As [`cleanup_plan`] (nothing removed) and [`cleanup_apply`].
pub async fn legacy_cleanup(
    root: &Path,
    hermit: &Pool,
    magpie: &dyn Referenced,
    cli: &dyn JfsCli,
    confirm: bool,
) -> Result<Cleanup, LegacyError> {
    let plan = cleanup_plan(root, hermit, magpie).await?;
    if !confirm {
        return Ok(Cleanup::Refused { plan });
    }
    let done = cleanup_apply(root, cli, hermit, &plan).await?;
    Ok(Cleanup::Done { plan, done })
}
