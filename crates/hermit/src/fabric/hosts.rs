//! The `hosts` table (hermit 10.2) as the fabric reads it, the slot arithmetic of hermit 13.3, and `host_events`
//! (today's `vm_events`, A3.F49).

use std::net::IpAddr;

use aviary_pg::{Row, Tx};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// A host's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKind {
    /// The `WireGuard` hub on a k3s node: no sandboxes, no `JuiceFS`.
    Gateway,
    /// An EC2 host.
    Aws,
}

impl HostKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Aws => "aws",
        }
    }
}

/// A host's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostStatus {
    Booting,
    Active,
    Retiring,
    Gone,
}

impl HostStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Booting => "booting",
            Self::Active => "active",
            Self::Retiring => "retiring",
            Self::Gone => "gone",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "booting" => Self::Booting,
            "active" => Self::Active,
            "retiring" => Self::Retiring,
            _ => Self::Gone,
        }
    }
}

/// One sandbox of a host's last heartbeat (`info.sandboxes[]`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InfoSandbox {
    pub name: String,
    pub ip: String,
    /// `running` | `receiving` | another state of a newer sandboxd.
    pub state: String,
    pub memory: u64,
    pub cpu_usage: f64,
    /// `None`: the host's sandboxd does not report it (read as alive).
    pub alive: Option<bool>,
    /// Bytes in use on its scratch filesystem; `None`: not reported (a receive, an older sandboxd).
    pub scratch_used: Option<u64>,
    pub created_at: String,
}

/// `hosts.info`: the last `GET /v1/host` (13.4 step 2) and what the scaler derived from it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostInfo {
    pub gateway: bool,
    pub public_ipv6: Option<String>,
    pub wg_port: Option<u16>,
    pub sandboxes: Vec<InfoSandbox>,
    pub cpu_features: String,
    pub images: Vec<String>,
    pub boot_id: String,
    pub gvisor_release: String,
    pub mem_available: Option<u64>,
    pub load_avg: Vec<f64>,
    pub uptime_s: Option<f64>,
    /// wg0's peer count.
    pub peers: usize,
    pub egress_targets: Vec<String>,
    /// `None` on a host without a `JuiceFS` mount (the gateway).
    pub jfs_mounted: Option<bool>,
    /// Since when an active host reports its mount missing (unix ms), `None` while it is there.
    pub jfs_unmounted_since: Option<i64>,
    /// sandboxd's own RSS and budget, bytes.
    pub sandboxd_rss: Option<u64>,
    pub sandboxd_budget: Option<u64>,
    /// When the host became active (unix ms): the fresh-host switch compares it (16).
    pub active_at: Option<i64>,
    /// The gateway only: since when it has answered every heartbeat without a gap (unix ms), `None` while it does
    /// not. Every replica reads it for the silence rule's settle time (12 step 3).
    pub up_since: Option<i64>,
    /// The gateway only: when the scaler leader last finished a pass (unix ms), for the ops page.
    pub last_pass_at: Option<i64>,
}

impl HostInfo {
    /// The heartbeat's facts, keeping what the scaler derived (`jfsUnmountedSince`, `activeAt`, `upSince`).
    #[must_use]
    pub fn from_host(h: &sandboxd_api::Host, prev: Option<&Self>) -> Self {
        Self {
            gateway: h.gateway,
            public_ipv6: h.public_ipv6.clone(),
            wg_port: Some(h.wg_port),
            sandboxes: h
                .sandboxes
                .iter()
                .map(|s| InfoSandbox {
                    name: s.name.clone(),
                    ip: s.ip.clone(),
                    state: match s.state {
                        sandboxd_api::SandboxState::Running => "running".into(),
                        sandboxd_api::SandboxState::Receiving => "receiving".into(),
                        sandboxd_api::SandboxState::Unknown => "unknown".into(),
                    },
                    memory: s.memory,
                    cpu_usage: s.cpu_usage,
                    alive: s.alive,
                    scratch_used: s.scratch_used,
                    created_at: s.created_at.clone(),
                })
                .collect(),
            cpu_features: h.cpu_features.clone(),
            images: h.images.clone(),
            boot_id: h.boot_id.clone(),
            gvisor_release: h.gvisor_release.clone(),
            mem_available: Some(h.mem_available),
            load_avg: h.load_avg.clone(),
            uptime_s: Some(h.uptime_s),
            peers: h.peers.len(),
            egress_targets: h.egress_targets.clone(),
            jfs_mounted: h.jfs_mounted,
            jfs_unmounted_since: prev.and_then(|p| p.jfs_unmounted_since),
            sandboxd_rss: Some(h.sandboxd.memory.rss),
            sandboxd_budget: Some(h.sandboxd.memory.budget),
            active_at: prev.and_then(|p| p.active_at),
            up_since: prev.and_then(|p| p.up_since),
            last_pass_at: prev.and_then(|p| p.last_pass_at),
        }
    }

