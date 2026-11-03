//! One fabric host: sandboxd's API with each sandbox a Pod of the sandbox image (the crate docs).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use kube::api::{DeleteParams, ListParams, LogParams, PostParams};
use sandboxd_api::{Answer, ApiError, Server};
use sandboxd_api::{
    Daemon, DaemonLimits, DaemonMemory, Deleted, EgressTargets, EnsureImage, Host, ImageDigest, Listed, PeerStatus,
    Peers, ReceiveResult, Receiving, Role, SandboxState, Sandboxes, SendRequest, SendResult, SetPeers, Spec,
};
use serde::Deserialize;

use crate::{HOST_LABEL, IP_ANNOTATION, iso, iso_now};

/// The spec a sandbox Pod was made from (JSON, env included: the harness's throwaway credentials).
const SPEC_ANNOTATION: &str = "fake-sandboxd/spec";
/// How long a create waits for its Pod to run.
const CREATE_WAIT: Duration = Duration::from_secs(240);
/// How long a delete waits for its Pod to go (the keeper's 20 s drain inside the 25 s grace).
const DELETE_WAIT: Duration = Duration::from_secs(45);

/// A host's config: the user data hermit gives a new instance (hermit `scaler::user_data`), or the gateway's file in
/// the same shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostFile {
    pub host: HostPart,
    pub wg_private_key: String,
    pub tls: TlsPart,
}

/// `host` of [`HostFile`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPart {
    pub name: String,
    pub role: Role,
    #[serde(default)]
    pub gateway: bool,
    pub wg_address: String,
    #[serde(default)]
    pub reserve_memory: u64,
}

/// `tls` of [`HostFile`]: the CA (of hermit's client certificate too) and the host's server certificate.
#[derive(Clone, Deserialize)]
pub struct TlsPart {
    pub ca: String,
    pub cert: String,
    pub key: String,
}

impl std::fmt::Debug for TlsPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsPart(..)")
    }
}

/// What a host reports about itself beyond its config (hermit.json's expectations of a host).
#[derive(Debug, Clone)]
pub struct Facts {
    pub namespace: String,
    /// The node path of the `JuiceFS` mount (`ws/<id>` below it is a sandbox's `/workspace`).
    pub jfs_root: String,
    pub cpus: u64,
    pub mem_total: u64,
    pub cpu_features: String,
    pub runsc: String,
    pub gvisor_release: String,
    pub version: String,
}

/// The host.
pub struct HostSim {
    file: HostFile,
    facts: Facts,
    pods: Api<Pod>,
    public_key: String,
    boot_id: String,
    started: Instant,
    started_at: String,
    peers: Mutex<Vec<PeerStatus>>,
    egress: Mutex<Vec<String>>,
    images: Mutex<Vec<String>>,
    /// Serialises creates and deletes (a create checks the address it takes is free).
    lifecycle: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for HostSim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSim")
            .field("name", &self.file.host.name)
            .finish_non_exhaustive()
    }
}

/// The `WireGuard` public key of a base64 private key.
///
/// # Errors
/// A key that is not 32 bytes of base64.
pub fn public_key(private_b64: &str) -> anyhow::Result<String> {
    let sk: [u8; 32] = STANDARD
        .decode(private_b64.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("a WireGuard key is 32 bytes"))?;
    Ok(STANDARD.encode(x25519_dalek::x25519(sk, x25519_dalek::X25519_BASEPOINT_BYTES)))
}

fn random_hex(n: usize) -> String {
    use ring::rand::SecureRandom as _;
    let mut b = vec![0u8; n];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b.iter().fold(String::new(), |mut s, x| {
        use std::fmt::Write as _;
        let _ = write!(s, "{x:02x}");
        s
    })
}

fn kube_err(what: &str, e: &kube::Error) -> ApiError {
    ApiError::new(500, format!("{what}: {e}"))
}

/// The pod image of a spec: its reference without the digest (the harness imports images by tag).
fn pod_image(spec: &Spec) -> Answer<String> {
    let r = spec
        .image_ref
        .as_deref()
        .ok_or_else(|| ApiError::new(400, "the harness runs images by reference: imageRef missing"))?;
    Ok(r.split_once('@').map_or(r, |(name, _)| name).to_owned())
}

impl HostSim {
    /// A host from its file.
    ///
    /// # Errors
    /// A bad `WireGuard` key.
    pub fn new(file: HostFile, facts: Facts, client: kube::Client) -> anyhow::Result<Self> {
        let public_key = public_key(&file.wg_private_key)?;
        Ok(Self {
            pods: Api::namespaced(client, &facts.namespace),
            file,
            facts,
            public_key,
            boot_id: random_hex(16),
            started: Instant::now(),
            started_at: iso_now(),
            peers: Mutex::default(),
            egress: Mutex::default(),
            images: Mutex::default(),
            lifecycle: tokio::sync::Mutex::new(()),
        })
    }

