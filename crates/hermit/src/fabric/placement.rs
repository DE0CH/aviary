//! Slots and placement (hermit 13.3): which hosts may take a sandbox, how full they are, and the placement of a
//! starting sandbox (a host and a fixed /32 from `sandboxCidr`) under lock 7300/5.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr};

use aviary_pg::Tx;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::LOCK_CLASS;
use crate::fabric::hosts::{self, HostRow, HostStatus, Sizing, Slot};
use crate::fabric::{FabricError, Place, Placement};

/// Lock 7300/5: placement (host + ip).
pub const LOCK_PLACE: i32 = 5;

/// An IPv4 CIDR `a.b.c.d/n`.
#[must_use]
pub fn parse_cidr(cidr: &str) -> Option<(u32, u32)> {
    let (base, bits) = cidr.split_once('/')?;
    let base: Ipv4Addr = base.parse().ok()?;
    let bits: u32 = bits.parse().ok()?;
    (bits <= 32).then(|| (u32::from(base), bits))
}

/// The lowest free address of `cidr` from host part `first` up to (not including) `last` (default: the end of the
/// range), skipping `.0` and `.255` of every /24.
#[must_use]
pub fn free_address(cidr: &str, used: &BTreeSet<IpAddr>, first: u32, last: Option<u32>) -> Option<Ipv4Addr> {
    let (base, bits) = parse_cidr(cidr)?;
    let size: u64 = 1u64 << (32 - bits);
    let end = last.map_or(size - 1, |l| u64::from(l).min(size - 1));
    let start = base & (u32::MAX.checked_shl(32 - bits).unwrap_or(0));
    (u64::from(first)..end).find_map(|i| {
        let n = start.checked_add(u32::try_from(i).ok()?)?;
        let low = n & 255;
        if low == 0 || low == 255 {
            return None;
        }
        let ip = Ipv4Addr::from(n);
        (!used.contains(&IpAddr::V4(ip))).then_some(ip)
    })
}

/// What decides placeability besides a host's own row.
#[derive(Clone, Copy, Debug)]
pub struct Rules {
    pub sizing: Sizing,
    /// `hostDeadSeconds`.
    pub dead: std::time::Duration,
    /// `controls.fresh_host_until` while in the future, with the time it was set (`controls.fresh_host_set_at`,
    /// written only by `POST /v1/admin/fresh-host`): only hosts that became active after that are placeable (16).
    pub fresh_since: Option<OffsetDateTime>,
}

impl Rules {
    /// Active, seen within `hostDeadSeconds`, `JuiceFS` not reported missing, not retiring, not the gateway, and not
    /// excluded by the fresh-host switch.
    #[must_use]
    pub fn placeable(&self, h: &HostRow, now: OffsetDateTime) -> bool {
        h.status == HostStatus::Active
            && !h.is_gateway()
            && h.fresh(now, self.dead)
            && h.jfs_ok()
            && self.fresh_since.is_none_or(|since| {
                h.info
                    .as_ref()
                    .and_then(|i| i.active_at)
                    .is_some_and(|a| a > since.unix_timestamp() * 1000 + i64::from(since.millisecond()))
            })
    }

    /// `free(h)`: 0 under memory pressure, else `max(0, slots - units)`.
    #[must_use]
    pub fn free(&self, h: &HostRow, units: u32) -> u32 {
        if self.sizing.under_pressure(h) {
            0
        } else {
            self.sizing.host_slots(h).saturating_sub(units)
        }
    }
}

/// The units each host carries: its live rows, plus rows migrating to it, each `ceil(memory / sandbox.memory)`.
///
/// # Errors
/// A database error.
pub async fn units_in(t: &Tx<'_>, sizing: &Sizing) -> Result<HashMap<String, u32>, aviary_pg::Error> {
    let rows = t
        .query(
            "select host, migrating_to, memory_bytes from sandboxes where status <> 'gone'",
            &[],
        )
        .await?;
    let mut out: HashMap<String, u32> = HashMap::new();
    for r in rows {
        let size = sizing.size(u64::try_from(r.try_get::<_, i64>(2)?).unwrap_or(0));
        for col in [0usize, 1] {
            if let Some(h) = r.try_get::<_, Option<String>>(col)? {
                *out.entry(h).or_default() += size;
            }
        }
    }
    Ok(out)
}