    /// The sandbox `name` of this heartbeat.
    #[must_use]
    pub fn sandbox(&self, name: &str) -> Option<&InfoSandbox> {
        self.sandboxes.iter().find(|s| s.name == name)
    }

    /// Whether the image digest (`sha256:<hex>` or bare hex) is unpacked there.
    #[must_use]
    pub fn has_image(&self, digest: &str) -> bool {
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        self.images
            .iter()
            .any(|i| i.strip_prefix("sha256:").unwrap_or(i) == hex)
    }
}

/// A `hosts` row.
#[derive(Clone, Debug)]
pub struct HostRow {
    pub name: String,
    pub kind: HostKind,
    pub url: String,
    pub wg_ip: IpAddr,
    pub wg_public_key: Option<String>,
    pub instance_id: Option<String>,
    pub private_ip: Option<IpAddr>,
    pub status: HostStatus,
    pub retire_by: Option<String>,
    pub max_sandboxes: Option<i32>,
    pub mem_total: Option<i64>,
    pub mem_reserve: Option<i64>,
    pub cpus: Option<i32>,
    pub info: Option<HostInfo>,
    pub created_at: OffsetDateTime,
    pub last_seen_at: Option<OffsetDateTime>,
    pub empty_since: Option<OffsetDateTime>,
}

impl HostRow {
    /// Decodes a `select *` row.
    ///
    /// # Errors
    /// A column of an unexpected type.
    pub fn from_row(r: &Row) -> Result<Self, aviary_pg::Error> {
        let kind: String = r.try_get("kind")?;
        let status: String = r.try_get("status")?;
        let info: Option<serde_json::Value> = r.try_get("info")?;
        Ok(Self {
            name: r.try_get("name")?,
            kind: if kind == "gateway" {
                HostKind::Gateway
            } else {
                HostKind::Aws
            },
            url: r.try_get("url")?,
            wg_ip: r.try_get("wg_ip")?,
            wg_public_key: r.try_get("wg_public_key")?,
            instance_id: r.try_get("instance_id")?,
            private_ip: r.try_get("private_ip")?,
            status: HostStatus::parse(&status),
            retire_by: r.try_get("retire_by")?,
            max_sandboxes: r.try_get("max_sandboxes")?,
            mem_total: r.try_get("mem_total")?,
            mem_reserve: r.try_get("mem_reserve")?,
            cpus: r.try_get("cpus")?,
            info: info.and_then(|v| serde_json::from_value(v).ok()),
            created_at: r.try_get("created_at")?,
            last_seen_at: r.try_get("last_seen_at")?,
            empty_since: r.try_get("empty_since")?,
        })
    }

    #[must_use]
    pub fn is_gateway(&self) -> bool {
        self.kind == HostKind::Gateway
    }

    /// Answered within `dead` (13.3: placeable only while seen within `hostDeadSeconds`).
    #[must_use]
    pub fn fresh(&self, now: OffsetDateTime, dead: std::time::Duration) -> bool {
        self.last_seen_at.is_some_and(|s| now - s < dead)
    }

    /// The `JuiceFS` mount is not reported missing (absent and `null` count as there).
    #[must_use]
    pub fn jfs_ok(&self) -> bool {
        self.info.as_ref().and_then(|i| i.jfs_mounted) != Some(false)
    }

    #[must_use]
    pub fn gvisor_release(&self) -> Option<&str> {
        self.info
            .as_ref()
            .map(|i| i.gvisor_release.as_str())
            .filter(|r| !r.is_empty())
    }
}

/// Every non-gone host, gateway first, then by age.
///
/// # Errors
/// A database error.
pub async fn live(pool: &aviary_pg::Pool) -> Result<Vec<HostRow>, aviary_pg::Error> {
    let c = pool.get().await?;
    c.query(
        "select * from hosts where status <> 'gone' order by kind desc, created_at, name",
        &[],
    )
    .await?
    .iter()
    .map(HostRow::from_row)
    .collect()
}

