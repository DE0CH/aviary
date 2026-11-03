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
//! - `vm_hosts` with `status = 'gone'` become `hosts` (kind `hub` → `gateway`); the live old hosts stay `web`'s. A
//!   host imported before whose old row changed since (gone, back, gone again: same name, same instance) is updated.
//! - every share committed in the old UI's share table (`cc_shared.shares` of the `cloudcli` database, a second
//!   read-only role) and not revoked has its frozen folder `users/<key>/shares/<id>/ws` cloned (`juicefs clone`, then
//!   the quota) into `ws/<uuidv5(SHARE_NS, "share:" + id)>`, the id magpie's import gives the share; a target already
//!   there is skipped (its quota still ensured). The old UI writes that folder first and commits the row after it, so
//!   a folder without a row (a share being made, or one whose making failed) is never cloned half-written; it is
//!   cloned whole by the first pass after its row commits. Every row read is kept in `legacy_shares` (revoked or not,
//!   cloned or not) for `legacy-cleanup`.
//!
//! Every `at` keeps its source value. Each copy is idempotent (sandboxes and hosts by their own key, the event tables
//! by the unique `source_id`, shares by the target folder), so a run that is cut short or repeated changes nothing it
//! already did. One copy failing does not stop the others (each failure is in [`ImportReport::errors`], and the
//! binary exits 1). `--delta` narrows a run to what can be new since the last: event rows above the highest
//! `source_id` copied less [`ID_LOOKBACK`] (a row whose id was handed out before a neighbour's but committed after it
//! is still read), sandboxes released after the latest imported release less [`RELEASE_LOOKBACK`]; hosts (a few
//! dozen) and the shares are always read whole. A full pass (no `--delta`) also counts, per table, the source rows it
//! read that hermit does not hold ([`Count::missing`]; any is an error). `--final` (A6's last run, after the old stack
//! is gone) is a full pass that also imports what the old fabric left unfinished: owned sandboxes not `gone` become
//! `gone` with `release_code` `sandbox_lost` and reason [`FINAL_REASON`], hosts not `gone` (the old gateway among
//! them) become `gone` with that reason. The share pass starts no clone after [`ImportOptions::share_budget`] of the
//! run (the rest wait for the next run), so a run fits its `CronJob`'s deadline: the budget plus one clone's
//! [`crate::workspaces::CLONE_TIMEOUT`].

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use aviary_pg::Pool;
use aviary_pg::reexports::tokio_postgres::types::FromSqlOwned;
use hermit_api::workspaces::SHARE_NS;
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::fabric::Fut;
use crate::workspaces::{Imported, JfsCli, JuiceFs};

/// Old sandboxes released in the last this many days are imported (hermit's own retention keeps 400 days, 10.6).
pub const SANDBOX_DAYS: i32 = 400;
/// Old start events of the last this many days are imported.
pub const STARTS_DAYS: i32 = 30;
/// `--delta` re-reads this many event ids below the highest one copied.
pub const ID_LOOKBACK: i64 = 1000;
/// `--delta` re-reads sandboxes released up to this long before the latest imported release.
pub const RELEASE_LOOKBACK: Duration = Duration::from_hours(1);
/// The release reason (sandboxes) and gone reason (hosts) `--final` gives what the old fabric left unfinished.
pub const FINAL_REASON: &str = "old fabric removed";
/// The share pass's default budget: no clone starts later than this into the run.
pub const SHARE_BUDGET: Duration = Duration::from_mins(4);
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
    /// A full pass only: the rows it read that hermit does not hold after it (an unreadable host address, or a row
    /// lost to a conflict); `None` for `--delta`.
    pub missing: Option<u64>,
}

/// What one share pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ShareCount {
    /// Committed share rows read (live and revoked).
    pub rows: u64,
    /// Rows revoked (never cloned here).
    pub revoked: u64,
    pub cloned: u64,
    /// The target was already there (its quota ensured).
    pub skipped: u64,
    /// Live shares not tried because the run's share budget was spent (the next run takes them).
    pub deferred: u64,
    /// Share folders on the volume with no committed row (being made, or a failed make): not cloned.
    pub uncommitted: Vec<String>,
    /// Shares whose clone failed (the next run retries them), as `<id>: <error>`.
    pub failed: Vec<String>,
}