    /// The host's server identity: its certificate, and hermit's client certificate required (CA of the fabric).
    #[must_use]
    pub fn identity(&self) -> crate::tls::Identity {
        crate::tls::Identity {
            cert_pem: self.file.tls.cert.clone(),
            key_pem: self.file.tls.key.clone(),
            client_ca_pem: Some(self.file.tls.ca.clone()),
        }
    }

    fn gateway(&self) -> bool {
        self.file.host.gateway
    }

    fn selector(&self) -> String {
        format!("{HOST_LABEL}={},app=sandbox", self.file.host.name)
    }

    async fn sandbox_pods(&self) -> Answer<Vec<Pod>> {
        if self.gateway() {
            return Ok(Vec::new());
        }
        let l = self
            .pods
            .list(&ListParams::default().labels(&self.selector()))
            .await
            .map_err(|e| kube_err("list pods", &e))?;
        Ok(l.items
            .into_iter()
            .filter(|p| p.metadata.deletion_timestamp.is_none())
            .collect())
    }

    async fn listed(&self) -> Answer<Vec<Listed>> {
        Ok(self.sandbox_pods().await?.iter().filter_map(listed).collect())
    }

    fn pod(&self, spec: &Spec) -> Answer<Pod> {
        let workspace = spec
            .workspace
            .as_deref()
            .ok_or_else(|| ApiError::new(400, "a sandbox needs a workspace"))?;
        let env: Vec<serde_json::Value> = spec
            .env
            .0
            .iter()
            .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
            .collect();
        let spec_json = serde_json::to_string(spec).map_err(|e| ApiError::new(500, e.to_string()))?;
        let pod = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": spec.name,
                "labels": {"app": "sandbox", "hermit/sandbox": spec.name, HOST_LABEL: self.file.host.name},
                "annotations": {IP_ANNOTATION: format!("[\"{}\"]", spec.ip), SPEC_ANNOTATION: spec_json},
            },
            "spec": {
                "hostname": "sandbox",
                "restartPolicy": "Never",
                "terminationGracePeriodSeconds": 25,
                "enableServiceLinks": false,
                "automountServiceAccountToken": false,
                // a sandbox has no DNS (hermit 5.1): every name goes through the gateway's proxy
                "dnsPolicy": "None",
                "dnsConfig": {"nameservers": ["127.0.0.1"]},
                "containers": [{
                    "name": "sandbox",
                    "image": pod_image(spec)?,
                    "imagePullPolicy": "IfNotPresent",
                    "env": env,
                    "securityContext": {"privileged": true},
                    "resources": {"limits": {
                        "memory": spec.memory.to_string(),
                        "cpu": format!("{}m", (spec.cpu * 1000.0).round()),
                    }},
                    "volumeMounts": [{"name": "workspace", "mountPath": "/workspace"}],
                }],
                "volumes": [{"name": "workspace", "hostPath": {
                    "path": format!("{}/ws/{workspace}", self.facts.jfs_root),
                    "type": "Directory",
                }}],
            },
        });
        serde_json::from_value(pod).map_err(|e| ApiError::new(500, format!("pod: {e}")))
    }

    async fn wait_running(&self, name: &str) -> Answer<Pod> {
        let until = Instant::now() + CREATE_WAIT;
        loop {
            let p = self.pods.get_opt(name).await.map_err(|e| kube_err("get pod", &e))?;
            let Some(p) = p else {
                return Err(ApiError::new(500, format!("{name}: the pod went away while starting")));
            };
            let phase = p.status.as_ref().and_then(|s| s.phase.clone()).unwrap_or_default();
            match phase.as_str() {
                "Running" => return Ok(p),
                "Failed" | "Succeeded" => {
                    let _ = self.pods.delete(name, &DeleteParams::default()).await;
                    return Err(ApiError::new(
                        500,
                        format!("{name}: the sandbox exited while starting ({phase})"),
                    ));
                }
                _ => {}
            }
            if Instant::now() > until {
                let _ = self.pods.delete(name, &DeleteParams::default()).await;
                return Err(ApiError::new(500, format!("{name}: not running after {CREATE_WAIT:?}")));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn wait_gone(&self, name: &str) -> Answer<()> {
        let until = Instant::now() + DELETE_WAIT;
        while self
            .pods
            .get_opt(name)
            .await
            .map_err(|e| kube_err("get pod", &e))?
            .is_some()
        {
            if Instant::now() > until {
                return Err(ApiError::new(500, format!("{name}: still there after {DELETE_WAIT:?}")));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Ok(())
    }
}

/// A sandbox Pod as sandboxd lists it.
fn listed(p: &Pod) -> Option<Listed> {
    let spec: Spec = serde_json::from_str(p.metadata.annotations.as_ref()?.get(SPEC_ANNOTATION)?).ok()?;
    let status = p.status.as_ref();
    let phase = status.and_then(|s| s.phase.as_deref()).unwrap_or_default();
    let running = status
        .and_then(|s| s.container_statuses.as_ref())
        .and_then(|c| c.first())
        .and_then(|c| c.state.as_ref())
        .is_some_and(|s| s.running.is_some());
    let created_at = p
        .metadata
        .creation_timestamp
        .as_ref()
        .and_then(|t| time::OffsetDateTime::from_unix_timestamp(t.0.as_second()).ok())
        .map_or_else(iso_now, iso);
    Some(Listed {
        name: spec.name.clone(),
        ip: spec.ip.clone(),
        state: SandboxState::Running,
        created_at,
        cpu: spec.cpu,
        memory: spec.memory,
        cpu_usage: 0.0,
        memory_current: 0,
        alive: Some(phase == "Running" && running),
        sending: false,
        scratch_used: Some(0),
    })
}

// the trait's methods are futures; the ones answering from memory have nothing to await
#[allow(clippy::unused_async_trait_impl)]
impl Server for HostSim {
    async fn host(&self) -> Answer<Host> {
        let sandboxes = self.listed().await?;
        let used: u64 = sandboxes.iter().map(|s| s.memory).sum();
        let reserve = if self.gateway() {
            self.facts.mem_total
        } else {
            self.file.host.reserve_memory
        };
        Ok(Host {
            name: self.file.host.name.clone(),
            role: self.file.host.role,
            gateway: self.gateway(),
            wg_address: self.file.host.wg_address.clone(),
            wg_public_key: self.public_key.clone(),
            wg_port: 51821,
            public_ipv6: None,
            peers: self.peers.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            egress_targets: self.egress.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            mem_total: self.facts.mem_total,
            mem_available: self.facts.mem_total.saturating_sub(used),
            reserve_memory: reserve,
            cpus: self.facts.cpus,
            load_avg: vec![0.0, 0.0, 0.0],
            uptime_s: self.started.elapsed().as_secs_f64(),
            cpu_features: if self.gateway() {
                String::new()
            } else {
                self.facts.cpu_features.clone()
            },
            runsc: if self.gateway() {
                String::new()
            } else {
                self.facts.runsc.clone()
            },
            gvisor_release: self.facts.gvisor_release.clone(),
            images: self.images.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            boot_id: self.boot_id.clone(),
            started_at: self.started_at.clone(),
            jfs_mounted: (!self.gateway()).then_some(true),
            sandboxes,
            sandboxd: Daemon {
                implementation: "fake-sandboxd".into(),
                version: self.facts.version.clone(),
                memory: DaemonMemory {
                    budget: 64 << 20,
                    rss: 8 << 20,
                    boot_arena: 0,
                    boot_used: 0,
                    boot_peak: 0,
                    tls_arenas: 0,
                    tls_arena_size: 0,
                    tls_used: 0,
                    tls_peak: 0,
                    api_stack: 0,
                    api_stack_peak: 0,
                    other_stack: 0,
                    other_stack_peak: 0,
                    stray: 0,
                    overflow: 0,
                    failed: 0,
                },
                limits: DaemonLimits {
                    max_connections: 64,
                    max_sandboxes: 64,
                    max_migrations: 0,
                },
            },
        })
    }

    async fn set_peers(&self, req: SetPeers) -> Answer<Peers> {
        let peers: Vec<PeerStatus> = req
            .peers
            .into_iter()
            .map(|p| PeerStatus {
                public_key: p.public_key,
                endpoint: p.endpoint.unwrap_or_default(),
                allowed_ips: p.allowed_ips,
                latest_handshake: 0,
            })
            .collect();
        eprintln!("fake-sandboxd: peers set ({})", peers.len());
        peers.clone_into(&mut self.peers.lock().unwrap_or_else(PoisonError::into_inner));
        Ok(Peers { peers })
    }

    async fn set_egress(&self, req: EgressTargets) -> Answer<EgressTargets> {
        if !self.gateway() {
            return Err(ApiError::new(409, "egress targets are the gateway's"));
        }
        eprintln!("fake-sandboxd: egress targets {:?}", req.targets);
        req.targets
            .clone_into(&mut self.egress.lock().unwrap_or_else(PoisonError::into_inner));
        Ok(req)
    }

    async fn ensure_image(&self, req: EnsureImage) -> Answer<ImageDigest> {
        // the harness imports the image into the cluster beforehand: nothing to pull
        let d = req
            .digest
            .ok_or_else(|| ApiError::new(400, "the harness ensures images by digest"))?;
        let mut images = self.images.lock().unwrap_or_else(PoisonError::into_inner);
        if !images.contains(&d) {
            images.push(d.clone());
        }
        Ok(ImageDigest { digest: d })
    }

    async fn list(&self) -> Answer<Sandboxes> {
        Ok(Sandboxes {
            sandboxes: self.listed().await?,
        })
    }

    async fn create(&self, spec: Spec) -> Answer<Listed> {
        if self.gateway() {
            return Err(ApiError::new(409, "a gateway runs no sandboxes"));
        }
        let _guard = self.lifecycle.lock().await;
        let pods = self.sandbox_pods().await?;
        if let Some(other) = pods
            .iter()
            .filter_map(listed)
            .find(|l| l.ip == spec.ip && l.name != spec.name)
        {
            return Err(ApiError::new(409, format!("ip {} is used by {}", spec.ip, other.name)));
        }
        if self
            .pods
            .get_opt(&spec.name)
            .await
            .map_err(|e| kube_err("get pod", &e))?
            .is_none()
        {
            let pod = self.pod(&spec)?;
            self.pods
                .create(&PostParams::default(), &pod)
                .await
                .map_err(|e| kube_err("create pod", &e))?;
            eprintln!("fake-sandboxd: {} created at {}", spec.name, spec.ip);
        }
        let p = self.wait_running(&spec.name).await?;
        listed(&p).ok_or_else(|| ApiError::new(500, "the pod lost its spec"))
    }

    async fn delete(&self, name: String) -> Answer<Deleted> {
        if self.gateway() {
            return Err(ApiError::new(409, "a gateway runs no sandboxes"));
        }
        let _guard = self.lifecycle.lock().await;
        let dp = DeleteParams {
            grace_period_seconds: Some(25),
            ..DeleteParams::default()
        };
        match self.pods.delete(&name, &dp).await {
            Ok(_) => {}
            Err(kube::Error::Api(s)) if s.code == 404 => {}
            Err(e) => return Err(kube_err("delete pod", &e)),
        }
        self.wait_gone(&name).await?;
        eprintln!("fake-sandboxd: {name} deleted");
        Ok(Deleted { deleted: name })
    }

    async fn spec(&self, name: String) -> Answer<Spec> {
        if self.gateway() {
            return Err(ApiError::new(409, "a gateway runs no sandboxes"));
        }
        let p = self.pods.get_opt(&name).await.map_err(|e| kube_err("get pod", &e))?;
        p.as_ref()
            .and_then(|p| p.metadata.annotations.as_ref()?.get(SPEC_ANNOTATION).cloned())
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or_else(|| ApiError::new(404, format!("{name} is not here")))
    }

    async fn receive(&self, _spec: Spec) -> Answer<Receiving> {
        Err(ApiError::new(501, "not_supported"))
    }

    async fn receive_result(&self, _name: String, _wait_ms: u64) -> Answer<ReceiveResult> {
        Err(ApiError::new(501, "not_supported"))
    }

    async fn send(&self, _name: String, _req: SendRequest) -> Answer<SendResult> {
        Err(ApiError::new(501, "not_supported"))
    }

    async fn log(&self, name: String, tail: usize) -> Answer<Vec<u8>> {
        let lp = LogParams {
            limit_bytes: Some(i64::try_from(tail).unwrap_or(i64::MAX)),
            ..LogParams::default()
        };
        match self.pods.logs(&name, &lp).await {
            Ok(s) => {
                let b = s.into_bytes();
                Ok(b[b.len().saturating_sub(tail)..].to_vec())
            }
            Err(_) => Err(ApiError::new(404, format!("{name} has no console here"))),
        }
    }
}

/// Every label of the host Pods of `ec2` (for its listing).
#[must_use]
pub fn host_pod_labels(name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app".to_owned(), "fake-host".to_owned()),
        (HOST_LABEL.to_owned(), format!("host-{name}")),
    ])
}

/// Serves host `file` until the process ends.
///
/// # Errors
/// No cluster credentials, a bad config or a port in use.
pub async fn run(file: HostFile, facts: Facts, listen: std::net::SocketAddr) -> anyhow::Result<()> {
    let client = kube::Client::try_default().await?;
    let sim = Arc::new(HostSim::new(file, facts, client)?);
    let id = sim.identity();
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!("fake-sandboxd: host {} on {listen}", sim.file.host.name);
    crate::tls::serve(listener, sandboxd_api::router(sim), &id).await
}