/// The rules as the database says now (the fresh-host switch).
///
/// # Errors
/// A database error.
pub async fn rules_in(t: &Tx<'_>, sizing: Sizing, dead: std::time::Duration) -> Result<Rules, aviary_pg::Error> {
    let r = t
        .query_opt(
            "select coalesce(fresh_host_set_at, updated_at) from controls where id = 1 and fresh_host_until > now()",
            &[],
        )
        .await?;
    Ok(Rules {
        sizing,
        dead,
        fresh_since: r.map(|r| r.try_get(0)).transpose()?,
    })
}

/// The placeable hosts with their use.
///
/// # Errors
/// A database error.
pub async fn slots_in(t: &Tx<'_>, rules: &Rules) -> Result<Vec<Slot>, aviary_pg::Error> {
    let now = OffsetDateTime::now_utc();
    let units = units_in(t, &rules.sizing).await?;
    Ok(hosts::live_in(t)
        .await?
        .into_iter()
        .filter(|h| rules.placeable(h, now))
        .map(|h| {
            let used = units.get(&h.name).copied().unwrap_or(0);
            let free = rules.free(&h, used);
            Slot { host: h, used, free }
        })
        .collect())
}

/// Places starting sandbox `id` (start step 3): under lock 7300/5, the fullest placeable host with room for
/// `memory` and the lowest free address of `sandbox_cidr`, recorded on the row in the same transaction, so the
/// next placement on any replica counts it.
///
/// # Errors
/// A database error, or no free address left.
pub async fn place(
    pool: &aviary_pg::Pool,
    sizing: Sizing,
    dead: std::time::Duration,
    sandbox_cidr: &str,
    id: Uuid,
    memory: u64,
) -> Result<Place, FabricError> {
    let cidr = sandbox_cidr.to_owned();
    aviary_pg::tx!(pool, |t| -> Result<Place, FabricError> {
        t.execute(
            "select pg_advisory_xact_lock($1::int4, $2::int4)",
            &[&LOCK_CLASS, &LOCK_PLACE],
        )
        .await?;
        // a row already placed (another replica's step, or the host wait's poll) keeps its place
        if let Some(r) = t
            .query_opt(
                "select host, ip from sandboxes where id = $1 and host is not null and ip is not null",
                &[&id],
            )
            .await?
        {
            return Ok(Place::Placed(Placement {
                host: r.try_get(0)?,
                ip: r.try_get(1)?,
            }));
        }
        let rules = rules_in(t, sizing, dead).await?;
        let slots = slots_in(t, &rules).await?;
        let Some(s) = hosts::pick(&slots, None, sizing.size(memory)) else {
            return Ok(Place::NoSlot);
        };
        let used: BTreeSet<IpAddr> = t
            .query(
                "select ip from sandboxes where status <> 'gone' and ip is not null",
                &[],
            )
            .await?
            .iter()
            .filter_map(|r| r.try_get(0).ok())
            .collect();
        let ip = IpAddr::V4(
            free_address(&cidr, &used, 10, None).ok_or_else(|| FabricError("no free sandbox address".into()))?,
        );
        let n = t
            .execute(
                "update sandboxes set host = $2, ip = $3 where id = $1 and status = 'starting'",
                &[&id, &s.host.name, &ip],
            )
            .await?;
        if n == 0 {
            return Err(FabricError("the sandbox is not starting".into()));
        }
        Ok(Place::Placed(Placement {
            host: s.host.name.clone(),
            ip,
        }))
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::fabric::hosts::{HostInfo, HostKind};

    fn ips(v: &[&str]) -> BTreeSet<IpAddr> {
        v.iter().map(|s| s.parse().unwrap()).collect()
    }

    /// Ported from `vm.test.ts` ("pure helpers: addresses").
    #[test]
    fn addresses() {
        let none = BTreeSet::new();
        assert_eq!(
            free_address("10.98.0.0/16", &none, 10, None),
            Some("10.98.0.10".parse().unwrap())
        );
        assert_eq!(
            free_address("10.98.0.0/16", &ips(&["10.98.0.10", "10.98.0.11"]), 10, None),
            Some("10.98.0.12".parse().unwrap())
        );
        assert_eq!(
            free_address("10.99.0.0/24", &none, 10, Some(12)),
            Some("10.99.0.10".parse().unwrap())
        );
        assert_eq!(
            free_address("10.99.0.0/24", &ips(&["10.99.0.10", "10.99.0.11"]), 10, Some(12)),
            None
        );
        // .255 and .0 of every /24 are skipped
        let mut full: Vec<String> = (10..255).map(|i| format!("10.98.0.{i}")).collect();
        full.push("10.98.1.1".into());
        let used: BTreeSet<IpAddr> = full.iter().map(|s| s.parse().unwrap()).collect();
        assert_eq!(
            free_address("10.98.0.0/16", &used, 10, None),
            Some("10.98.1.2".parse().unwrap())
        );
        assert_eq!(free_address("bad", &none, 10, None), None);
    }

    fn host(status: HostStatus, info: Option<HostInfo>, seen: Option<OffsetDateTime>) -> HostRow {
        HostRow {
            name: "a".into(),
            kind: HostKind::Aws,
            url: String::new(),
            wg_ip: "198.19.0.10".parse().unwrap(),
            wg_public_key: None,
            instance_id: None,
            private_ip: None,
            status,
            retire_by: None,
            max_sandboxes: None,
            mem_total: Some(8 << 30),
            mem_reserve: Some(1 << 30),
            cpus: Some(4),
            info,
            created_at: OffsetDateTime::now_utc(),
            last_seen_at: seen,
            empty_since: None,
        }
    }

    /// Ported from `vm.test.ts` ("placeable: active, answering, and not reporting its `JuiceFS` mount missing").
    #[test]
    fn placeable() {
        let now = OffsetDateTime::now_utc();
        let r = Rules {
            sizing: Sizing {
                memory: 2 << 30,
                cpu: 1.0,
                overcommit: 3,
                min_available: 256 << 20,
            },
            dead: std::time::Duration::from_secs(30),
            fresh_since: None,
        };
        let jfs = |m: Option<bool>| {
            Some(HostInfo {
                jfs_mounted: m,
                ..HostInfo::default()
            })
        };
        assert!(r.placeable(&host(HostStatus::Active, jfs(Some(true)), Some(now)), now));
        assert!(r.placeable(&host(HostStatus::Active, jfs(None), Some(now)), now));
        assert!(r.placeable(&host(HostStatus::Active, None, Some(now)), now));
        assert!(!r.placeable(&host(HostStatus::Active, jfs(Some(false)), Some(now)), now));
        assert!(!r.placeable(&host(HostStatus::Booting, jfs(Some(true)), Some(now)), now));
        assert!(!r.placeable(&host(HostStatus::Retiring, jfs(Some(true)), Some(now)), now));
        let old = now - time::Duration::hours(1);
        assert!(
            !r.placeable(&host(HostStatus::Active, jfs(Some(true)), Some(old)), now),
            "not answering"
        );
        assert!(!r.placeable(&host(HostStatus::Active, jfs(Some(true)), None), now));
        // the fresh-host switch: only hosts that became active after it was set
        let fresh = Rules {
            fresh_since: Some(now - time::Duration::minutes(1)),
            ..r
        };
        let active_at = |t: OffsetDateTime| {
            Some(HostInfo {
                active_at: Some(i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap()),
                ..HostInfo::default()
            })
        };
        assert!(!fresh.placeable(
            &host(
                HostStatus::Active,
                active_at(now - time::Duration::minutes(5)),
                Some(now)
            ),
            now
        ));
        assert!(fresh.placeable(&host(HostStatus::Active, active_at(now), Some(now)), now));
        // memory pressure: no room
        let low = host(
            HostStatus::Active,
            Some(HostInfo {
                mem_available: Some(1),
                ..HostInfo::default()
            }),
            Some(now),
        );
        assert_eq!(r.free(&low, 0), 0);
        assert_eq!(r.free(&host(HostStatus::Active, None, Some(now)), 1), 2);
    }
}
