//! Network pushes (hermit 13.8): the gateway's `WireGuard` peers (one per AWS host with the /32s of its sandboxes),
//! each AWS host's mesh peers (the other AWS hosts, for migrations), and the egress targets behind the VIP, read
//! from the `EndpointSlices` of Service `fabric.egressService` with a watch. Every push sets the whole set from the
//! rows, so it is idempotent; it is never made inside a transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};

use sandboxd_api::Peer;

use crate::fabric::hosts::{HostKind, HostRow, HostStatus};

/// sandboxd's peer table holds at most this many peers (A5.F37).
pub const MAX_PEERS: usize = 32;
/// The AWS hosts' `WireGuard` port on their VPC addresses (mesh peers).
pub const MESH_PORT: u16 = 51820;

/// The gateway's peers: every non-gone AWS host with a key, its overlay /32 and its sandboxes' /32s (`placed` is
/// `(host, ip)` of every live row with both, `host` being where it runs now), and its IPv6 endpoint when it
/// reported one, so the gateway can dial it first.
#[must_use]
pub fn gateway_peers(hosts: &[HostRow], placed: &[(String, IpAddr)]) -> Vec<Peer> {
    let mut by_host: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (h, ip) in placed {
        by_host.entry(h.as_str()).or_default().push(format!("{ip}/32"));
    }
    let mut peers: Vec<Peer> = hosts
        .iter()
        .filter(|h| h.kind == HostKind::Aws && h.status != HostStatus::Gone)
        .filter_map(|h| {
            let key = h.wg_public_key.clone()?;
            let endpoint = h.info.as_ref().and_then(|i| {
                i.public_ipv6
                    .as_ref()
                    .map(|v6| format!("[{v6}]:{}", i.wg_port.unwrap_or(MESH_PORT)))
            });
            let mut allowed = vec![format!("{}/32", h.wg_ip)];
            allowed.extend(by_host.get(h.name.as_str()).cloned().unwrap_or_default());
            Some(Peer {
                public_key: key,
                endpoint,
                allowed_ips: allowed,
            })
        })
        .collect();
    peers.truncate(MAX_PEERS);
    peers
}

/// The mesh peers of AWS host `of`: the other AWS hosts with a key and a VPC address, at `<private_ip>:51820`.
#[must_use]
pub fn mesh_peers(of: &HostRow, hosts: &[HostRow]) -> Vec<Peer> {
    let mut peers: Vec<Peer> = hosts
        .iter()
        .filter(|o| o.kind == HostKind::Aws && o.status != HostStatus::Gone && o.name != of.name)
        .filter_map(|o| {
            Some(Peer {
                public_key: o.wg_public_key.clone()?,
                endpoint: Some(std::net::SocketAddr::new(o.private_ip?, MESH_PORT).to_string()),
                allowed_ips: vec![format!("{}/32", o.wg_ip)],
            })
        })
        .collect();
    peers.truncate(MAX_PEERS);
    peers
}

/// Where the egress targets come from.
pub trait EgressSource: Send + Sync + 'static {
    /// The ready egress pods' IPv4 addresses now; `None` before the first list (the last pushed list is kept).
    fn targets(&self) -> Option<Vec<String>>;
}

/// A list set by hand (tests, and a deployment without a watch).
#[derive(Debug, Default)]
pub struct Fixed(Mutex<Option<Vec<String>>>);

impl Fixed {
    #[must_use]
    pub fn new(targets: Option<Vec<String>>) -> Self {
        Self(Mutex::new(targets))
    }

    pub fn set(&self, targets: Option<Vec<String>>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = targets;
    }
}

