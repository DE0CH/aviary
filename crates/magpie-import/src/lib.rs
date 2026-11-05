//! The one-off importer into magpie (cutover 6-7, magpie 19), linked into the `magpie` binary as `magpie import …`
//! and removed after the switch (cutover 10.5 step 8).
//!
//! It reads the old stack's sources read-only (database `cloudcli`, every `cc_u*` schema, and the person's Claude
//! Code files under the `JuiceFS` mount) and writes every row, event and cc file through magpie's own library
//! ([`magpie::ccfs::ops`] with [`magpie::ccfs::Caller::importer`], [`magpie::events::append`],
//! [`magpie::changes::upserted`]), so an imported conversation is what live writes would have made.
//!
//! - [`source`]: one `repeatable read` read of `cloudcli` (persons, sessions rows, failed turns) and the timestamp
//!   rules of magpie 19.1.
//! - [`tree`]: a conversation's source files on `JuiceFS` and their cc paths (cutover 7.4.2), stats and hashes.
//! - [`sync`]: the sync of one file (cutover 7.4.2 rules 1-6) through the importer's cc writes.
//! - [`conv`]: creating a migrating conversation, mapping a sessions row's changes, the purge of a vanished one.
//! - [`export`]: the reverse export of owned conversations to the old stack (cutover 14.3).
//! - [`failed`]: `jarvis_failed_turns` as `user.sent` + `turn.ended` failed.
//! - [`sent`]: the old hook's sent-file snapshots as `sent_files` and `files.sent`.
//! - [`share`]: `cc_shared.shares` as magpie shares with the same ids (cutover 7.4.4).
//! - [`viewas`]: `jarvis.ops_events` view-as rows as magpie's `view_as_log`.
//! - [`questions`]: the old stack's parked questions (`jarvis_open_questions`) carried over at the release.
//! - [`parity`]: magpie's rows through pigeon's projection, normalised for the render-parity rehearsal.
//! - [`pass`]: one pass (`run`, `catch-up`): every conversation whose fingerprints changed, 8 at a time.
//! - [`verify`]: byte-exact verification through magpie's read path (cutover 7.7).
//! - [`plan`]: the dry run that reports blockers (cutover 7.9).
//! - [`holds`]: the running sets, `holds.js` run in the old ui pods.
//! - [`kube`]: the old cluster's API: the ui pods, `pods/exec` (the running sets' transport) and `pods/proxy` (the
//!   switch module's report and restart).
//! - [`gate`]: the gate's socket check (every ui pod's switch-module report).
//! - [`handover`]: the hand-over loop (cutover 7.5, DESIGN 9.4), the per-person gate, and the recovery of fences a
//!   killed leader left.
//! - [`reverse`]: the reverse hand-over of one conversation (cutover 14.2).
//! - [`uploads`]: upload ingestion of the old asset folders (cutover 7.4.3).
//! - [`server`]: `handover` and `export-server`.
//! - [`lock`]: the leader lock the hand-over replicas bid for and every one-shot command waits for.
//! - [`control`]: `status`, `hold`, `reset`, `unfence`, `release --force`.
//! - [`cli`]: `magpie import <command>`.

pub mod cli;
pub mod control;
pub mod conv;
pub mod export;
pub mod export_more;
pub mod failed;
pub mod gate;
pub mod handover;
pub mod holds;
pub mod kube;
pub mod lock;
pub mod parity;
pub mod pass;
pub mod plan;
pub mod questions;
pub mod reverse;
pub mod sent;
pub mod server;
pub mod share;
pub mod source;
pub mod sync;
pub mod tree;
pub mod uploads;
pub mod verify;
pub mod viewas;

use std::path::PathBuf;
use std::sync::Arc;

use aviary_pg::Pool;
use magpie::app::Magpie;
use uuid::Uuid;

/// The namespace of every uuidv5 of the switch (cutover 6.6: hermit 3.8's and magpie 12's `SHARE_NS`).
pub const NS: Uuid = Uuid::from_u128(0x6f1c_9d2e_0b7a_4f53_9a43_5b2f_3f1e_8c61);

