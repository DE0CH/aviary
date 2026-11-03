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
//! - [`failed`]: `jarvis_failed_turns` as `user.sent` + `turn.ended` failed.
//! - [`sent`]: the old hook's sent-file snapshots as `sent_files` and `files.sent`.
//! - [`viewas`]: `jarvis.ops_events` view-as rows as magpie's `view_as_log`.
//! - [`pass`]: one pass (`run`, `catch-up`): every conversation whose fingerprints changed, 8 at a time.
//! - [`verify`]: byte-exact verification through magpie's read path (cutover 7.7).
//! - [`plan`]: the dry run that reports blockers (cutover 7.9).
//! - [`lock`]: the leader lock the hand-over replicas bid for and every one-shot command waits for.
//! - [`control`]: `status`, `hold`, `reset`, `unfence`, `release --force`.
//! - [`cli`]: `magpie import <command>`.

pub mod cli;
pub mod control;
pub mod conv;
pub mod failed;
pub mod lock;
pub mod pass;
pub mod plan;
pub mod sent;
pub mod source;
pub mod sync;
pub mod tree;
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
