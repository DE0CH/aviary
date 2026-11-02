//! Claude Code state behind the supervisor's FUSE layer (magpie 6): the conversation's `~/.claude` scope as files
//! (`lines`, `bytes`, `dir`), the single-writer lease, and the operations of the cc routes (magpie 16.2).
//!
//! Every state-changing operation is one transaction ([`crate::cc_op!`]): the conversation row lock, the lease and `opId`
//! checks, the operation, quota accounting, the `cc_ops` record (magpie 6.10). Nothing in it calls the bucket: a blob
//! the operation needs that is only in the bucket fails it with [`Error::NeedBlobs`], and the macro fetches those and
//! runs it again.
//!
//! - [`scan`] / [`extract`]: a JSONL line into its stub and blobs (6.4); [`classify`]: what a main-transcript line
//!   is (6.5).
//! - [`lines`]: lines files, the rewrite algorithm and the main-transcript rules (6.2, 6.3, 6.6); [`limbo`]:
//!   retraction and limbo (6.7); [`pages`]: byte files (6.8); [`reconstruct`]: the read path (6.11);
//!   [`lease`]: the lease (6.9); [`ops`]: the routes' operations.
//!
//! The seam for the turn package (WP-14): [`lines::set_main_transcript`] (the `running` report, which promotes the
//! lines already written at that path), [`limbo::flush_scope`] (an accepted end report flushes the turn's limbo) and
//! [`set_resync`] (`cc.write_stuck`).

pub mod classify;
pub mod extract;
pub mod lease;
pub mod limbo;
pub mod lines;
pub mod ops;
pub mod pages;
pub mod reconstruct;
pub mod scan;

use std::collections::HashMap;

use aviary_pg::Tx;
use magpie_api::errors::{ErrorCode, Refusal};
use magpie_api::server::Ctx;
use serde_json::Value;
use uuid::Uuid;

use crate::blobs;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::limits;
use extract::Sha;

/// The sandbox behind a gateway call (`Aviary-Sandbox-*`, set by the gateway only; magpie 3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// hermit's sandbox id.
    pub sandbox: Uuid,
    /// Its conversation: the cc scope.
    pub conversation: Uuid,
    /// The conversation's owner.
    pub person: String,
    /// `Aviary-Lease`, when sent.
    pub lease: Option<String>,
}

impl Caller {
    /// From the request's headers.
    ///
    /// # Errors
    /// 400 `bad_request` without the sandbox identity (the gateway always sets it).
    pub fn from_ctx(ctx: &Ctx) -> Result<Self> {
        let s = ctx
            .sandbox
            .as_ref()
            .ok_or_else(|| Error::bad_request("the sandbox identity is missing"))?;
        let parse = |v: &str, what: &str| {
            Uuid::parse_str(v.trim()).map_err(|_| Error::bad_request(format!("{what} is not a uuid")))
        };
        Ok(Self {
            sandbox: parse(&s.id, "Aviary-Sandbox-Id")?,
            conversation: parse(&s.conversation, "Aviary-Sandbox-Conversation")?,
            person: s.person.clone(),
            lease: ctx.lease.clone(),
        })
    }
}

/// What a path is (magpie 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A directory.
    Dir,
    /// A `.jsonl` file (or one whose name contains `.jsonl.`), stored as lines.
    Lines,
    /// Any other file, stored in pages.
    Bytes,
}

impl Kind {
    /// `cc_files.kind`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Dir => "dir",
            Kind::Lines => "lines",
            Kind::Bytes => "bytes",
        }
    }

    fn of_db(s: &str) -> Self {
        match s {
            "dir" => Kind::Dir,
            "lines" => Kind::Lines,
            _ => Kind::Bytes,
        }
    }

    /// The contract's kind.
    #[must_use]
    pub fn api(self) -> magpie_api::sandbox::CcKind {
        match self {
            Kind::Dir => magpie_api::sandbox::CcKind::Dir,
            Kind::Lines => magpie_api::sandbox::CcKind::Lines,
            Kind::Bytes => magpie_api::sandbox::CcKind::Bytes,
        }
    }
}

