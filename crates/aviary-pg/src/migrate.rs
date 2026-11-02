//! Embedded migrations applied once under an advisory lock (platform 4.3).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio_postgres::Client;
use tokio_postgres::error::SqlState;

use crate::{Error, LockKey, Pool, sql, stmt};

/// The table that records applied migrations, in each service's own database.
pub const MIGRATIONS_TABLE: &str = "_migrations";

/// The first line that marks a migration to run outside a transaction (for `create index concurrently`, which
/// Postgres refuses inside one). Such a migration holds exactly one statement and must be safe to run again: a crash
/// between the statement and its record re-runs it on the next start, so write `create index concurrently if not
/// exists <name> on …`. A failed or interrupted concurrent build leaves an INVALID index, which `if not exists` would
/// accept: so when the statement is `create [unique] index concurrently [if not exists] <name> on <table>`,
/// [`migrate`] drops that one index first if it is invalid (and no build of it is under way), and fails the start if
/// the statement leaves it invalid ([`Error::InvalidIndex`], the index dropped). No other index is touched: an
/// operator's own build elsewhere in the database is never dropped.
pub const NO_TRANSACTION_MARKER: &str = "-- aviary-pg: no-transaction";

/// How long a migration's statement waits for a table lock before it gives up (`lock_timeout`). While an `alter
/// table` waits for its `ACCESS EXCLUSIVE` lock, every later query on the table queues behind it; a short wait,
/// rolled back and retried, keeps the running replicas serving (zero downtime, platform 3.3). A no-transaction
/// migration (`create index concurrently`) takes a lock that does not block reads or writes and waits for older
/// transactions by design, so it runs without the limit.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
/// Attempts of a migration that gave up waiting for a lock, and the first pause between them (doubling).
const LOCK_ATTEMPTS: u32 = 6;
const LOCK_BACKOFF: Duration = Duration::from_secs(1);

/// How often a starter waiting for another one's migrations retries the lock. Waiters poll with
/// `pg_try_advisory_lock` rather than block in platform 4.3's `pg_advisory_lock`: a blocked statement holds a
/// snapshot, and `create index concurrently` in the holder waits for every older snapshot, so blocking waiters would
/// deadlock it. The wait has no bound (the holder's migrations may take long); it logs the holder's pid every
/// [`LOCK_WAIT_LOG`].
const LOCK_POLL: Duration = Duration::from_millis(200);
const LOCK_WAIT_LOG: Duration = Duration::from_secs(30);

/// One embedded migration: `migrations/NNNN_name.sql`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// `NNNN`, from 1, strictly increasing in the list.
    pub version: i32,
    /// `name`.
    pub name: String,
    /// The file's SQL, exactly as embedded.
    pub sql: &'static str,
}

impl Migration {
    /// A migration from its file name (`0001_init.sql`) and contents; [`migrations!`](crate::migrations!) builds
    /// these.
    ///
    /// # Errors
    /// [`Error::BadMigration`] when the file name is not `NNNN_name.sql` with `NNNN` > 0.
    pub fn embedded(file: &str, sql: &'static str) -> Result<Self, Error> {
        let bad = || Error::BadMigration(format!("file name {file:?} is not NNNN_name.sql"));
        let stem = file.strip_suffix(".sql").ok_or_else(bad)?;
        let (num, name) = stem.split_once('_').ok_or_else(bad)?;
        if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) || name.is_empty() {
            return Err(bad());
        }
        let version: i32 = num.parse().map_err(|_| bad())?;
        if version <= 0 {
            return Err(bad());
        }
        Ok(Self {
            version,
            name: name.to_owned(),
            sql,
        })
    }

    /// Lowercase hex sha256 of the SQL.
    #[must_use]
    pub fn sha256(&self) -> String {
        let digest = Sha256::digest(self.sql.as_bytes());
        let mut hex = String::with_capacity(64);
        for b in digest {
            let _ = write!(hex, "{b:02x}");
        }
        hex
    }

    fn no_transaction(&self) -> bool {
        self.sql.lines().next().map(str::trim_end) == Some(NO_TRANSACTION_MARKER)
    }
}

/// Embeds `migrations/<file>` of the calling crate, in the order given:
/// `aviary_pg::migrations!["0001_init.sql", "0002_people.sql"]` evaluates to
/// `Result<Vec<Migration>, aviary_pg::Error>`. `aviary_pg::migrations!(dir = "other/dir"; "0001_init.sql")` reads
/// another directory of the crate.
#[macro_export]
macro_rules! migrations {
    (dir = $dir:literal; $($file:literal),* $(,)?) => {
        (|| -> ::std::result::Result<::std::vec::Vec<$crate::Migration>, $crate::Error> {
            ::std::result::Result::Ok(::std::vec![$(
                $crate::Migration::embedded(
                    $file,
                    ::std::include_str!(::std::concat!(::std::env!("CARGO_MANIFEST_DIR"), "/", $dir, "/", $file)),
                )?
            ),*])
        })()
    };
    ($($file:literal),* $(,)?) => {
        $crate::migrations!(dir = "migrations"; $($file),*)
    };
}

