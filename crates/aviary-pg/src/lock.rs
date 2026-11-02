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

    /// The pids of the sessions holding this key in the current database (from `pg_locks`, for the log of a
    /// waiter).
    pub(crate) async fn holders(&self, client: &Client) -> Result<Vec<i32>, tokio_postgres::Error> {
        // pg_locks shows a two-key lock as (classid, objid, objsubid 2) and a one-key int8 lock as its high and low
        // halves with objsubid 1
        const BASE: &str = "select pid from pg_locks
             where locktype = 'advisory' and granted
               and database = (select oid from pg_database where datname = current_database())";
        let rows =
            match self {
                Self::Pair(a, b) => client
                    .query(
                        &format!(
                            "{BASE} and objsubid = 2 and classid = $1::int4::oid and objid = $2::int4::oid order by 1"
                        ),
                        &[a, b],
                    )
                    .await?,
                Self::Name(n) => {
                    client
                        .query(
                            &format!(
                                "{BASE} and objsubid = 1
                               and classid::int8 = ((hashtext($1::text)::int8 >> 32) & 4294967295)
                               and objid::int8 = (hashtext($1::text)::int8 & 4294967295)
                             order by 1"
                            ),
                            &[n],
                        )
                        .await?
                }
            };
        rows.iter().map(|r| r.try_get(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::LockKey;

    #[tokio::test]
    async fn the_holder_of_a_key_is_found() {
        let pool = crate::testdb::pool("holders-test", 1);
        let (a, _ta) = pool.dedicated().await.unwrap();
        let (b, _tb) = pool.dedicated().await.unwrap();
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let a_pid: i32 = a.query_one("select pg_backend_pid()", &[]).await.unwrap().get(0);
        // a pair with a negative half, and a name (hashtext may be negative: the int8 key's high half is all ones)
        let objid = i32::from_str_radix(&tag[..7], 16).unwrap();
        for key in [
            LockKey::Pair(-7, objid),
            LockKey::Pair(7, -objid),
            LockKey::name(format!("holders.{tag}")),
            LockKey::name(format!("holders.{tag}.2")),
        ] {
            assert_eq!(key.holders(&b).await.unwrap(), Vec::<i32>::new(), "{key:?}");
            assert!(key.try_lock(&a).await.unwrap(), "{key:?}");
            assert!(!key.try_lock(&b).await.unwrap(), "{key:?}");
            assert_eq!(key.holders(&b).await.unwrap(), vec![a_pid], "{key:?}");
            assert!(key.unlock(&a).await.unwrap());
        }
        // names until both signs of hashtext were seen (a negative one makes the int8 key's high half all ones)
        let (mut neg, mut pos) = (false, false);
        for g in 0..64 {
            let name = format!("holders.{g}.{tag}");
            let negative: bool = b
                .query_one("select hashtext($1::text) < 0", &[&name])
                .await
                .unwrap()
                .get(0);
            let key = LockKey::name(name);
            assert!(key.try_lock(&a).await.unwrap());
            assert_eq!(key.holders(&b).await.unwrap(), vec![a_pid], "{key:?}");
            assert!(key.unlock(&a).await.unwrap());
            neg |= negative;
            pos |= !negative;
            if neg && pos {
                break;
            }
        }
        assert!(neg && pos);
    }
}
