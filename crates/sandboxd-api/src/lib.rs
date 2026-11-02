//! sandboxd's REST types, shared by sandboxd, hermit's client and fake-sandboxd.
//!
//! sandboxd (crate `sandboxd`, crates/sandboxd/README.md) is the agent on every sandbox host and on the gateway. It
//! serves this API over HTTPS on the host's overlay address, port [`PORT`]: TLS 1.3 only, mutual TLS, and only a
//! client certificate whose CN is [`CLIENT_CN`] is served (others get 403). sandboxd itself writes and parses its JSON
//! by hand (no heap after start, crates/sandboxd/docs/MEMORY.md); these types are the same wire shapes, checked both
//! ways by the golden fixtures in `fixtures/` (round-tripped here, and parsed and written by sandboxd's own tests) and
//! by `tests/live.rs` against the answers of a running sandboxd (the soak job).
//!
//! | Route | Request | Answer |
//! |---|---|---|
//! | `GET /v1/host` | — | [`Host`] |
//! | `PUT /v1/peers` | [`SetPeers`] | [`Peers`] |
//! | `PUT /v1/egress` (hub only) | [`EgressTargets`] | [`EgressTargets`] |
//! | `POST /v1/images` | [`EnsureImage`] | [`ImageDigest`] |
//! | `GET /v1/sandboxes` | — | [`Sandboxes`] |
//! | `POST /v1/sandboxes` | [`Spec`] | [`Listed`] (idempotent for the same name and ip) |
//! | `DELETE /v1/sandboxes/{name}` | — | [`Deleted`] (a missing one is fine) |
//! | `GET /v1/sandboxes/{name}/spec` | — | [`Spec`] |
//! | `POST /v1/sandboxes/{name}/receive` | [`Spec`] | [`Receiving`] (migration target: prepare and listen) |
//! | `GET /v1/sandboxes/{name}/receive?wait=<ms>` | — | [`ReceiveResult`] |
//! | `POST /v1/sandboxes/{name}/send` | [`SendRequest`] | [`SendResult`] (409: not moved, still running here) |
//!
//! **Answers only gain fields (expand-only).** A host runs the sandboxd of its AMI for hours while hermit rolls forward
//! and back, so a client must read the answers of a newer sandboxd: the answer types ignore fields they do not know,
//! every field added to an answer is an `Option` or has a serde default, and an enum that gains a value gains it with
//! the `Unknown` catch-all already in place ([`SandboxState::Unknown`], [`ReceiveResult::Unknown`]). Exact shapes are
//! checked by the tests instead (the fixtures round-trip, and `tests/live.rs` checks a real sandboxd's answers key for
//! key). Removing or retyping a field is a contract change (docs/CONTRACTS.md).
//!
//! Every error is [`ErrorBody`] `{"error": "<message>"}` with its status: 400 bad request, 403 not the controller,
//! 404, 409 conflict, 411 chunked body, 413 body too large, 500, 502 lost in a migration's stopped part, 503 a fixed
//! table full or the shared filesystem not mounted.

mod client;
mod server;

use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use client::{
    CONTROL_SLOTS, CREATE_TIMEOUT, Client, Error, HEARTBEAT_SLOTS, HOST_TIMEOUT, IMAGE_TIMEOUT, LONG_SLOTS,
    MAX_CONNECTIONS, POOL_IDLE, RECEIVE_TIMEOUT, RECEIVE_WAIT_MS, SEND_TIMEOUT, TIMEOUT, Tls,
};
pub use server::{Answer, ApiError, Server, router};

/// sandboxd's API port on the overlay address.
pub const PORT: u16 = 7443;
/// The hub's image server port (plain HTTP, overlay hosts only): `GET /images/<digest>.tar.zst`.
pub const IMAGE_PORT: u16 = 7444;
/// The name in every sandboxd server certificate (the client connects by address and checks this name).
pub const SERVER_NAME: &str = "sandboxd";
/// The only client certificate CN sandboxd serves.
pub const CLIENT_CN: &str = "hermit";
/// The largest request body sandboxd accepts by default (host.json `limits.maxBodyKiB`); larger is 413.
pub const MAX_BODY: usize = 64 << 10;
/// The longest `wait` of `GET …/receive` (larger values are clamped).
pub const RECEIVE_WAIT_MAX_MS: u64 = 120_000;