/// Applies `migrations` in order, each at most once, under the session advisory lock `key` on a dedicated
/// connection, and records each in `_migrations(version, name, sha256, applied_at)` in the same transaction as its
/// SQL. Concurrent starters wait for the lock; the one that gets it applies what is missing, the others then find
/// everything recorded. Returns the versions this call applied.
///
/// A recorded version this build does not embed is accepted (a newer release applied it; migrations are expand-only
/// within a release, so the older build still works).
///
/// # Errors
/// [`Error::MigrationEdited`] when a recorded migration's sha256 differs from the embedded one (a start failure);
/// [`Error::BadMigration`] for a malformed list; [`Error::Db`] when a migration fails (it is rolled back and the
/// error names the version).
pub async fn migrate(pool: &Pool, key: &LockKey, migrations: &[Migration]) -> Result<Vec<i32>, Error> {
    check_list(migrations)?;
    let (client, connection) = pool.dedicated().await?;
    let result = run(&client, key, migrations).await;
    // dropping the session releases the lock whatever happened
    drop(client);
    let _ = connection.await;
    result
}

fn check_list(migrations: &[Migration]) -> Result<(), Error> {
    let mut last = 0;
    for m in migrations {
        if m.version <= last {
            return Err(Error::BadMigration(format!(
                "version {} ({}) does not follow {last}",
                m.version, m.name
            )));
        }
        last = m.version;
        if m.no_transaction() {
            // comments, strings, quoted names and dollar bodies are lexed, so only real statements count
            let statements = sql::statements(m.sql).len();
            if statements != 1 {
                return Err(Error::BadMigration(format!(
                    "no-transaction migration {} ({}) holds {statements} statements, not 1",
                    m.version, m.name
                )));
            }
        } else if let Err(e) = stmt::refuse_control(m.sql, true) {
            // `commit;` in the file would commit the SQL apart from its `_migrations` record
            return Err(Error::BadMigration(format!(
                "migration {} ({}) controls the transaction it runs in: {e}",
                m.version, m.name
            )));
        }
    }
    Ok(())
}

async fn run(client: &Client, key: &LockKey, migrations: &[Migration]) -> Result<Vec<i32>, Error> {
    // DDL may take longer than the request timeout; this session only migrates
    client
        .batch_execute(&format!(
            "set statement_timeout = 0; set lock_timeout = {}",
            LOCK_TIMEOUT.as_millis()
        ))
        .await?;
    let mut next_log = Some(std::time::Instant::now());
    let started = std::time::Instant::now();
    while !key.try_lock(client).await? {
        if next_log.is_some_and(|t| t <= std::time::Instant::now()) {
            let holders = key.holders(client).await.unwrap_or_default();
            tracing::info!(
                holder_pids = ?holders,
                waited_s = started.elapsed().as_secs(),
                "waiting for another starter's migrations"
            );
            next_log = Some(std::time::Instant::now() + LOCK_WAIT_LOG);
        }
        tokio::time::sleep(LOCK_POLL).await;
    }
    client
        .batch_execute(&format!(
            "create table if not exists {MIGRATIONS_TABLE} (
               version int primary key,
               name text not null,
               sha256 text not null,
               applied_at timestamptz not null default now()
             )"
        ))
        .await?;
    let recorded: HashMap<i32, String> = client
        .query(&format!("select version, sha256 from {MIGRATIONS_TABLE}"), &[])
        .await?
        .iter()
        .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
        .collect::<Result<_, tokio_postgres::Error>>()?;
    // every recorded one is verified before anything new is applied
    for m in migrations {
        if let Some(sha) = recorded.get(&m.version) {
            let embedded = m.sha256();
            if *sha != embedded {
                return Err(Error::MigrationEdited {
                    version: m.version,
                    name: m.name.clone(),
                    recorded: sha.clone(),
                    embedded,
                });
            }
        }
    }
    let mut applied = Vec::new();
    for m in migrations.iter().filter(|m| !recorded.contains_key(&m.version)) {
        if m.no_transaction() {
            apply_no_transaction(client, m).await?;
        } else {
            apply_retrying(client, m).await?;
        }
        tracing::info!(version = m.version, migration = %m.name, "migration applied");
        applied.push(m.version);
    }
    key.unlock(client).await?;
    Ok(applied)
}

fn record_sql() -> String {
    format!("insert into {MIGRATIONS_TABLE} (version, name, sha256) values ($1, $2, $3)")
}

