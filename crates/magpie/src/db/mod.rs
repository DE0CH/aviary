//! magpie's Postgres (magpie 4): the embedded migrations, the advisory lock keys, and [`Db`], the statements both a
//! transaction ([`aviary_pg::Tx`]) and a pooled connection ([`aviary_pg::Conn`]) run, so a read can be written once
//! and used inside or outside a transaction.

use std::future::Future;

use aviary_pg::{Conn, LockKey, Migration, Row, ToSql, Tx};

/// Every migration of this build, in order. Shipped files are never edited (aviary-pg refuses a changed sha256).
///
/// # Errors
/// When the embedded list is malformed (a bad file name): a start failure.
pub fn migrations() -> Result<Vec<Migration>, aviary_pg::Error> {
    aviary_pg::migrations!(dir = "src/db/migrations"; "0001_init.sql", "0002_retention_indexes.sql", "0003_view_as_order.sql", "0004_ccfs.sql", "0005_timeline.sql", "0006_shares.sql", "0007_admin.sql", "0008_timeline_derivation.sql", "0009_cc_limbo.sql", "0010_shares_frozen.sql", "0011_cc_echoes.sql", "0012_admin_read_index.sql", "0013_window.sql", "0014_uploads_legacy_path_index.sql", "0015_ticket_supersede.sql", "0016_cc_echo_uuids.sql", "0017_blob_reverify.sql", "0018_window_rollback.sql", "0019_import_purged.sql", "0020_ticket_take.sql", "0021_legacy_import_verified.sql", "0022_ticket_claim_take.sql", "0023_import_gate_persons.sql")
}

/// The migrations' advisory lock (magpie 4).
#[must_use]
pub fn migrate_key() -> LockKey {
    LockKey::name("magpie.migrate")
}

/// The job leader lock (magpie 15).
#[must_use]
pub fn jobs_key() -> LockKey {
    LockKey::name("magpie.jobs")
}

/// Statements on a transaction or a pooled connection.
pub trait Db: Sync {
    /// Every row of `sql`.
    fn query(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> impl Future<Output = Result<Vec<Row>, aviary_pg::Error>> + Send;
    /// At most one row of `sql`.
    fn query_opt(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> impl Future<Output = Result<Option<Row>, aviary_pg::Error>> + Send;
    /// Exactly one row of `sql`.
    fn query_one(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> impl Future<Output = Result<Row, aviary_pg::Error>> + Send;
    /// The number of rows `sql` affected.
    fn execute(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> impl Future<Output = Result<u64, aviary_pg::Error>> + Send;
}

macro_rules! impl_db {
    ($t:ty) => {
        impl Db for $t {
            fn query(
                &self,
                sql: &str,
                params: &[&(dyn ToSql + Sync)],
            ) -> impl Future<Output = Result<Vec<Row>, aviary_pg::Error>> + Send {
                <$t>::query(self, sql, params)
            }
            fn query_opt(
                &self,
                sql: &str,
                params: &[&(dyn ToSql + Sync)],
            ) -> impl Future<Output = Result<Option<Row>, aviary_pg::Error>> + Send {
                <$t>::query_opt(self, sql, params)
            }
            fn query_one(
                &self,
                sql: &str,
                params: &[&(dyn ToSql + Sync)],
            ) -> impl Future<Output = Result<Row, aviary_pg::Error>> + Send {
                <$t>::query_one(self, sql, params)
            }
            fn execute(
                &self,
                sql: &str,
                params: &[&(dyn ToSql + Sync)],
            ) -> impl Future<Output = Result<u64, aviary_pg::Error>> + Send {
                <$t>::execute(self, sql, params)
            }
        }
    };
}

impl_db!(Tx<'_>);
impl_db!(Conn);

/// Makes `t` one snapshot: repeatable read, read only. The first statement of the transaction.
///
/// # Errors
/// When the statement fails.
pub async fn snapshot(t: &Tx<'_>) -> Result<(), aviary_pg::Error> {
    t.batch_execute("set transaction isolation level repeatable read, read only")
        .await
}

/// A count column (`bigint`) as `u64`.
#[must_use]
pub fn count(row: &Row, idx: &str) -> u64 {
    u64::try_from(row.get::<_, i64>(idx)).unwrap_or(0)
}
