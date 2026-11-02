//! Advisory lock keys.

use tokio_postgres::Client;

/// An advisory lock key. Postgres keeps the two-key space `(int4, int4)` and the one-key space `(int8)` apart, so a
/// [`LockKey::Pair`] never collides with a [`LockKey::Name`]. Keys are per service and live in that service's own
/// database (platform 4.3): hermit `Pair(7300, objid)`, magpie `Name("magpie.<loop>")`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LockKey {
    /// `pg_advisory_lock(classid, objid)`.
    Pair(i32, i32),
    /// `pg_advisory_lock(hashtext(name))`.
    Name(String),
}

impl LockKey {
    /// The key `hashtext(name)`.
    #[must_use]
    pub fn name(name: impl Into<String>) -> Self {
        Self::Name(name.into())
    }

    /// `pg_try_advisory_lock` on this session: `true` when it is now held.
    pub(crate) async fn try_lock(&self, client: &Client) -> Result<bool, tokio_postgres::Error> {
        let row = match self {
            Self::Pair(a, b) => {
                client
                    .query_one("select pg_try_advisory_lock($1::int4, $2::int4)", &[a, b])
                    .await?
            }
            Self::Name(n) => {
                client
                    .query_one("select pg_try_advisory_lock(hashtext($1::text))", &[n])
                    .await?
            }
        };
        row.try_get(0)
    }

    /// `pg_advisory_unlock` on this session: `true` when it was held.
    pub(crate) async fn unlock(&self, client: &Client) -> Result<bool, tokio_postgres::Error> {
        let row = match self {
            Self::Pair(a, b) => {
                client
                    .query_one("select pg_advisory_unlock($1::int4, $2::int4)", &[a, b])
                    .await?
            }
            Self::Name(n) => {
                client
                    .query_one("select pg_advisory_unlock(hashtext($1::text))", &[n])
                    .await?
            }
        };
        row.try_get(0)
    }
}