/// A transactional migration, retried with backoff when it gave up waiting for a lock.
async fn apply_retrying(client: &Client, m: &Migration) -> Result<(), Error> {
    let mut pause = LOCK_BACKOFF;
    for attempt in 1..=LOCK_ATTEMPTS {
        match apply(client, m).await {
            Err(e) if attempt < LOCK_ATTEMPTS && e.code() == Some(&SqlState::LOCK_NOT_AVAILABLE) => {
                tracing::warn!(
                    version = m.version,
                    migration = %m.name,
                    attempt,
                    retry_in_ms = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
                    "migration gave up waiting for a lock; retrying"
                );
                tokio::time::sleep(pause).await;
                pause *= 2;
            }
            Err(e) => {
                tracing::error!(version = m.version, migration = %m.name, attempt, error = %e, "migration failed");
                return Err(e.into());
            }
            Ok(()) => return Ok(()),
        }
    }
    unreachable!("the last attempt returns")
}

/// One attempt: the SQL and its record in one transaction, rolled back on any error.
async fn apply(client: &Client, m: &Migration) -> Result<(), tokio_postgres::Error> {
    client.batch_execute("begin").await?;
    let res = async {
        client.batch_execute(m.sql).await?;
        client
            .execute(&record_sql(), &[&m.version, &m.name, &m.sha256()])
            .await?;
        client.batch_execute("commit").await
    }
    .await;
    if res.is_err() {
        let _ = client.batch_execute("rollback").await;
    }
    res
}

/// A no-transaction migration: the index it builds dropped first if a failed build left it invalid, its one
/// statement without a lock limit, then that index may not be left invalid.
async fn apply_no_transaction(client: &Client, m: &Migration) -> Result<(), Error> {
    let target = sql::statements(m.sql).first().and_then(|s| sql::create_index_target(s));
    client.batch_execute("set lock_timeout = 0").await?;
    if let Some((index, table)) = &target {
        match invalid_index(client, index, table).await? {
            Some(Invalid::Idle(q)) => {
                tracing::warn!(index = %q, "dropping an invalid index (an earlier concurrent build failed)");
                drop_index(client, &q).await?;
            }
            Some(Invalid::Building(q)) => {
                // `if not exists` would accept it; the check after the statement fails the start
                tracing::warn!(index = %q, "an invalid index may be being built by another session; left alone");
            }
            None => {}
        }
    }
    let res = client.batch_execute(m.sql).await;
    let mut left = Vec::new();
    if let Some((index, table)) = &target {
        match invalid_index(client, index, table).await? {
            Some(Invalid::Idle(q)) => {
                drop_index(client, &q).await?;
                left.push(q);
            }
            Some(Invalid::Building(q)) => left.push(q),
            None => {}
        }
    }
    client
        .batch_execute(&format!("set lock_timeout = {}", LOCK_TIMEOUT.as_millis()))
        .await?;
    if let Err(e) = res {
        tracing::error!(version = m.version, migration = %m.name, error = %e, invalid = ?left, "migration failed");
        return Err(e.into());
    }
    if !left.is_empty() {
        return Err(Error::InvalidIndex {
            version: m.version,
            name: m.name.clone(),
            indexes: left,
        });
    }
    client
        .execute(&record_sql(), &[&m.version, &m.name, &m.sha256()])
        .await?;
    Ok(())
}

/// An invalid index, quoted (`"schema"."name"`).
enum Invalid {
    /// No session is building it: safe to drop.
    Idle(String),
    /// A build of it is under way, or this role cannot see every build in progress (without `pg_read_all_stats`
    /// another role's rows of `pg_stat_progress_create_index` show no relation): never dropped.
    Building(String),
}

/// The index `name` in `table`'s schema when it is a plain index (`relkind 'i'`; a partitioned parent's index is
/// left alone) and invalid.
async fn invalid_index(client: &Client, name: &str, table: &str) -> Result<Option<Invalid>, Error> {
    let row = client
        .query_opt(
            "select format('%I.%I', n.nspname, c.relname),
                    exists (select 1 from pg_stat_progress_create_index p
                            where p.datid = (select oid from pg_database where datname = current_database())
                              and (p.index_relid = c.oid or p.relid = i.indrelid or p.relid is null)
                              and p.pid <> pg_backend_pid())
             from pg_class c
             join pg_index i on i.indexrelid = c.oid
             join pg_namespace n on n.oid = c.relnamespace
             where c.relname = $1
               and c.relkind = 'i'
               and c.relnamespace = (select relnamespace from pg_class where oid = to_regclass($2))
               and not i.indisvalid",
            &[&name, &table],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let quoted: String = row.try_get(0)?;
    let building: bool = row.try_get(1)?;
    Ok(Some(if building {
        Invalid::Building(quoted)
    } else {
        Invalid::Idle(quoted)
    }))
}

async fn drop_index(client: &Client, quoted: &str) -> Result<(), Error> {
    client
        .batch_execute(&format!("drop index concurrently if exists {quoted}"))
        .await?;
    Ok(())
}