/// [`live`] inside a transaction.
///
/// # Errors
/// A database error.
pub async fn live_in(t: &Tx<'_>) -> Result<Vec<HostRow>, aviary_pg::Error> {
    t.query(
        "select * from hosts where status <> 'gone' order by kind desc, created_at, name",
        &[],
    )
    .await?
    .iter()
    .map(HostRow::from_row)
    .collect()
}

/// The host `name` (any status).
///
/// # Errors
/// A database error.
pub async fn get(pool: &aviary_pg::Pool, name: &str) -> Result<Option<HostRow>, aviary_pg::Error> {
    let c = pool.get().await?;
    c.query_opt("select * from hosts where name = $1", &[&name])
        .await?
        .as_ref()
        .map(HostRow::from_row)
        .transpose()
}

/// Records a `host_events` row (best effort: a failed insert is logged, never an error of the caller).
pub async fn event(
    pool: &aviary_pg::Pool,
    kind: &str,
    host: Option<&str>,
    sandbox: Option<Uuid>,
    data: serde_json::Value,
) {
    let r = async {
        pool.get()
            .await?
            .execute(
                "insert into host_events (kind, host, sandbox_id, data) values ($1, $2, $3, $4)",
                &[&kind, &host, &sandbox, &data],
            )
            .await
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(kind, error = %e, "host event not recorded");
    }
}

/// The sizes the slot arithmetic of hermit 13.3 uses.
#[derive(Clone, Copy, Debug)]
pub struct Sizing {
    /// `sandbox.memory`, bytes.
    pub memory: u64,
    /// `sandbox.cpu`.
    pub cpu: f32,
    /// `fabric.cpuOvercommit`.
    pub overcommit: u32,
    /// `fabric.minAvailableMemory`, bytes.
    pub min_available: u64,
}

