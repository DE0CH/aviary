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
/// A byte file, bytes (magpie 17; a lines file is bounded by the quota, [`CC_QUOTA`]).
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
/// A read that answers at once is made again at most this many times when the file changed under it (6.11).
pub const CC_READ_RETRIES: u32 = 3;

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

// Blobs, uploads, files and shares (magpie 5, 10-12, 17).

/// The bucket: overall timeout of one call.
pub const BUCKET_CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// The blob read cache per replica, bytes.
pub const BLOB_CACHE_BYTES: u64 = 256 << 20;
/// Image decodes at once per replica.
pub const DECODES: usize = 4;
/// An image decode over this many pixels answers 415 (40 megapixels).
pub const DECODE_MAX_PIXELS: u64 = 40_000_000;
/// Thumbnail: long edge, pixels.
pub const THUMB_EDGE: u32 = 512;
/// Display copy: long edge, pixels.
pub const DISPLAY_EDGE: u32 = 2_000;
/// The model copy of an image: long edge, pixels.
pub const MODEL_IMAGE_EDGE: u32 = 2_000;
/// The model copy of an image: at most this many bytes.
pub const MODEL_IMAGE_MAX_BYTES: usize = 3_932_160;
/// The model copy's JPEG qualities, tried in order.
pub const MODEL_JPEG_QUALITIES: [u8; 3] = [85, 70, 55];
/// Thumbnails and display copies: WebP quality.
pub const WEBP_QUALITY: f32 = 80.0;
/// The uploader: how often each replica looks for staged blobs.
pub const UPLOAD_EVERY: Duration = Duration::from_secs(1);
/// The uploader: blobs in flight per replica.
pub const UPLOAD_IN_FLIGHT: usize = 4;
/// See [`UPLOAD_IN_FLIGHT`].
pub const UPLOAD_IN_FLIGHT_I64: i64 = 4;
/// The uploader: an item's claim (another replica takes it after this).
pub const UPLOAD_CLAIM: Duration = Duration::from_secs(120);
/// The uploader's backoff: first retry after this, doubling to [`UPLOAD_BACKOFF_MAX`].
pub const UPLOAD_BACKOFF_FIRST: Duration = Duration::from_secs(1);
/// See [`UPLOAD_BACKOFF_FIRST`].
pub const UPLOAD_BACKOFF_MAX: Duration = Duration::from_mins(5);
/// `blob upload failing` is logged at most this often per replica.
pub const UPLOAD_FAILING_LOG_EVERY: Duration = Duration::from_mins(10);
/// The blob GC: how often.
pub const BLOB_GC_EVERY: Duration = Duration::from_hours(1);
/// The blob GC: an unreferenced blob is freed only this long after its last reference.
pub const BLOB_GC_GRACE: Duration = Duration::from_hours(24);
/// The blob GC: candidates per query.
pub const BLOB_GC_BATCH: i64 = 500;
/// The bucket reconcile: how often.
pub const BUCKET_RECONCILE_EVERY: Duration = Duration::from_hours(7 * 24);
/// Turns (magpie 8.8): an admitted or preparing turn whose sandbox is not ready this long after admission fails
/// `sandbox.timeout`.
pub const ADMIT_TIMEOUT: Duration = Duration::from_mins(15);
/// The driver shows `sandbox.preparing` when no answer said ready this long after admission.
pub const PREPARING_AFTER: Duration = Duration::from_secs(1);
/// A starting turn whose `start_turn` is not acked this long after ready fails `sandbox.unreachable`.
pub const START_ACK_TIMEOUT: Duration = Duration::from_secs(120);
/// An acked `start_turn` (the `running` report in flight) gets this much more, each time.
pub const START_ACK_GRACE: Duration = Duration::from_secs(30);
/// A running turn's deadline: this long after the later of its last heartbeat and magpie's availability.
pub const HEARTBEAT_DEADLINE: Duration = Duration::from_secs(90);
/// A running turn silent this long while hermit still says alive fails `sandbox.unreachable`.
pub const SILENCE_MAX: Duration = Duration::from_mins(5);
/// hermit's `lastActiveAt` within this counts as alive for the watchdog.
pub const HERMIT_ALIVE_WITHIN: Duration = Duration::from_secs(90);
/// A stop not confirmed this long after it was asked for is forced (`stop.forced`).
pub const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// A driver's claim on a turn lasts this long and is renewed every [`DRIVER_RENEW`].
pub const DRIVER_CLAIM: Duration = Duration::from_secs(30);
/// See [`DRIVER_CLAIM`].
pub const DRIVER_RENEW: Duration = Duration::from_secs(10);
/// hermit's get-or-start (`PUT`): timeout per call.
pub const HERMIT_PUT_TIMEOUT: Duration = Duration::from_millis(2_500);
/// hermit's long-poll: `waitMs`, and the client timeout per call.
pub const HERMIT_WAIT_MS: u32 = 25_000;
/// See [`HERMIT_WAIT_MS`].
pub const HERMIT_WAIT_TIMEOUT: Duration = Duration::from_secs(35);
/// hermit's sandbox read without a wait (the watchdog, the feed's recovery): timeout per call.
pub const HERMIT_READ_TIMEOUT: Duration = Duration::from_millis(2_500);
/// The driver's retry after a hermit error: this long first, doubling to [`HERMIT_BACKOFF_MAX`].
pub const HERMIT_BACKOFF_FIRST: Duration = Duration::from_millis(500);
/// See [`HERMIT_BACKOFF_FIRST`].
pub const HERMIT_BACKOFF_MAX: Duration = Duration::from_secs(10);
/// The turn sweeper (lapsed claims, deadlines) and the availability tick run this often on every replica.
pub const SWEEP_EVERY: Duration = Duration::from_secs(5);
/// Turns the sweeper handles per pass.
pub const SWEEP_BATCH: i64 = 50;
/// An availability tick that finds the last one older than this starts a new available period (magpie 8.8).
pub const AVAILABILITY_GAP: Duration = Duration::from_secs(20);
/// hermit's sandbox-events feed: page size, `waitMs`, client timeout.
pub const FEED_LIMIT: u32 = 500;
/// See [`FEED_LIMIT`].
pub const FEED_WAIT_MS: u32 = 25_000;
/// See [`FEED_LIMIT`].
pub const FEED_TIMEOUT: Duration = Duration::from_secs(35);
/// The feed follower's pause after a failed call.
pub const FEED_RETRY: Duration = Duration::from_secs(2);
/// The title command: attempts per conversation, characters of the first message, `maxTokens`, the cleaned title.
pub const TITLE_ATTEMPTS: i16 = 3;
/// See [`TITLE_ATTEMPTS`].
pub const TITLE_PROMPT_CHARS: usize = 2_000;
/// See [`TITLE_ATTEMPTS`].
pub const TITLE_MAX_TOKENS: u32 = 100;
/// See [`TITLE_ATTEMPTS`].
pub const TITLE_MAX_CHARS: usize = 80;
/// An attached upload's workspace name: at most this many characters.
pub const WORKSPACE_NAME_MAX_CHARS: usize = 200;
