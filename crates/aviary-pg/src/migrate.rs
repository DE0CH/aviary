//! Embedded migrations applied once under an advisory lock (platform 4.3).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio_postgres::Client;

use crate::{Error, LockKey, Pool};

/// The table that records applied migrations, in each service's own database.
pub const MIGRATIONS_TABLE: &str = "_migrations";

/// The first line that marks a migration to run outside a transaction (for `create index concurrently`, which
/// Postgres refuses inside one). Such a migration holds exactly one statement and must be safe to run again
/// (`if not exists`): a crash between the statement and its record re-runs it on the next start.
pub const NO_TRANSACTION_MARKER: &str = "-- aviary-pg: no-transaction";

/// How often a starter waiting for another one's migrations retries the lock. Waiters poll with
/// `pg_try_advisory_lock` rather than block in `pg_advisory_lock`: a blocked statement holds a snapshot, and
/// `create index concurrently` in the holder waits for every older snapshot, so blocking waiters would deadlock it.
const LOCK_POLL: Duration = Duration::from_millis(200);

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
            let statements = m
                .sql
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect::<String>()
                .split(';')
                .filter(|s| !s.trim().is_empty())
                .count();
            if statements != 1 {
                return Err(Error::BadMigration(format!(
                    "no-transaction migration {} ({}) holds {statements} statements, not 1",
                    m.version, m.name
                )));
            }
        }
    }
    Ok(())
}

async fn run(client: &Client, key: &LockKey, migrations: &[Migration]) -> Result<Vec<i32>, Error> {
    // DDL may take longer than the request timeout; this session only migrates
    client.batch_execute("set statement_timeout = 0").await?;
    let mut waited = false;
    while !key.try_lock(client).await? {
        if !waited {
            tracing::info!("waiting for another starter's migrations");
            waited = true;
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
        apply(client, m).await?;
        tracing::info!(version = m.version, migration = %m.name, "migration applied");
        applied.push(m.version);
    }
    key.unlock(client).await?;
    Ok(applied)
}

async fn apply(client: &Client, m: &Migration) -> Result<(), Error> {
    let record = format!("insert into {MIGRATIONS_TABLE} (version, name, sha256) values ($1, $2, $3)");
    let sha = m.sha256();
    if m.no_transaction() {
        client.batch_execute(m.sql).await?;
        client.execute(&record, &[&m.version, &m.name, &sha]).await?;
        return Ok(());
    }
    client.batch_execute("begin").await?;
    let res = async {
        client.batch_execute(m.sql).await?;
        client.execute(&record, &[&m.version, &m.name, &sha]).await?;
        client.batch_execute("commit").await
    }
    .await;
    if let Err(e) = res {
        let _ = client.batch_execute("rollback").await;
        tracing::error!(version = m.version, migration = %m.name, error = %e, "migration failed");
        return Err(e.into());
    }
    Ok(())
}