impl Sizing {
    /// `min(floor((mem_total - mem_reserve) / memory), floor(cpus * overcommit / cpu), max_sandboxes)`, at least 0.
    #[must_use]
    pub fn slots(&self, mem_total: i64, mem_reserve: i64, cpus: i32, max: Option<i32>) -> u32 {
        let mem = u64::try_from(mem_total.saturating_sub(mem_reserve)).unwrap_or(0);
        let by_mem = mem / self.memory.max(1);
        let cpu = f64::from(cpus.max(0)) * f64::from(self.overcommit) / f64::from(self.cpu.max(0.001));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // floor of a small non-negative value
        let by_cpu = cpu.floor().max(0.0) as u64;
        let mut n = by_mem.min(by_cpu);
        if let Some(m) = max {
            n = n.min(u64::try_from(m.max(0)).unwrap_or(0));
        }
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// [`Sizing::slots`] of a row (0 until its size is known).
    #[must_use]
    pub fn host_slots(&self, h: &HostRow) -> u32 {
        match (h.mem_total, h.mem_reserve, h.cpus) {
            (Some(t), Some(r), Some(c)) => self.slots(t, r, c, h.max_sandboxes),
            _ => 0,
        }
    }

    /// The slots a sandbox of `memory` bytes takes: `ceil(memory / sandbox.memory)`, at least 1.
    #[must_use]
    pub fn size(&self, memory: u64) -> u32 {
        u32::try_from(memory.div_ceil(self.memory.max(1)).max(1)).unwrap_or(u32::MAX)
    }

    /// Short of memory: `memAvailable < minAvailableMemory`.
    #[must_use]
    pub fn under_pressure(&self, h: &HostRow) -> bool {
        h.info
            .as_ref()
            .and_then(|i| i.mem_available)
            .is_some_and(|a| a < self.min_available)
    }
}

/// A placeable host with its use (13.3).
#[derive(Clone, Debug)]
pub struct Slot {
    pub host: HostRow,
    pub used: u32,
    pub free: u32,
}

/// `pick`: the fullest host with `free >= need` (tie by name), never `exclude` (13.3).
#[must_use]
pub fn pick<'a>(slots: &'a [Slot], exclude: Option<&str>, need: u32) -> Option<&'a Slot> {
    slots
        .iter()
        .filter(|s| s.free >= need && Some(s.host.name.as_str()) != exclude)
        .min_by(|a, b| a.free.cmp(&b.free).then_with(|| a.host.name.cmp(&b.host.name)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const GIB: u64 = 1 << 30;

    fn sizing() -> Sizing {
        Sizing {
            memory: 2 * GIB,
            cpu: 1.0,
            overcommit: 3,
            min_available: 256 << 20,
        }
    }

    fn row(name: &str, mem: u64, max: Option<i32>, info: Option<HostInfo>) -> HostRow {
        HostRow {
            name: name.into(),
            kind: HostKind::Aws,
            url: String::new(),
            wg_ip: "192.0.2.1".parse().unwrap(),
            wg_public_key: None,
            instance_id: None,
            private_ip: None,
            status: HostStatus::Active,
            retire_by: None,
            max_sandboxes: max,
            mem_total: Some(i64::try_from(mem).unwrap()),
            mem_reserve: Some(i64::try_from(GIB).unwrap()),
            cpus: Some(4),
            info,
            created_at: OffsetDateTime::now_utc(),
            last_seen_at: Some(OffsetDateTime::now_utc()),
            empty_since: None,
        }
    }

    /// Ported from jarvis3 `vm.test.ts` ("slots: a sandbox made with a bigger limit takes more of them; a host short
    /// of memory takes no new one").
    #[test]
    fn slots_sizes_and_pressure() {
        let s = sizing();
        assert_eq!(s.host_slots(&row("a", 8 * GIB, None, None)), 3, "(8 - 1) GiB / 2 GiB");
        assert_eq!(s.host_slots(&row("a", 8 * GIB, Some(1), None)), 1, "the operator's cap");
        assert_eq!(s.host_slots(&row("a", 8 * GIB, Some(0), None)), 0);
        // m7i.xlarge with 1 GiB sandboxes: min(14, 12) = 12 (13.3)
        let one = Sizing { memory: GIB, ..s };
        assert_eq!(
            one.slots(16 * 1024 * 1024 * 1024 - 512 * 1024 * 1024, 1 << 30, 4, None),
            12
        );
        // the gateway reports all memory reserved: 0 slots
        assert_eq!(one.slots(8 << 30, 8 << 30, 4, None), 0);
        assert_eq!(
            s.size(4 * GIB),
            2,
            "a 4 GiB sandbox from before a smaller sandbox.memory counts as two"
        );
        assert_eq!(s.size(2 * GIB), 1);
        assert_eq!(s.size(1), 1);
        let low = HostInfo {
            mem_available: Some(100 << 20),
            ..HostInfo::default()
        };
        assert!(s.under_pressure(&row("a", 8 * GIB, None, Some(low))));
        let ok = HostInfo {
            mem_available: Some(2 * GIB),
            ..HostInfo::default()
        };
        assert!(!s.under_pressure(&row("a", 8 * GIB, None, Some(ok))));
        assert!(!s.under_pressure(&row("a", 8 * GIB, None, None)));
    }

    /// Ported from `vm.test.ts` ("pick: the fullest AWS host"; the hub-first rule is gone, 13.3).
    #[test]
    fn pick_the_fullest_with_room() {
        let slot = |n: &str, used, free| Slot {
            host: row(n, 16 * GIB, None, None),
            used,
            free,
        };
        let s = [slot("a", 1, 6), slot("b", 5, 2), slot("c", 3, 0)];
        assert_eq!(pick(&s, None, 1).unwrap().host.name, "b");
        assert_eq!(pick(&s, Some("b"), 1).unwrap().host.name, "a");
        assert_eq!(
            pick(&s, None, 3).unwrap().host.name,
            "a",
            "a 3-slot sandbox needs 3 free"
        );
        assert!(pick(&s, None, 7).is_none());
        let tie = [slot("y", 1, 2), slot("x", 1, 2)];
        assert_eq!(pick(&tie, None, 1).unwrap().host.name, "x", "tie by name");
    }

    #[test]
    fn jfs_and_images() {
        let mut h = row("a", 8 * GIB, None, None);
        assert!(h.jfs_ok(), "a sandboxd that does not report it");
        h.info = Some(HostInfo {
            jfs_mounted: None,
            ..HostInfo::default()
        });
        assert!(h.jfs_ok(), "no JuiceFS on the host (the gateway)");
        h.info = Some(HostInfo {
            jfs_mounted: Some(false),
            images: vec!["ab".repeat(32)],
            ..HostInfo::default()
        });
        assert!(!h.jfs_ok());
        let i = h.info.as_ref().unwrap();
        assert!(i.has_image(&format!("sha256:{}", "ab".repeat(32))));
        assert!(!i.has_image("sha256:00"));
    }
}