/// The route table (servers and clients use these, so they cannot drift).
pub mod routes {
    /// `GET`: [`Host`](crate::Host).
    pub const HOST: &str = "/v1/host";
    /// `PUT`: [`SetPeers`](crate::SetPeers) → [`Peers`](crate::Peers).
    pub const PEERS: &str = "/v1/peers";
    /// `PUT`: [`EgressTargets`](crate::EgressTargets) both ways.
    pub const EGRESS: &str = "/v1/egress";
    /// `POST`: [`EnsureImage`](crate::EnsureImage) → [`ImageDigest`](crate::ImageDigest).
    pub const IMAGES: &str = "/v1/images";
    /// `GET`: [`Sandboxes`](crate::Sandboxes); `POST`: [`Spec`](crate::Spec) → [`Listed`](crate::Listed).
    pub const SANDBOXES: &str = "/v1/sandboxes";
    /// The axum path of one sandbox (`DELETE`).
    pub const SANDBOX: &str = "/v1/sandboxes/{name}";
    /// The axum path of `GET …/spec`.
    pub const SANDBOX_SPEC: &str = "/v1/sandboxes/{name}/spec";
    /// The axum path of `POST`/`GET …/receive`.
    pub const SANDBOX_RECEIVE: &str = "/v1/sandboxes/{name}/receive";
    /// The axum path of `POST …/send`.
    pub const SANDBOX_SEND: &str = "/v1/sandboxes/{name}/send";

    /// `/v1/sandboxes/<name>`.
    #[must_use]
    pub fn sandbox(name: &str) -> String {
        format!("{SANDBOXES}/{name}")
    }

    /// `/v1/sandboxes/<name>/<what>` (`spec`, `receive`, `send`).
    #[must_use]
    pub fn sandbox_part(name: &str, what: &str) -> String {
        format!("{SANDBOXES}/{name}/{what}")
    }
}

/// Is this a sandbox name sandboxd accepts: `sbx-` and 1 to 40 lowercase letters or digits.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    name.len() > 4
        && name.len() <= 44
        && name.starts_with("sbx-")
        && name[4..].bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// A host's role in host.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The `WireGuard` hub (a gateway when [`Host::gateway`]).
    Hub,
    /// A host that peers the hub (AWS).
    Burst,
}

/// `GET /v1/host`: the host's facts, capacity, network and sandboxes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    /// host.json `name`.
    pub name: String,
    /// hub or burst.
    pub role: Role,
    /// A gateway runs no sandboxes (hub only).
    pub gateway: bool,
    /// The overlay (wg0) address.
    pub wg_address: String,
    /// wg0's public key.
    pub wg_public_key: String,
    /// wg0's UDP port.
    pub wg_port: u16,
    /// The public interface's global IPv6 address, if any.
    pub public_ipv6: Option<String>,
    /// wg0's peers as the kernel has them.
    pub peers: Vec<PeerStatus>,
    /// Hub: the egress addresses behind the egress VIP.
    pub egress_targets: Vec<String>,
    /// `/proc/meminfo` `MemTotal`, bytes.
    pub mem_total: u64,
    /// `/proc/meminfo` `MemAvailable`, bytes.
    pub mem_available: u64,
    /// Memory kept for the host itself (a gateway reports all of it: no room for sandboxes), bytes.
    pub reserve_memory: u64,
    /// Online CPUs.
    pub cpus: u64,
    /// The 1, 5 and 15 minute load averages.
    pub load_avg: Vec<f64>,
    /// Seconds since boot.
    pub uptime_s: f64,
    /// `runsc cpu-features` (empty on a gateway).
    pub cpu_features: String,
    /// `runsc --version`'s first line (empty on a gateway).
    pub runsc: String,
    /// The installed runsc fork release (its `.release` file).
    pub gvisor_release: String,
    /// Unpacked image digests (64 hex).
    pub images: Vec<String>,
    /// The kernel's boot id.
    pub boot_id: String,
    /// When this sandboxd started (ISO 8601, ms).
    pub started_at: String,
    /// Whether the shared filesystem is mounted; null on a host that has none (the gateway).
    pub jfs_mounted: Option<bool>,
    /// The sandboxes here.
    pub sandboxes: Vec<Listed>,
    /// sandboxd's own facts.
    pub sandboxd: Daemon,
}