/// The kind of a file at `path`, by its name: `lines` when the last component ends in `.jsonl` or contains
/// `.jsonl.` (a compaction temp file, a set-aside), else `bytes`.
#[must_use]
pub fn kind_of(path: &str) -> Kind {
    let last = path.rsplit('/').next().unwrap_or(path);
    #[allow(clippy::case_sensitive_file_extension_comparisons)] // the rule is by exact name
    if last.ends_with(".jsonl") || last.contains(".jsonl.") {
        Kind::Lines
    } else {
        Kind::Bytes
    }
}

/// The parent directory of `path` (`None` at the top of the scope).
#[must_use]
pub fn parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(p, _)| p)
}

/// A path relative to `~/.claude`: at most 4,096 bytes, components of 1-255 bytes, no NUL, no `.` or `..`, not
/// absolute, no trailing slash.
///
/// # Errors
/// 400 `bad_path`.
pub fn check_path(path: &str) -> Result<()> {
    let ok = !path.is_empty()
        && path.len() <= limits::CC_PATH_MAX
        && !path.contains('\0')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c.len() <= limits::CC_COMPONENT_MAX && c != "." && c != "..");
    if ok {
        Ok(())
    } else {
        Err(Error::code(ErrorCode::BadPath, "not a valid path under ~/.claude"))
    }
}

/// The locked conversation as an operation sees it.
#[derive(Debug, Clone)]
pub struct Conv {
    /// The conversation (the cc scope).
    pub id: Uuid,
    /// Its owner.
    pub person: String,
    /// The main transcript path.
    pub main: Option<String>,
    /// The active turn.
    pub active_turn: Option<Uuid>,
    /// `cc_bytes` before this operation.
    pub cc_bytes: i64,
    /// The title.
    pub title: Option<String>,
    /// Where it came from (`None` = provisional).
    pub name_source: Option<String>,
    /// The model of the latest assistant line.
    pub model: Option<String>,
    /// The importer holds it.
    pub migrating: bool,
    lease_id: Option<Uuid>,
    lease_sandbox: Option<Uuid>,
}

/// The turn the lines of a write are credited to (magpie 6.3).
#[derive(Debug, Clone, Copy)]
pub struct Turn {
    /// The turn.
    pub id: Uuid,
    /// A person-origin turn whose `user.sent` echo is not matched yet (6.5).
    pub echo_pending: bool,
}

/// One state-changing operation in its transaction.
#[derive(Debug)]
pub struct Op<'p> {
    /// The conversation, locked.
    pub conv: Conv,
    /// The change to `cc_bytes` so far.
    pub cc_delta: i64,
    /// The turn new lines are credited to.
    pub turn: Option<Turn>,
    /// Blob bytes readable in the transaction.
    pub blobs: blobs::Bytes<'p>,
    /// `conversation_upserted` already emitted in this transaction.
    pub upserted: bool,
}

