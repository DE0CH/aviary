//! Every numeric limit and timeout of magpie (magpie spec 17). Nothing else is configurable: there are no deployment
//! switches (magpie 1), so a limit is changed by changing this file.

use std::time::Duration;

/// List page (M1): at most, and the default.
pub const LIST_MAX: u32 = magpie_api::conversations::LIST_MAX;
/// Change page (M12): at most, and the default.
pub const CHANGES_MAX: u32 = magpie_api::changes::CHANGES_MAX;
/// Long-polls (timeline, changes, commands): the longest `waitMs` honoured.
pub const WAIT_MAX: Duration = Duration::from_secs(25);
/// A held long-poll re-queries this often even without a notification (magpie 14).
pub const REQUERY: Duration = Duration::from_secs(1);

/// Provisional title: this many characters (Unicode scalar values) of the first message, then `…`.
pub const PROVISIONAL_TITLE_CHARS: usize = 24;
/// Rename: 1-200 characters after trimming.
pub const RENAME_MAX_CHARS: usize = magpie_api::conversations::RENAME_MAX_CHARS;
/// `idempotencyKey` of a create: at most this many bytes (a bound for the unique index; the app's keys are short).
pub const IDEMPOTENCY_KEY_MAX: usize = 200;
/// Turn text: 1 MiB of UTF-8.
pub const TURN_TEXT_MAX_BYTES: usize = magpie_api::turns::TURN_TEXT_MAX_BYTES;
/// Attachments per turn: images and files, each.
pub const ATTACHMENTS_MAX: usize = 10;

/// Recently deleted: purged this long after the soft delete.
pub const RECENTLY_DELETED: Duration = Duration::from_hours(30 * 24);
/// Change feed retention.
pub const CHANGES_RETENTION: Duration = Duration::from_hours(30 * 24);
/// Unattached uploads are deleted after this.
pub const UNATTACHED_UPLOAD: Duration = Duration::from_hours(24);
/// Acked commands, `cc_ops` and `report_ids` are kept this long.
pub const IDEMPOTENCY_RETENTION: Duration = Duration::from_hours(24);
/// Sent-file staging rows (phase 1 of a snapshot) are kept this long.
pub const SENT_STAGING_RETENTION: Duration = Duration::from_hours(1);
/// `moved` limbo entries expire after this (the moved-limbo match window).
pub const MOVED_LIMBO: Duration = Duration::from_mins(10);

/// cc write body, bytes (16 MiB; the supervisor splits larger writes).
pub const CC_WRITE_MAX: usize = magpie_api::sandbox::CC_WRITE_MAX_BYTES;
/// A JSONL line, including a tail being built, bytes.
pub const CC_LINE_MAX: i64 = 64 << 20;
/// A file (a byte file; a lines file too, so the supervisor can always cache it whole), bytes.
pub const CC_FILE_MAX: i64 = 256 << 20;
/// Claude Code state per conversation (`cc_bytes`).
pub const CC_QUOTA: i64 = 2 << 30;
/// Paths: at most this many bytes, components at most [`CC_COMPONENT_MAX`].
pub const CC_PATH_MAX: usize = 4096;
/// See [`CC_PATH_MAX`].
pub const CC_COMPONENT_MAX: usize = 255;
/// Byte files are stored in pages of this many bytes.
pub const CC_PAGE: i64 = 256 * 1024;
/// Blob staging total: past it, cc writes that add a blob answer 503 until the uploader drains it.
pub const STAGING_MAX: i64 = 2 << 30;
/// Limbo of a truncate is retracted after the file has had no operation for this long (or at the turn's end).
pub const LIMBO_QUIET: Duration = Duration::from_secs(10);
/// The limbo sweeper runs this often.
pub const LIMBO_SWEEP_EVERY: Duration = Duration::from_secs(2);
/// The lease's question to hermit (no wait): its timeout.
pub const LEASE_HERMIT_TIMEOUT: Duration = Duration::from_secs(2);
/// One in this many reconstructed lines has its sha256 checked in a release build (every line in a debug build).
pub const CC_SHA_SAMPLE: u64 = 1000;
/// A file read streams lines in batches of this many.
pub const CC_READ_BATCH: i64 = 256;

/// The job leader lock is retried this often by the replicas that do not hold it (magpie 15).
pub const LEADER_RETRY: Duration = Duration::from_secs(10);
/// The purge job: how often, and how many conversations per pass.
pub const PURGE_EVERY: Duration = Duration::from_mins(10);
/// See [`PURGE_EVERY`].
pub const PURGE_BATCH: i64 = 20;
/// The outbox: how often due items are tried, and how many per pass.
pub const OUTBOX_EVERY: Duration = Duration::from_secs(5);
/// See [`OUTBOX_EVERY`].
pub const OUTBOX_BATCH: i64 = 50;
/// The outbox backoff: first retry after this, doubling to [`OUTBOX_BACKOFF_MAX`].
pub const OUTBOX_BACKOFF_FIRST: Duration = Duration::from_secs(5);
/// See [`OUTBOX_BACKOFF_FIRST`].
pub const OUTBOX_BACKOFF_MAX: Duration = Duration::from_hours(1);
/// An outbox item failing for this long logs an error (`outbox failing`) every [`OUTBOX_FAILING_LOG_EVERY`].
pub const OUTBOX_FAILING_AFTER: Duration = Duration::from_hours(24);
/// See [`OUTBOX_FAILING_AFTER`].
pub const OUTBOX_FAILING_LOG_EVERY: Duration = Duration::from_hours(1);
/// One hermit call of the outbox: overall timeout.
pub const OUTBOX_CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// The retention job: how often.
pub const RETENTION_EVERY: Duration = Duration::from_hours(1);
/// Retention deletes at most this many rows per statement, looping until nothing is due, so a pass after a long
/// pause (or the first after the import) never runs one statement past the statement timeout.
pub const RETENTION_BATCH: i64 = 5_000;

/// Postgres pool: request connections per replica (R11; plus one LISTEN and one leader connection).
pub const POOL_SIZE: usize = 12;
/// Per-statement timeout of the pool's connections.
pub const STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);
/// A request that could not reach Postgres answers 503 `unavailable` with this `retry-after` (seconds).
pub const UNAVAILABLE_RETRY_AFTER_S: u64 = 1;
/// 409 `conversation_migrating`: `retryAfterMs` (cutover 6.4).
pub const MIGRATING_RETRY_AFTER_MS: u64 = 2_000;

/// Request bodies, every route: the largest any route takes (an upload, 50 MiB, with headroom). Routes with a
/// smaller limit (a cc write: 16 MiB) check their own.
pub const BODY_MAX: usize = 64 << 20;
/// Drain: in-flight requests are given this long to finish (magpie 18.3).
pub const DRAIN_IN_FLIGHT: Duration = Duration::from_secs(30);