/// What one `import-jarvis` run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub delta: bool,
    #[serde(rename = "final")]
    pub final_pass: bool,
    pub sandboxes: Count,
    pub sandbox_starts: Count,
    pub host_events: Count,
    pub hosts: Count,
    /// `None` when the run had no share source.
    pub shares: Option<ShareCount>,
    /// Copies that failed (`<table>: <error>`) and full-pass counts that do not match; the others still ran.
    pub errors: Vec<String>,
}

impl ImportReport {
    /// Whether everything was copied: no error, no failed share clone.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.errors.is_empty() && self.shares.as_ref().is_none_or(|s| s.failed.is_empty())
    }
}

/// How an `import-jarvis` run reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    /// `--delta`: only what can be new since the last run.
    pub delta: bool,
    /// `--final`: a full pass that also imports the rows the old fabric left unfinished (module header).
    pub final_pass: bool,
    /// No share clone starts later than this into the run.
    pub share_budget: Duration,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            delta: false,
            final_pass: false,
            share_budget: SHARE_BUDGET,
        }
    }
}

/// The share source of a run: the old UI's database (`cc_shared.shares`) and the workspace store for the clones.
#[derive(Clone, Copy, Debug)]
pub struct ShareSource<'a> {
    pub cloudcli: &'a Pool,
    pub store: &'a JuiceFs,
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

/// The frozen workspace of a share row, within the volume: `users/<key>/shares/<id>/ws` from the row's `dir` (the
/// old pods' `/jfs/users/<key>/shares/<id>`), or `None` when `dir` is not that shape for this id.
#[must_use]
pub fn share_folder(id: &str, dir: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = dir.trim_end_matches('/').rsplit('/').take(4).collect();
    match parts.as_slice() {
        [last, "shares", key, "users"] if *last == id && plain_name(id) && plain_name(key) => {
            Some(PathBuf::from("users").join(key).join("shares").join(id).join("ws"))
        }
        _ => None,
    }
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

/// How many of `keys` (the source rows a full pass read) hermit does not hold: `sql` counts the ones it holds, with
/// the keys as `$1`.
async fn missing<K>(hermit: &aviary_pg::Conn, sql: &str, keys: &[K]) -> Result<u64, LegacyError>
where
    K: aviary_pg::reexports::tokio_postgres::types::ToSql + Sync,
{
    let r = hermit.query_one(sql, &[&keys]).await?;
    let held = u64::try_from(get::<i64>(&r, "n")?).unwrap_or(0);
    Ok((keys.len() as u64).saturating_sub(held))
}

/// One `import-jarvis` run: `source` is the old database, `hermit` hermit's own, `shares` the share source (`None`
/// skips the share pass). Copies in the order of the module header; a failed copy is reported in
/// [`ImportReport::errors`] and does not stop the others.
///
/// # Errors
/// None today (every failure is in the report); kept for the callers' `?`.
pub async fn import_jarvis(
    hermit: &Pool,
    source: &Pool,
    shares: Option<ShareSource<'_>>,
    root: &Path,
    opts: ImportOptions,
) -> Result<ImportReport, LegacyError> {
    let started = Instant::now();
    let delta = opts.delta && !opts.final_pass;
    let mut report = ImportReport {
        delta,
        final_pass: opts.final_pass,
        ..ImportReport::default()
    };
    let steps: [(&str, CopyFut<'_>); 4] = [
        (
            "sandboxes",
            Box::pin(import_sandboxes(hermit, source, delta, opts.final_pass)),
        ),
        ("sandbox_starts", Box::pin(import_starts(hermit, source, delta))),
        ("host_events", Box::pin(import_host_events(hermit, source, delta))),
        ("hosts", Box::pin(import_hosts(hermit, source, opts.final_pass))),
    ];
    for (table, step) in steps {
        let n = match step.await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(table, error = %e, "import-jarvis copy failed (the others go on; the next run retries)");
                report.errors.push(format!("{table}: {e}"));
                Count::default()
            }
        };
        tracing::info!(table, read = n.read, copied = n.copied, missing = ?n.missing, delta, "import-jarvis");
        if let Some(m) = n.missing.filter(|m| *m > 0) {
            report
                .errors
                .push(format!("{table}: {m} source rows read are not in hermit"));
        }
        match table {
            "sandboxes" => report.sandboxes = n,
            "sandbox_starts" => report.sandbox_starts = n,
            "host_events" => report.host_events = n,
            _ => report.hosts = n,
        }
    }
    if let Some(src) = shares {
        match import_shares(hermit, src, root, started, opts.share_budget).await {
            Ok(s) => {
                tracing::info!(
                    rows = s.rows,
                    revoked = s.revoked,
                    cloned = s.cloned,
                    skipped = s.skipped,
                    deferred = s.deferred,
                    uncommitted = s.uncommitted.len(),
                    failed = s.failed.len(),
                    "import-jarvis shares"
                );
                report.shares = Some(s);
            }
            Err(e) => {
                tracing::error!(error = %e, "import-jarvis share pass failed (the next run retries)");
                report.errors.push(format!("shares: {e}"));
            }
        }
    }
    Ok(report)
}