/// What the importer can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// magpie's library refused or failed.
    #[error("magpie: {0}")]
    Magpie(#[from] magpie::error::Error),
    /// A source or magpie database statement.
    #[error("database: {0}")]
    Db(#[from] aviary_pg::Error),
    /// The `JuiceFS` mount.
    #[error("files: {0}")]
    Io(#[from] std::io::Error),
    /// The sources say something the importer cannot map (a schema whose key is not its person's, a row id that
    /// is not a uuid, a line over the cap).
    #[error("source: {0}")]
    Source(String),
    /// The conversation stopped being migrating between the read and the write (the release took it): the pass
    /// leaves it (cutover 6.2 item 2).
    #[error("the conversation is no longer migrating")]
    Released,
    /// The one-shot command's leader lock went with its connection: it stops before another writer starts.
    #[error("the importer's lock was lost (its connection ended)")]
    LockLost,
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Source(format!("json: {e}"))
    }
}

impl From<aviary_pg::reexports::tokio_postgres::Error> for Error {
    fn from(e: aviary_pg::reexports::tokio_postgres::Error) -> Self {
        Error::Db(aviary_pg::Error::Db(e))
    }
}

/// Shorthand.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A magpie library error, with "no longer migrating" (409 `lease_lost` from an importer write) told apart.
#[must_use]
pub fn from_magpie(e: magpie::error::Error) -> Error {
    if e.is(&magpie_api::errors::ErrorCode::LeaseLost) {
        Error::Released
    } else {
        Error::Magpie(e)
    }
}

/// The importer's connections: magpie's library (its pool, blobs and fan-out) and the old sources.
#[derive(Debug, Clone)]
pub struct Importer {
    /// magpie, without its jobs ([`magpie::app::Service::start_library`]).
    pub magpie: Arc<Magpie>,
    /// Database `cloudcli`, read-only.
    pub cloudcli: Pool,
    /// Database `jarvis` (the view-as audit), read-only.
    pub jarvis: Pool,
    /// The `JuiceFS` mount (`/jfs`), read-only.
    pub jfs: PathBuf,
    /// The `JuiceFS` client the export clones a share's workspace with (`juicefs clone`, metadata only, through the
    /// mount's `.control` file: no credentials); `None` copies the tree instead (tests, which have no mount).
    pub juicefs: Option<PathBuf>,
    /// The one-shot command's leader lock, checked before each conversation's write ([`Importer::check_lock`]);
    /// `None` when nothing holds it (tests, the read-only commands).
    pub lock: Option<Arc<lock::OneShot>>,
    /// How long a `sent/<conversation>/<toolUseId>/` folder must be unchanged before it is imported (the old hook may
    /// still be writing it): [`sent::QUIET`] in the importer, zero in tests.
    pub sent_quiet: std::time::Duration,
    /// A pause inside each fence the importer holds (after each file the export writes, before a forced release's
    /// final sync): zero in the importer; tests make it longer than [`magpie::limits::FENCE_LEASE`].
    pub pause_in_fence: std::time::Duration,
    /// The uid and gid the export gives the files it writes (Claude Code's in the old sandboxes, [`export::OWNER_ID`]);
    /// a file that cannot be given to it fails the export. `None` leaves them as written (tests, run unprivileged).
    pub owner: Option<u32>,
}

impl Importer {
    /// Fails with [`Error::LockLost`] when this process held the leader lock and no longer does.
    ///
    /// # Errors
    /// [`Error::LockLost`].
    pub async fn check_lock(&self) -> Result<()> {
        match &self.lock {
            Some(l) if !l.held().await => Err(Error::LockLost),
            _ => Ok(()),
        }
    }

    /// [`Importer::check_lock`] inside a write transaction (see [`lock::OneShot::check_in`]).
    ///
    /// # Errors
    /// [`Error::LockLost`]; a statement error.
    pub async fn check_lock_in(&self, t: &aviary_pg::Tx<'_>) -> Result<()> {
        match &self.lock {
            Some(l) => l.check_in(t).await,
            None => Ok(()),
        }
    }
}

/// Sleeps for `d` (nothing for zero): [`Importer::pause_in_fence`].
pub(crate) async fn pause(d: std::time::Duration) {
    if !d.is_zero() {
        tokio::time::sleep(d).await;
    }
}

/// Lowercase hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 of `bytes`, hex.
#[must_use]
pub fn sha_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex(&sha2::Sha256::digest(bytes))
}