/// Locks the caller's conversation row (magpie 4: the conversation first).
///
/// # Errors
/// 404 `not_found` when the conversation does not exist or is not the sandbox person's; a statement error.
pub async fn lock(tx: &Tx<'_>, caller: &Caller) -> Result<Conv> {
    let row = tx
        .query_opt(
            "select person, main_transcript, active_turn, cc_bytes, title, name_source, model, migrating, lease_id,
                    lease_sandbox
             from conversations where id = $1 for update",
            &[&caller.conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let person: String = row.get(0);
    if person != caller.person {
        return Err(Error::not_found());
    }
    Ok(Conv {
        id: caller.conversation,
        person,
        main: row.get(1),
        active_turn: row.get(2),
        cc_bytes: row.get(3),
        title: row.get(4),
        name_source: row.get(5),
        model: row.get(6),
        migrating: row.get(7),
        lease_id: row.get(8),
        lease_sandbox: row.get(9),
    })
}

impl Conv {
    /// 409 `lease_lost` unless `caller` presents the current lease from the sandbox that holds it (magpie 6.9).
    ///
    /// # Errors
    /// That refusal.
    pub fn check_lease(&self, caller: &Caller) -> Result<()> {
        let presented = caller.lease.as_deref().and_then(|l| Uuid::parse_str(l.trim()).ok());
        if presented.is_some() && presented == self.lease_id && self.lease_sandbox == Some(caller.sandbox) {
            Ok(())
        } else {
            Err(Error::code(
                ErrorCode::LeaseLost,
                "this lease is not the conversation's current one",
            ))
        }
    }

    /// 409 `conversation_migrating` while the importer holds the conversation.
    ///
    /// # Errors
    /// That refusal.
    pub fn refuse_migrating(&self) -> Result<()> {
        if self.migrating {
            return Err(Refusal::migrating(limits::MIGRATING_RETRY_AFTER_MS).into());
        }
        Ok(())
    }

    /// Whether `path` is the main transcript.
    #[must_use]
    pub fn is_main(&self, path: &str) -> bool {
        self.main.as_deref() == Some(path)
    }
}

/// The start of an operation: the stored result of a replayed `opId`, or the operation to run.
#[derive(Debug)]
pub enum Begun<'p> {
    /// A committed `opId`: its stored result.
    Replay(Value),
    /// Run it.
    Go(Box<Op<'p>>),
}

/// Starts an operation: lock, migrating, lease, `opId` (magpie 6.10, in that order).
#[allow(clippy::implicit_hasher)] // the fetched bytes are always this map
///
/// # Errors
/// 404, 409 `conversation_migrating` / `lease_lost`; a statement error.
pub async fn begin<'p>(tx: &Tx<'_>, caller: &Caller, op_id: Uuid, pre: &'p HashMap<Sha, Vec<u8>>) -> Result<Begun<'p>> {
    let conv = lock(tx, caller).await?;
    conv.refuse_migrating()?;
    conv.check_lease(caller)?;
    if let Some(row) = tx
        .query_opt(
            "select result from cc_ops where scope = $1 and op_id = $2",
            &[&caller.conversation, &op_id],
        )
        .await?
    {
        return Ok(Begun::Replay(row.get(0)));
    }
    Ok(Begun::Go(Box::new(Op {
        conv,
        cc_delta: 0,
        turn: None,
        blobs: blobs::Bytes::with(pre),
        upserted: false,
    })))
}

impl Op<'_> {
    /// Credits the operation's new lines to `turn` when it names a turn of this conversation, else to the active
    /// turn, else to none (magpie 6.3).
    ///
    /// # Errors
    /// When a statement fails.
    pub async fn credit(&mut self, tx: &Tx<'_>, turn: Option<Uuid>) -> Result<()> {
        for candidate in [turn, self.conv.active_turn].into_iter().flatten() {
            if let Some(row) = tx
                .query_opt(
                    "select origin, echo_matched from turns where id = $1 and conversation_id = $2",
                    &[&candidate, &self.conv.id],
                )
                .await?
            {
                let origin: String = row.get(0);
                let matched: bool = row.get(1);
                self.turn = Some(Turn {
                    id: candidate,
                    echo_pending: origin == "person" && !matched,
                });
                return Ok(());
            }
        }
        Ok(())
    }

    /// The turn id new lines get.
    #[must_use]
    pub fn turn_id(&self) -> Option<Uuid> {
        self.turn.map(|t| t.id)
    }

    /// Ends the operation: the quota check (only an operation that grows the state can be refused), `cc_bytes`, the
    /// `cc_ops` record of `result`.
    ///
    /// # Errors
    /// 507 `quota_exceeded`; a statement error.
    pub async fn finish(&self, tx: &Tx<'_>, op_id: Uuid, result: &Value) -> Result<()> {
        let after = self.conv.cc_bytes + self.cc_delta;
        if self.cc_delta > 0 && after > limits::CC_QUOTA {
            return Err(Error::code(
                ErrorCode::QuotaExceeded,
                "the conversation's Claude Code state is over its quota",
            ));
        }
        if self.cc_delta != 0 {
            tx.execute(
                "update conversations set cc_bytes = greatest(cc_bytes + $2, 0) where id = $1",
                &[&self.conv.id, &self.cc_delta],
            )
            .await?;
        }
        tx.execute(
            "insert into cc_ops (scope, op_id, result) values ($1, $2, $3)",
            &[&self.conv.id, &op_id, result],
        )
        .await?;
        Ok(())
    }

    /// Emits `conversation_upserted` once per operation (a title or model change).
    ///
    /// # Errors
    /// When a statement fails.
    pub async fn upserted(&mut self, tx: &Tx<'_>) -> Result<()> {
        if !self.upserted {
            crate::changes::upserted(tx, &self.conv.person, self.conv.id).await?;
            self.upserted = true;
        }
        Ok(())
    }
}

/// A path's row (magpie 4 `cc_files`).
#[derive(Debug, Clone)]
pub struct File {
    /// Its id.
    pub id: i64,
    /// Relative to `~/.claude`.
    pub path: String,
    /// What it is.
    pub kind: Kind,
    /// Mode bits.
    pub mode: i32,
    /// The supervisor's mtime.
    pub mtime_ns: i64,
    /// Bytes.
    pub size: i64,
    /// Bumped by every operation.
    pub version: i64,
    /// Lines files: the bytes after the last `\n`.
    pub tail: Vec<u8>,
    /// Lines files: the next `ord`.
    pub next_ord: i64,
}

const FILE_COLS: &str = "id, path, kind, mode, mtime_ns, size, version, tail, next_ord";

fn file_of(r: &aviary_pg::Row) -> File {
    File {
        id: r.get(0),
        path: r.get(1),
        kind: Kind::of_db(r.get(2)),
        mode: r.get(3),
        mtime_ns: r.get(4),
        size: r.get(5),
        version: r.get(6),
        tail: r.get(7),
        next_ord: r.get(8),
    }
}

/// The file at `path` of `scope`.
///
/// # Errors
/// When the statement fails.
pub async fn load_file(db: &impl Db, scope: Uuid, path: &str) -> Result<Option<File>> {
    Ok(db
        .query_opt(
            &format!("select {FILE_COLS} from cc_files where scope = $1 and path = $2"),
            &[&scope, &path],
        )
        .await?
        .as_ref()
        .map(file_of))
}

/// Every path of `scope` at or under `dir`.
///
/// # Errors
/// When the statement fails.
pub async fn load_under(db: &impl Db, scope: Uuid, dir: &str) -> Result<Vec<File>> {
    let rows = db
        .query(
            &format!(
                "select {FILE_COLS} from cc_files where scope = $1 and (path = $2 or starts_with(path, $2 || '/'))
                 order by path"
            ),
            &[&scope, &dir],
        )
        .await?;
    Ok(rows.iter().map(file_of).collect())
}

/// Whether `path` has anything under it.
///
/// # Errors
/// When the statement fails.
pub async fn has_children(db: &impl Db, scope: Uuid, path: &str) -> Result<bool> {
    Ok(db
        .query_opt(
            "select 1 from cc_files where scope = $1 and starts_with(path, $2 || '/') limit 1",
            &[&scope, &path],
        )
        .await?
        .is_some())
}

/// 404 `not_found` unless the parent of `path` is a directory of `scope` (or the top).
///
/// # Errors
/// That refusal; a statement error.
pub async fn require_parent(db: &impl Db, scope: Uuid, path: &str) -> Result<()> {
    if let Some(p) = parent(path) {
        match load_file(db, scope, p).await? {
            Some(f) if f.kind == Kind::Dir => {}
            _ => return Err(Error::code(ErrorCode::NotFound, "the parent directory does not exist")),
        }
    }
    Ok(())
}

/// Creates the row of a new path.
///
/// # Errors
/// When the statement fails.
pub async fn create_file(tx: &Tx<'_>, scope: Uuid, path: &str, kind: Kind, mode: i32, mtime_ns: i64) -> Result<File> {
    let row = tx
        .query_one(
            &format!(
                "insert into cc_files (scope, path, kind, mode, mtime_ns) values ($1, $2, $3, $4, $5)
                 returning {FILE_COLS}"
            ),
            &[&scope, &path, &kind.as_str(), &mode, &mtime_ns],
        )
        .await?;
    Ok(file_of(&row))
}

/// Writes a file's row back, `last_op_at` now (the limbo debounce, 6.7).
///
/// # Errors
/// When the statement fails.
pub async fn save_file(tx: &Tx<'_>, f: &File) -> Result<()> {
    tx.execute(
        "update cc_files set path = $2, kind = $3, mode = $4, mtime_ns = $5, size = $6, version = $7, tail = $8,
                next_ord = $9, last_op_at = now()
         where id = $1",
        &[
            &f.id,
            &f.path,
            &f.kind.as_str(),
            &f.mode,
            &f.mtime_ns,
            &f.size,
            &f.version,
            &f.tail,
            &f.next_ord,
        ],
    )
    .await?;
    Ok(())
}

/// The bytes a file's rows count against the quota (stubs and tail, or pages).
///
/// # Errors
/// When the statement fails.
pub async fn stored_bytes(db: &impl Db, f: &File) -> Result<i64> {
    let sql = match f.kind {
        Kind::Dir => return Ok(0),
        Kind::Lines => "select coalesce(sum(octet_length(stub)), 0)::bigint from cc_lines where file_id = $1",
        Kind::Bytes => "select coalesce(sum(octet_length(bytes)), 0)::bigint from cc_pages where file_id = $1",
    };
    let row = db.query_one(sql, &[&f.id]).await?;
    let n: i64 = row.get(0);
    Ok(n + i64::try_from(f.tail.len()).unwrap_or(0))
}

/// Sets `cc_resync` (a `cc.write_stuck` report, magpie 6.12): the next lease answers `resync: true`.
///
/// # Errors
/// When the statement fails.
pub async fn set_resync(tx: &Tx<'_>, conversation: Uuid) -> Result<()> {
    tx.execute(
        "update conversations set cc_resync = true where id = $1",
        &[&conversation],
    )
    .await?;
    Ok(())
}

/// Runs one cc transaction (`aviary_pg::tx!` with the body), fetching the bucket's blobs it asks for
/// ([`Error::NeedBlobs`]) outside it and running it again, at most a few times. `$pre` names the fetched bytes inside
/// the body. Evaluates to the transaction's `Result`; a failed fetch returns from the enclosing function.
#[macro_export]
#[doc(hidden)]
macro_rules! cc_tx {
    ($app:expr, |$pre:ident, $t:ident| -> $ret:ty $body:block) => {{
        let mut __cc_pre: ::std::collections::HashMap<$crate::ccfs::extract::Sha, Vec<u8>> =
            ::std::collections::HashMap::new();
        let mut __cc_tries = 0u32;
        loop {
            let $pre = &__cc_pre;
            let __cc_r: $crate::error::Result<$ret> =
                aviary_pg::tx!(&$app.pool, |$t| -> $crate::error::Result<$ret> $body).await;
            match __cc_r {
                Err($crate::error::Error::NeedBlobs(shas)) if __cc_tries < 3 => {
                    __cc_tries += 1;
                    let got = $crate::blobs::fetch_stored($app, &shas).await?;
                    __cc_pre.extend(got);
                }
                other => break other,
            }
        }
    }};
}

/// One cc operation with `opId` (magpie 6.10): [`cc_tx!`] around [`begin`] (a replay answers the stored result),
/// the body (with `$op`, the [`Op`]), and [`Op::finish`] recording the body's result.
#[macro_export]
#[doc(hidden)]
macro_rules! cc_op {
    ($app:expr, $caller:expr, $op_id:expr, |$t:ident, $op:ident| -> $ret:ty $body:block) => {
        $crate::cc_tx!($app, |__cc_pre, $t| -> $ret {
            let mut $op = match $crate::ccfs::begin($t, $caller, $op_id, __cc_pre).await? {
                $crate::ccfs::Begun::Replay(v) => return Ok(::serde_json::from_value(v)?),
                $crate::ccfs::Begun::Go(op) => op,
            };
            let __cc_r: $ret = $body;
            $op.finish($t, $op_id, &::serde_json::to_value(&__cc_r)?).await?;
            Ok(__cc_r)
        })
    };
}

/// `i64` of a length.
#[must_use]
pub fn n64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// `usize` of a non-negative offset.
#[must_use]
pub fn nus(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
}