/// `Host.sandboxd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Daemon {
    /// `rust`.
    pub implementation: String,
    /// The crate version.
    pub version: String,
    /// Its memory budget and use (crates/sandboxd/docs/MEMORY.md).
    pub memory: DaemonMemory,
    /// The fixed capacities it was started with.
    pub limits: DaemonLimits,
}

/// `Host.sandboxd.memory`: the budget and what is used of it, bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct DaemonMemory {
    pub budget: u64,
    pub rss: u64,
    pub boot_arena: u64,
    pub boot_used: u64,
    pub boot_peak: u64,
    pub tls_arenas: u64,
    pub tls_arena_size: u64,
    pub tls_used: u64,
    pub tls_peak: u64,
    pub api_stack: u64,
    pub api_stack_peak: u64,
    pub other_stack: u64,
    pub other_stack_peak: u64,
    /// Allocations outside TLS after start (must stay 0).
    pub stray: u64,
    /// Allocations a worker arena could not serve, served from the boot arena.
    pub overflow: u64,
    /// Allocations refused (null).
    pub failed: u64,
}

/// `Host.sandboxd.limits`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonLimits {
    /// TLS connections served at once.
    pub max_connections: u64,
    /// Sandboxes on this host.
    pub max_sandboxes: u64,
    /// Migrations in flight, each direction counted.
    pub max_migrations: u64,
}

/// One of wg0's peers as the kernel has it (`wg show wg0 dump`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStatus {
    /// The peer's public key.
    pub public_key: String,
    /// `host:port`, or `(none)` before the peer's first handshake.
    pub endpoint: String,
    /// The peer's allowed addresses (CIDRs).
    pub allowed_ips: Vec<String>,
    /// Unix seconds of the latest handshake, 0 for none.
    pub latest_handshake: u64,
}

/// One wanted peer in `PUT /v1/peers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    /// The peer's public key.
    pub public_key: String,
    /// `host:port`; none for peers that connect to this host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// CIDRs routed to the peer (entries that are not CIDRs are ignored by sandboxd).
    #[serde(default)]
    pub allowed_ips: Vec<String>,
}

/// `PUT /v1/peers`: wg0's peers, exactly these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetPeers {
    /// The wanted peers.
    pub peers: Vec<Peer>,
}

/// The answer of `PUT /v1/peers`: wg0's peers afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peers {
    /// wg0's peers.
    pub peers: Vec<PeerStatus>,
}

/// `PUT /v1/egress` and its answer (hub only): the egress addresses behind the egress VIP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressTargets {
    /// IPv4 addresses.
    pub targets: Vec<String>,
}

/// `POST /v1/images`: make sure an image is unpacked here (by digest from the hub's image server, else by `ref`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EnsureImage {
    /// The image reference to pull.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The manifest digest (64 hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// The answer of `POST /v1/images`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDigest {
    /// The unpacked image's digest (64 hex).
    pub digest: String,
}

/// A sandbox's environment, in the order given (sandboxd keeps it: the bundle's `process.env` follows it). Its values
/// hold the sandbox's credentials (the proxy credential, tokens), so `Debug` shows the keys only.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Env(pub Vec<(String, String)>);

impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Env")
            .field(&self.0.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>())
            .finish()
    }
}

impl Serialize for Env {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Env {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Env;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of strings")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Env, E> {
                Ok(Env::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Env, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = a.next_entry::<String, String>()? {
                    out.push((k, v));
                }
                Ok(Env(out))
            }
        }
        d.deserialize_any(V)
    }
}