type CopyFut<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Count, LegacyError>> + Send + 'a>>;

async fn import_sandboxes(hermit: &Pool, source: &Pool, delta: bool, final_pass: bool) -> Result<Count, LegacyError> {
    let (hermit, source) = (hermit.get().await?, source.get().await?);
    let since: Option<OffsetDateTime> = if delta {
        let r = hermit
            .query_one("select max(released_at) from sandboxes where imported", &[])
            .await?;
        get::<Option<OffsetDateTime>>(&r, "max")?.map(|t| t - RELEASE_LOOKBACK)
    } else {
        None
    };
    // --final: a row the old fabric never finished is gone now, lost with it
    let rows = source
        .query(
            "select id, conversation_id, lower(owner) as person, pod, host, ip, created_at, last_active_at, expires_at,
                    case when status = 'gone' then released_at else coalesce(released_at, now()) end as released_at,
                    case when status = 'gone' then release_reason else $4 end as release_reason,
                    status = 'gone' as was_gone
               from sandboxes
              where (status = 'gone' or $3) and owner is not null
                and coalesce(released_at, last_active_at) > now() - make_interval(days => $1)
                and ($2::timestamptz is null or coalesce(released_at, last_active_at) > $2)
              order by coalesce(released_at, last_active_at), id",
            &[&SANDBOX_DAYS, &since, &final_pass, &FINAL_REASON],
        )
        .await?;
    let mut n = Count::default();
    let mut ids = Vec::with_capacity(rows.len());
    for r in &rows {
        n.read += 1;
        let reason: Option<String> = get(r, "release_reason")?;
        let code = if get::<bool>(r, "was_gone")? {
            old_release_code(reason.as_deref())
        } else {
            Some("sandbox_lost")
        };
        let id: Uuid = get(r, "id")?;
        ids.push(id);
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
                    &id,
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
    if !delta {
        n.missing = Some(
            missing(
                &hermit,
                "select count(*) as n from sandboxes where imported and id = any($1)",
                &ids,
            )
            .await?,
        );
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
    let mut ids = Vec::with_capacity(rows.len());
    for r in &rows {
        n.read += 1;
        let id: i64 = get(r, "id")?;
        ids.push(id);
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
                    &id,
                ],
            )
            .await?;
    }
    if !delta {
        n.missing = Some(
            missing(
                &hermit,
                "select count(*) as n from sandbox_starts where source_id = any($1)",
                &ids,
            )
            .await?,
        );
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
    let mut ids = Vec::with_capacity(rows.len());
    for r in &rows {
        n.read += 1;
        let id: i64 = get(r, "id")?;
        ids.push(id);
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
                    &id,
                ],
            )
            .await?;
    }
    if !delta {
        n.missing = Some(
            missing(
                &hermit,
                "select count(*) as n from host_events where source_id = any($1)",
                &ids,
            )
            .await?,
        );
    }
    Ok(n)
}