impl EgressSource for Fixed {
    fn targets(&self) -> Option<Vec<String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The ready IPv4 endpoints of a set of `EndpointSlices` (one endpoint is ready unless its `ready` condition is
/// false or it is terminating; a slice of another address type is skipped), sorted, without repeats.
#[must_use]
pub fn ready_ipv4<'a>(
    slices: impl IntoIterator<Item = &'a k8s_openapi::api::discovery::v1::EndpointSlice>,
) -> Vec<String> {
    let mut out = BTreeSet::new();
    for s in slices {
        if s.address_type != "IPv4" {
            continue;
        }
        for e in s.endpoints.iter().flatten() {
            let c = e.conditions.as_ref();
            let ready = c.and_then(|c| c.ready) != Some(false) && c.and_then(|c| c.terminating) != Some(true);
            if !ready {
                continue;
            }
            for a in &e.addresses {
                if a.parse::<std::net::Ipv4Addr>().is_ok() {
                    out.insert(a.clone());
                }
            }
        }
    }
    out.into_iter().collect()
}

/// The `EndpointSlices` of Service `service` in `namespace`, kept current by a watch (`discovery.k8s.io/v1`,
/// `kubernetes.io/service-name=<service>`).
pub struct Watched {
    latest: Arc<Mutex<Option<Vec<String>>>>,
    task: tokio::task::AbortHandle,
}

impl std::fmt::Debug for Watched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watched").finish_non_exhaustive()
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Watched {
    /// Starts the watch with the in-cluster configuration.
    ///
    /// # Errors
    /// When the in-cluster configuration cannot be read.
    pub async fn start(namespace: &str, service: &str) -> Result<Self, kube::Error> {
        use futures_util::StreamExt as _;
        use k8s_openapi::api::discovery::v1::EndpointSlice;
        use kube::runtime::watcher;
        let client = kube::Client::try_default().await?;
        let api: kube::Api<EndpointSlice> = kube::Api::namespaced(client, namespace);
        let cfg = watcher::Config::default().labels(&format!("kubernetes.io/service-name={service}"));
        let latest = Arc::new(Mutex::new(None));
        let out = latest.clone();
        let task = tokio::spawn(async move {
            let mut slices: BTreeMap<String, EndpointSlice> = BTreeMap::new();
            let mut init: BTreeMap<String, EndpointSlice> = BTreeMap::new();
            let mut stream = watcher(api, cfg).boxed();
            while let Some(ev) = stream.next().await {
                let changed = match ev {
                    Ok(watcher::Event::Apply(s)) => {
                        slices.insert(s.metadata.name.clone().unwrap_or_default(), s);
                        true
                    }
                    Ok(watcher::Event::Delete(s)) => {
                        slices.remove(&s.metadata.name.clone().unwrap_or_default());
                        true
                    }
                    Ok(watcher::Event::Init) => {
                        init.clear();
                        false
                    }
                    Ok(watcher::Event::InitApply(s)) => {
                        init.insert(s.metadata.name.clone().unwrap_or_default(), s);
                        false
                    }
                    Ok(watcher::Event::InitDone) => {
                        slices = std::mem::take(&mut init);
                        true
                    }
                    Err(e) => {
                        // the watcher retries by itself; the last list is kept meanwhile
                        tracing::warn!(error = %e, "egress endpoint watch");
                        false
                    }
                };
                if changed {
                    let t = ready_ipv4(slices.values());
                    *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(t);
                }
            }
        })
        .abort_handle();
        Ok(Self { latest, task })
    }
}

impl EgressSource for Watched {
    fn targets(&self) -> Option<Vec<String>> {
        self.latest.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use k8s_openapi::api::discovery::v1::{Endpoint, EndpointConditions, EndpointSlice};
    use time::OffsetDateTime;

    use super::*;
    use crate::fabric::hosts::HostInfo;

    fn host(name: &str, ip: &str, key: Option<&str>, v6: Option<&str>, private: Option<&str>) -> HostRow {
        HostRow {
            name: name.into(),
            kind: HostKind::Aws,
            url: String::new(),
            wg_ip: ip.parse().unwrap(),
            wg_public_key: key.map(Into::into),
            instance_id: None,
            private_ip: private.map(|p| p.parse().unwrap()),
            status: HostStatus::Active,
            retire_by: None,
            max_sandboxes: None,
            mem_total: None,
            mem_reserve: None,
            cpus: None,
            info: v6.map(|v| HostInfo {
                public_ipv6: Some(v.into()),
                wg_port: Some(51820),
                ..HostInfo::default()
            }),
            created_at: OffsetDateTime::now_utc(),
            last_seen_at: None,
            empty_since: None,
        }
    }

    /// Ported from `vm.test.ts` ("hub peers: each AWS host's /32s, and its IPv6 endpoint when it reports one").
    #[test]
    fn gateway_peers_carry_each_hosts_32s_and_its_endpoint() {
        let hosts = [
            host("a", "198.19.0.10", Some("k-a"), Some("2001:db8::5"), None),
            host("b", "198.19.0.11", Some("k-b"), None, None),
            host("c", "198.19.0.12", None, None, None),
        ];
        let peers = gateway_peers(&hosts, &[("a".into(), "198.18.0.10".parse().unwrap())]);
        assert_eq!(
            peers,
            vec![
                Peer {
                    public_key: "k-a".into(),
                    endpoint: Some("[2001:db8::5]:51820".into()),
                    allowed_ips: vec!["198.19.0.10/32".into(), "198.18.0.10/32".into()],
                },
                Peer {
                    public_key: "k-b".into(),
                    endpoint: None,
                    allowed_ips: vec!["198.19.0.11/32".into()],
                },
            ],
            "a host without a key yet is left out"
        );
    }

    #[test]
    fn mesh_peers_are_the_other_aws_hosts_on_their_vpc_addresses() {
        let hosts = [
            host("a", "198.19.0.10", Some("k-a"), None, Some("172.31.0.5")),
            host("b", "198.19.0.11", Some("k-b"), None, Some("172.31.0.6")),
            host("c", "198.19.0.12", Some("k-c"), None, None),
        ];
        assert_eq!(
            mesh_peers(&hosts[0], &hosts),
            vec![Peer {
                public_key: "k-b".into(),
                endpoint: Some("172.31.0.6:51820".into()),
                allowed_ips: vec!["198.19.0.11/32".into()],
            }]
        );
    }

    #[test]
    fn egress_targets_are_the_ready_ipv4_endpoints() {
        let ep = |a: &str, ready: Option<bool>, terminating: Option<bool>| Endpoint {
            addresses: vec![a.into()],
            conditions: Some(EndpointConditions {
                ready,
                serving: None,
                terminating,
            }),
            ..Endpoint::default()
        };
        let slice = |t: &str, eps: Vec<Endpoint>| EndpointSlice {
            address_type: t.into(),
            endpoints: Some(eps),
            ..EndpointSlice::default()
        };
        let s = [
            slice(
                "IPv4",
                vec![
                    ep("10.244.1.5", Some(true), None),
                    ep("10.244.2.7", Some(false), None),
                    ep("10.244.3.9", Some(true), Some(true)),
                    ep("10.244.0.4", None, None),
                ],
            ),
            slice("IPv6", vec![ep("fd00::1", Some(true), None)]),
            slice("IPv4", vec![ep("10.244.1.5", Some(true), None)]),
        ];
        assert_eq!(ready_ipv4(&s), vec!["10.244.0.4", "10.244.1.5"]);
    }
}