/// A sandbox's spec: `POST /v1/sandboxes`, `POST …/receive`, `GET …/spec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    /// `sbx-<lowercase hex/digits>` ([`valid_name`]): the runsc container id and netns name.
    pub name: String,
    /// Its fixed address in the sandbox range.
    pub ip: String,
    /// The image's manifest digest (64 hex).
    pub image: String,
    /// The image reference it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_ref: Option<String>,
    /// Environment of the first process (at most 128 variables, 16 KiB).
    #[serde(default)]
    pub env: Env,
    /// CPUs, in (0, 64].
    pub cpu: f64,
    /// Memory limit, bytes, at least 128 MiB.
    pub memory: u64,
    /// The CPU feature mask (runsc annotation); sandboxd always answers it, empty when none.
    #[serde(default)]
    pub cpu_features: String,
    /// Folder `ws/<workspace>` of the host's shared filesystem, mounted at `/workspace`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Folder `users/<user>/claude` of the shared filesystem, mounted at `~/.claude`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// A sandbox's state on a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxState {
    /// Created or restored.
    Running,
    /// A migration target waiting for its sandbox.
    Receiving,
    /// A state this version does not know (a newer sandboxd).
    #[serde(other)]
    Unknown,
}

/// One sandbox in a list, and the answer of a create.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listed {
    /// Its name.
    pub name: String,
    /// Its address.
    pub ip: String,
    /// running or receiving.
    pub state: SandboxState,
    /// When it was created here (ISO 8601, ms).
    pub created_at: String,
    /// CPUs of its spec.
    pub cpu: f64,
    /// Memory limit of its spec, bytes.
    pub memory: u64,
    /// CPU use over the last 10 s sample, in CPUs (3 decimals).
    pub cpu_usage: f64,
    /// Its cgroup's `memory.current`, bytes.
    pub memory_current: u64,
}

/// `GET /v1/sandboxes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sandboxes {
    /// The sandboxes on this host.
    pub sandboxes: Vec<Listed>,
}

/// The answer of `DELETE /v1/sandboxes/{name}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deleted {
    /// The name.
    pub deleted: String,
}

/// The answer of `POST …/receive`: where the source sends to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receiving {
    /// The migration relay's port.
    pub port: u16,
    /// The target's overlay address.
    pub address: String,
}

/// The answer of `GET …/receive?wait=<ms>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ReceiveResult {
    /// Restored and running here.
    Running {
        /// runsc's restore statistics (its own JSON line), when it wrote them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stats: Option<serde_json::Value>,
    },
    /// Still waiting for the sandbox.
    Receiving,
    /// The receive failed or the sandbox is not here.
    Failed {
        /// Why (`not here` when nothing is known).
        error: String,
    },
    /// A state this version does not know (a newer sandboxd): ask again.
    #[serde(other)]
    Unknown,
}

/// `POST …/send`: move the sandbox to a receive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    /// `<overlay address>:<port>` of the target's [`Receiving`].
    pub to: String,
    /// Target pause of the final round, ms (default 100, 10 to 5000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downtime_ms: Option<u64>,
    /// Pre-copy rounds at most (default 20, 1 to 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u64>,
    /// Give up (409, still running here) when the pause would exceed this, ms (0 = no limit, at most 60000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_downtime_ms: Option<u64>,
}

/// The answer of `POST …/send`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendResult {
    /// The whole send, ms.
    pub ms: u64,
    /// The scratch filesystem's copy.
    pub scratch: ScratchStats,
}

/// `SendResult.scratch`: the scratch copy's rounds while running and its final delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct ScratchStats {
    pub live_rounds: u64,
    pub live_files: u64,
    pub live_bytes: u64,
    pub live_ms: u64,
    pub final_files: u64,
    pub final_bytes: u64,
    pub final_deleted: u64,
    pub final_ms: u64,
    /// From the end of the memory stream to the target's ack: what the scratch adds to the pause.
    pub stopped_ms: u64,
}

/// Every error answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The message.
    pub error: String,
}