/// Hosts are always read whole (a few dozen rows), so every pass counts [`Count::missing`]. A host hermit already
/// holds by that name is updated only when it is `gone` there and has the same instance (the same old host, whose
/// row changed since: gone, back, gone again); a name hermit's own scaler gave a host of its own is left alone.
async fn import_hosts(hermit: &Pool, source: &Pool, final_pass: bool) -> Result<Count, LegacyError> {
    let (hermit, source) = (hermit.get().await?, source.get().await?);
    let rows = source
        .query(
            "select name, case when kind = 'hub' then 'gateway' else kind end as kind, url, wg_ip, wg_public_key,
                    instance_id, private_ip, max_sandboxes, mem_total, mem_reserve, cpus, info, created_at,
                    last_seen_at, empty_since,
                    case when status = 'gone' then gone_at else coalesce(gone_at, last_seen_at, now()) end as gone_at,
                    case when status = 'gone' then gone_reason else $2 end as gone_reason
               from vm_hosts where status = 'gone' or $1 order by name",
            &[&final_pass, &FINAL_REASON],
        )
        .await?;
    let mut n = Count::default();
    let mut names = Vec::with_capacity(rows.len());
    for r in &rows {
        n.read += 1;
        let name: String = get(r, "name")?;
        let Some(wg_ip) = inet(get(r, "wg_ip")?) else {
            tracing::warn!(host = %name, "old host with an unreadable wg_ip not imported");
            continue;
        };
        names.push(name.clone());
        let max: Option<i32> = get(r, "max_sandboxes")?;
        n.copied += hermit
            .execute(
                "insert into hosts (name, kind, url, wg_ip, wg_public_key, instance_id, private_ip, status, max_sandboxes,
                                    mem_total, mem_reserve, cpus, info, created_at, last_seen_at, empty_since, gone_at,
                                    gone_reason)
                 values ($1, $2, $3, $4, $5, $6, $7, 'gone', $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
                 on conflict (name) do update set
                   url = excluded.url, wg_ip = excluded.wg_ip, wg_public_key = excluded.wg_public_key,
                   private_ip = excluded.private_ip, max_sandboxes = excluded.max_sandboxes,
                   mem_total = excluded.mem_total, mem_reserve = excluded.mem_reserve, cpus = excluded.cpus,
                   info = excluded.info, created_at = excluded.created_at, last_seen_at = excluded.last_seen_at,
                   empty_since = excluded.empty_since, gone_at = excluded.gone_at, gone_reason = excluded.gone_reason
                 where hosts.status = 'gone' and hosts.kind = excluded.kind
                   and hosts.instance_id is not distinct from excluded.instance_id
                   and (hosts.created_at, hosts.last_seen_at, hosts.gone_at, hosts.gone_reason, hosts.info)
                       is distinct from
                       (excluded.created_at, excluded.last_seen_at, excluded.gone_at, excluded.gone_reason,
                        excluded.info)",
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
    n.missing = Some(missing(&hermit, "select count(*) as n from hosts where name = any($1)", &names).await?);
    Ok(n)
}

/// A committed share row: id, `dir`, whether revoked.
struct ShareRow {
    id: String,
    dir: String,
    revoked: bool,
}

/// `cc_shared.shares` (none when the table does not exist: an old UI that never shared).
async fn share_rows(cloudcli: &Pool) -> Result<Vec<ShareRow>, LegacyError> {
    let c = cloudcli.get().await?;
    let has: bool = get(
        &c.query_one("select to_regclass('cc_shared.shares') is not null as has", &[])
            .await?,
        "has",
    )?;
    if !has {
        return Ok(Vec::new());
    }
    c.query(
        "select id::text as id, dir::text as dir, revoked_at is not null as revoked from cc_shared.shares order by id",
        &[],
    )
    .await?
    .iter()
    .map(|r| {
        Ok(ShareRow {
            id: get(r, "id")?,
            dir: get(r, "dir")?,
            revoked: get(r, "revoked")?,
        })
    })
    .collect()
}

async fn import_shares(
    hermit: &Pool,
    src: ShareSource<'_>,
    root: &Path,
    started: Instant,
    budget: Duration,
) -> Result<ShareCount, LegacyError> {
    let rows = share_rows(src.cloudcli).await?;
    let r = root.to_owned();
    let folders = tokio::task::spawn_blocking(move || old_shares(&r))
        .await
        .map_err(|e| LegacyError::Other(e.to_string()))?
        .map_err(|e| LegacyError::Other(format!("users/: {e}")))?;
    let mut n = ShareCount::default();
    let committed: BTreeSet<&str> = rows.iter().map(|s| s.id.as_str()).collect();
    n.uncommitted = folders
        .iter()
        .filter(|(id, _)| !committed.contains(id.as_str()))
        .map(|(id, _)| id.clone())
        .collect();
    let db = hermit.get().await?;
    for s in &rows {
        n.rows += 1;
        let wid = share_workspace(&s.id);
        db.execute(
            "insert into legacy_shares (id, workspace, revoked) values ($1, $2, $3)
             on conflict (id) do update set revoked = legacy_shares.revoked or excluded.revoked, seen_at = now()",
            &[&s.id, &wid, &s.revoked],
        )
        .await?;
        if s.revoked {
            n.revoked += 1;
            continue;
        }
        let Some(rel) = share_folder(&s.id, &s.dir) else {
            n.failed
                .push(format!("{}: its dir {:?} is not users/<key>/shares/<id>", s.id, s.dir));
            continue;
        };
        if started.elapsed() >= budget {
            n.deferred += 1;
            continue;
        }
        let made = match src.store.import_clone(wid, &rel).await {
            Ok(Imported::Cloned(w)) => {
                crate::db::workspace_created(hermit, wid, w.created_at, true).await;
                tracing::info!(share = %s.id, workspace = %wid, "share workspace cloned");
                n.cloned += 1;
                true
            }
            Ok(Imported::Present(at)) => {
                crate::db::workspace_created(hermit, wid, at, false).await;
                n.skipped += 1;
                true
            }
            Err(e) => {
                tracing::warn!(share = %s.id, workspace = %wid, error = %e, "share workspace not cloned");
                n.failed.push(format!("{}: {e}", s.id));
                false
            }
        };
        if made {
            db.execute(
                "update legacy_shares set cloned_at = coalesce(cloned_at, now()) where id = $1",
                &[&s.id],
            )
            .await?;
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
/// Share clones with content removed per batch (one `exists` call, then one `juicefs rmr` each).
pub const ORPHAN_BATCH: usize = 20;
/// A workspace folder modified, or whose `workspaces` row was written, more recently than this is never removed: it
/// may belong to something being made right now (a clone whose share magpie has not committed yet).
pub const RECENT: Duration = Duration::from_hours(24);

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
    /// Old shares' clones (`legacy_shares`) with content that magpie does not reference: a share revoked, or deleted
    /// with its conversation, before magpie's import took it.
    pub orphan_shares: Vec<Uuid>,
    /// Folders kept because magpie references them.
    pub kept_referenced: u64,
    /// Folders kept because a sandbox that is not gone has that workspace.
    pub kept_live: u64,
    /// Folders kept because they, or their `workspaces` row, are younger than [`RECENT`].
    pub kept_recent: u64,
    /// Workspace folders with content that are not old shares' clones (never removed here).
    pub kept_with_content: u64,
    /// Old share folders whose data the switch has not carried over, as `<share id>: <why>`. Any one blocks the
    /// whole cleanup: `users/` holds their only copy.
    pub shares_not_carried: Vec<String>,
}

/// One `ws/<uuid>` folder: whether it is empty and when it was last modified.
struct Folder {
    id: Uuid,
    empty: bool,
    modified: SystemTime,
}

/// The `ws/<uuid>` folders under `root` (other names, such as a clone's temporary tree, are not workspaces and are
/// left alone).
fn workspace_folders(root: &Path) -> std::io::Result<Vec<Folder>> {
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
        out.push(Folder {
            id,
            empty,
            modified: m.modified().unwrap_or(SystemTime::now()),
        });
    }
    out.sort_by_key(|f| f.id);
    Ok(out)
}

fn recent(t: SystemTime) -> bool {
    t.elapsed().map_or(true, |age| age < RECENT)
}

/// What hermit's import recorded of one old share.
struct LegacyShare {
    workspace: Uuid,
    revoked: bool,
}

async fn legacy_shares(hermit: &Pool) -> Result<BTreeMap<String, LegacyShare>, LegacyError> {
    hermit
        .get()
        .await?
        .query("select id, workspace, revoked from legacy_shares", &[])
        .await?
        .iter()
        .map(|r| {
            Ok((
                get::<String>(r, "id")?,
                LegacyShare {
                    workspace: get(r, "workspace")?,
                    revoked: get(r, "revoked")?,
                },
            ))
        })
        .collect()
}

/// The plan of one `legacy-cleanup`: reads the volume, hermit's live sandboxes, recent workspace rows and the import's
/// `legacy_shares`, checks that every old share folder is carried over, and asks magpie about every folder it could
/// remove (at least one call: a magpie that does not answer stops the cleanup). Removes nothing.
///
/// # Errors
/// The volume or the database cannot be read, or magpie does not answer: then nothing is to be removed.
pub async fn cleanup_plan(root: &Path, hermit: &Pool, magpie: &dyn Referenced) -> Result<CleanupPlan, LegacyError> {
    let r = root.to_owned();
    let (folders, trees, shares) = tokio::task::spawn_blocking(move || {
        let trees: Vec<String> = ["users", "sent"]
            .into_iter()
            .filter(|t| std::fs::symlink_metadata(r.join(t)).is_ok_and(|m| m.is_dir()))
            .map(str::to_owned)
            .collect();
        let folders = workspace_folders(&r).map_err(|e| format!("ws/: {e}"))?;
        let shares = old_shares(&r).map_err(|e| format!("users/: {e}"))?;
        Ok::<_, String>((folders, trees, shares))
    })
    .await
    .map_err(|e| LegacyError::Other(e.to_string()))?
    .map_err(LegacyError::Other)?;
    let c = hermit.get().await?;
    let ids = |rows: Vec<aviary_pg::Row>| -> Result<BTreeSet<Uuid>, LegacyError> {
        rows.iter()
            .map(|r| r.try_get::<_, Uuid>(0).map_err(|e| LegacyError::Db(e.into())))
            .collect()
    };
    let live = ids(c
        .query("select conversation_id from sandboxes where status <> 'gone'", &[])
        .await?)?;
    let recent_rows = ids(c
        .query(
            "select id from workspaces where created_at > now() - $1::bigint * interval '1 second'",
            &[&i64::try_from(RECENT.as_secs()).unwrap_or(i64::MAX)],
        )
        .await?)?;
    let legacy = legacy_shares(hermit).await?;
    let share_clones: BTreeSet<Uuid> = legacy.values().map(|s| s.workspace).collect();
    let present: BTreeSet<Uuid> = folders.iter().map(|f| f.id).collect();
    let mut plan = CleanupPlan {
        trees,
        ..CleanupPlan::default()
    };
    for (id, _) in &shares {
        match legacy.get(id) {
            None => plan.shares_not_carried.push(format!(
                "{id}: no committed share row was imported (being made, a failed make, or the import did not run)"
            )),
            Some(s) if s.revoked => {}
            Some(s) if !present.contains(&s.workspace) => plan
                .shares_not_carried
                .push(format!("{id}: its clone ws/{} is missing", s.workspace)),
            Some(_) => {}
        }
    }
    let mut empty = Vec::new();
    let mut orphans = Vec::new();
    for f in &folders {
        if live.contains(&f.id) {
            plan.kept_live += 1;
        } else if recent(f.modified) || recent_rows.contains(&f.id) {
            plan.kept_recent += 1;
        } else if f.empty {
            empty.push(f.id);
        } else if share_clones.contains(&f.id) {
            orphans.push(f.id);
        } else {
            plan.kept_with_content += 1;
        }
    }
    let named = referenced(magpie, &empty, EXISTS_BATCH).await?;
    let named_orphans = referenced(magpie, &orphans, EXISTS_BATCH).await?;
    if empty.is_empty() && orphans.is_empty() {
        // nothing to ask about: magpie still has to answer before its import's trees go
        magpie.exists(&[]).await.map_err(LegacyError::Other)?;
    }
    for (all, named, out) in [
        (&empty, &named, &mut plan.empty_workspaces),
        (&orphans, &named_orphans, &mut plan.orphan_shares),
    ] {
        for id in all {
            if named.contains(id) {
                plan.kept_referenced += 1;
            } else {
                out.push(*id);
            }
        }
    }
    Ok(plan)
}

/// The ids among `ids` magpie names, asked in batches of `batch`.
async fn referenced(magpie: &dyn Referenced, ids: &[Uuid], batch: usize) -> Result<BTreeSet<Uuid>, LegacyError> {
    let mut named = BTreeSet::new();
    for chunk in ids.chunks(batch) {
        named.extend(magpie.exists(chunk).await.map_err(LegacyError::Other)?);
    }
    Ok(named)
}

/// What [`cleanup_apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupDone {
    pub trees_removed: Vec<String>,
    pub workspaces_removed: u64,
    /// Folders kept on the re-check right before their removal (`ws/<id>: <why>`): magpie or a sandbox took them,
    /// a delete of hermit's own is in flight, or an empty one got content.
    pub kept: Vec<String>,
    /// Removals that failed, as `<path>: <error>`.
    pub failed: Vec<String>,
}

impl CleanupDone {
    /// Whether everything planned was removed or kept on purpose (no failure).
    #[must_use]
    pub fn complete(&self, plan: &CleanupPlan) -> bool {
        self.failed.is_empty() && self.trees_removed.len() == plan.trees.len()
    }
}

async fn unmark(hermit: &Pool, id: Uuid) -> Result<(), LegacyError> {
    hermit
        .get()
        .await?
        .execute("delete from workspace_deletes where id = $1", &[&id])
        .await?;
    Ok(())
}

/// Carries out `plan`: `juicefs rmr` of each tree, then the workspace folders batch by batch. Each folder of a batch
/// is first marked as being deleted (`workspace_deletes`: a start or an ensure refuses it from then on), then magpie
/// is asked again about the batch, then, right before each removal, hermit's sandboxes and the folder's age are
/// checked again; a folder that fails any of these is kept and unmarked. An empty folder goes with `rmdir` (which
/// keeps one that got content since the plan), an orphan share clone with `juicefs rmr`; then its quota, its
/// `workspaces` row and its mark.
///
/// # Errors
/// A database failure.
pub async fn cleanup_apply(
    root: &Path,
    cli: &dyn JfsCli,
    hermit: &Pool,
    magpie: &dyn Referenced,
    plan: &CleanupPlan,
) -> Result<CleanupDone, LegacyError> {
    let mut done = CleanupDone::default();
    for t in &plan.trees {
        match cli.rmr(&root.join(t)).await {
            Ok(()) => done.trees_removed.push(t.clone()),
            Err(e) => done.failed.push(format!("{t}: {e}")),
        }
    }
    let batches = plan
        .empty_workspaces
        .chunks(EXISTS_BATCH)
        .map(|b| (b, false))
        .chain(plan.orphan_shares.chunks(ORPHAN_BATCH).map(|b| (b, true)));
    for (batch, orphan) in batches {
        let mut marked = Vec::with_capacity(batch.len());
        for id in batch {
            if crate::db::workspace_deleting(hermit, *id).await? {
                done.kept
                    .push(format!("ws/{id}: a delete of hermit's own is in flight"));
                continue;
            }
            crate::db::mark_workspace_deleting(hermit, *id).await?;
            marked.push(*id);
        }
        let named = match referenced(magpie, &marked, EXISTS_BATCH).await {
            Ok(n) => n,
            Err(e) => {
                for id in &marked {
                    unmark(hermit, *id).await?;
                    done.failed.push(format!("ws/{id}: not removed: {e}"));
                }
                continue;
            }
        };
        for id in marked {
            if let Some(why) = keep_now(root, hermit, id, &named).await? {
                unmark(hermit, id).await?;
                done.kept.push(format!("ws/{id}: {why}"));
                continue;
            }
            let dir = root.join("ws").join(id.to_string());
            let removed = if orphan {
                cli.rmr(&dir).await.map_err(|e| e.to_string())
            } else {
                let d = dir.clone();
                match tokio::task::spawn_blocking(move || std::fs::remove_dir(&d)).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) if e.raw_os_error() == Some(rustix::io::Errno::NOTEMPTY.raw_os_error()) => {
                        unmark(hermit, id).await?;
                        done.kept.push(format!("ws/{id}: it has content now"));
                        continue;
                    }
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            };
            if let Err(e) = removed {
                unmark(hermit, id).await?;
                done.failed.push(format!("ws/{id}: {e}"));
                continue;
            }
            if let Err(e) = cli.quota_delete(&format!("/ws/{id}")).await {
                tracing::info!(workspace = %id, error = %e, "quota delete after the cleanup's removal");
            }
            crate::db::forget_workspace(hermit, id).await;
            done.workspaces_removed += 1;
        }
    }
    Ok(done)
}

/// Why folder `id` must be kept now (the re-check right before its removal), if it must.
async fn keep_now(
    root: &Path,
    hermit: &Pool,
    id: Uuid,
    named: &BTreeSet<Uuid>,
) -> Result<Option<&'static str>, LegacyError> {
    if named.contains(&id) {
        return Ok(Some("magpie references it now"));
    }
    let c = hermit.get().await?;
    let busy = c
        .query_opt(
            "select 1 from sandboxes where conversation_id = $1 and status <> 'gone'
             union all
             select 1 from workspaces where id = $1 and created_at > now() - $2::bigint * interval '1 second'
             limit 1",
            &[&id, &i64::try_from(RECENT.as_secs()).unwrap_or(i64::MAX)],
        )
        .await?;
    if busy.is_some() {
        return Ok(Some("a sandbox has it now, or it was made recently"));
    }
    let dir = root.join("ws").join(id.to_string());
    let m = tokio::task::spawn_blocking(move || std::fs::symlink_metadata(dir))
        .await
        .map_err(|e| LegacyError::Other(e.to_string()))?;
    Ok(match m {
        Ok(m) if !m.is_dir() => Some("it is not a folder any more"),
        Ok(m) if recent(m.modified().unwrap_or(SystemTime::now())) => Some("it was modified recently"),
        Ok(_) => None,
        Err(_) => Some("it is gone already"),
    })
}

/// What one `legacy-cleanup` run decided.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum Cleanup {
    /// Without `--confirm`: the plan, and nothing removed.
    Refused {
        plan: CleanupPlan,
    },
    /// With `--confirm`, but an old share folder is not carried over ([`CleanupPlan::shares_not_carried`]): nothing
    /// removed.
    Blocked {
        plan: CleanupPlan,
    },
    Done {
        plan: CleanupPlan,
        done: CleanupDone,
    },
}

/// `hermit legacy-cleanup [--confirm]`: the plan ([`cleanup_plan`]), then, only with `confirm` and every old share
/// carried over, [`cleanup_apply`].
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
    if !plan.shares_not_carried.is_empty() {
        return Ok(Cleanup::Blocked { plan });
    }
    let done = cleanup_apply(root, cli, hermit, magpie, &plan).await?;
    Ok(Cleanup::Done { plan, done })
}
