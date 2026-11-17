//! The host configuration (/etc/sbx/host.json, mode 600) and the sandbox spec the controller sends
//! (docs/HOST.md, "sandboxd"). Same fields as the Node sandboxd; `limits` is new (docs/MEMORY.md).
use crate::err;
use crate::fixed::Str;
use crate::json::{self, JStr, Val, W};
use crate::util::{Error, Res, in_cidr};

#[derive(Clone, Copy, Default)]
pub struct Peer {
    pub public_key: Str<64>,
    /// host:port; empty for peers that connect to us
    pub endpoint: Str<64>,
    /// comma-separated a.b.c.d/n
    pub allowed: Str<1024>,
}

/// Fixed capacities: every table and buffer in sandboxd is sized from these at start, never grown.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// API worker threads = TLS connections served at once (more wait in the kernel's accept queue)
    pub max_connections: usize,
    /// sandboxes on this host (state entries)
    pub max_sandboxes: usize,
    /// migrations in flight on this host, each direction counted (receives + sends)
    pub max_migrations: usize,
    /// heap arena per API worker, for rustls' per-connection state
    pub tls_arena: usize,
    /// largest request body
    pub max_body: usize,
    /// the image server's connections at once (role hub)
    pub image_connections: usize,
    /// cgroup memory.max for the commands sandboxd runs (runsc CLI, ip, nft, crane, tar...)
    pub helper_memory_max: u64,
    /// put sandboxd and its commands into their own cgroups below the unit's (needs Delegate=yes)
    pub cgroups: bool,
    /// mlockall after start (a failure stops sandboxd unless false)
    pub mlock: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_connections: 16,
            max_sandboxes: 24,
            max_migrations: 4,
            tls_arena: 512 << 10,
            max_body: 64 << 10,
            image_connections: 2,
            helper_memory_max: 384 << 20,
            cgroups: true,
            mlock: true,
        }
    }
}

pub struct HostConfig {
    pub name: Str<64>,
    pub hub: bool,
    /// hub only: a gateway on a shared machine (a k3s node), no sandboxes: the WireGuard hub, egress address
    /// translation and image source only; host rules that touch nothing but wg0's traffic; no runsc
    pub gateway: bool,
    pub wg_address: Str<16>,
    pub host_cidr: Str<20>,
    pub wg_port: u16,
    pub wg_key_file: Str<256>,
    pub api_port: u16,
    pub image_port: u16,
    pub tls_cert: Str<256>,
    pub tls_key: Str<256>,
    pub tls_ca: Str<256>,
    pub control_cidr: Str<20>,
    pub sandbox_cidr: Str<20>,
    pub egress_vip: Str<16>,
    pub pod_cidr: Str<20>,
    pub private_gateway: Str<16>,
    pub hub_peer: Option<Peer>,
    pub reserve_memory: u64,
    pub registry_config_dir: Str<256>,
    pub image_source: Str<256>,
    pub mtu: u32,
    /// gateway: the MSS of TCP SYNs forwarded from wg0 is clamped to this (`gatewayMss`, default 1280: a 1320-byte
    /// pod MTU minus 40), so a sandbox's full-size segment never meets a smaller hop behind the gateway
    pub gateway_mss: u32,
    /// burst: the host's sandboxes also reach the internet straight from this host's public address
    /// (`directEgress`, default false: the proxy at the egress VIP is a sandbox's only way out); docs/HOST.md
    /// "Direct egress"
    pub direct_egress: bool,
    pub limits: Limits,
}

fn s<const N: usize>(v: Val, what: &str) -> Res<Str<N>> {
    let js = v.str().ok_or_else(|| err!(400, "host.json: {what} must be a string"))?;
    let mut tmp = [0u8; 4096];
    let d = js.decode(&mut tmp).ok_or_else(|| err!(400, "host.json: {what} is too long"))?;
    Str::from(d).ok_or_else(|| err!(400, "host.json: {what} is too long"))
}

fn n(v: Val, what: &str) -> Res<f64> {
    v.num().ok_or_else(|| err!(400, "host.json: {what} must be a number"))
}

pub fn parse_peer(o: &[u8]) -> Res<Peer> {
    let mut p = Peer::default();
    json::object::<Error>(o, |k, v| {
        if k.is("publicKey") {
            p.public_key = s(v, "publicKey")?;
        } else if k.is("endpoint") {
            if !matches!(v, Val::Null) {
                p.endpoint = s(v, "endpoint")?;
            }
        } else if k.is("allowedIps")
            && let Val::Arr(a) = v
        {
            json::array::<Error>(a, |e| {
                let ip: Str<24> = s(e, "allowedIps")?;
                if crate::util::looks_like_cidr(ip.as_str())
                    && (!p.allowed.is_empty() && !p.allowed.push(",") || !p.allowed.push(ip.as_str()))
                {
                    return Err(Error::new(400, "too many allowedIps"));
                }
                Ok(())
            })?;
        }
        Ok(())
    })?;
    Ok(p)
}

impl HostConfig {
    /// Pulls images from the registry (a hub, or a host with `registryConfigDir`); else fetches them from
    /// `imageSource`.
    pub fn pulls_from_registry(&self) -> bool {
        self.hub || !self.registry_config_dir.is_empty()
    }

    pub fn parse(text: &[u8]) -> Res<HostConfig> {
        let mut c = HostConfig {
            name: Str::new(),
            hub: false,
            gateway: false,
            wg_address: Str::new(),
            host_cidr: Str::new(),
            wg_port: 51820,
            wg_key_file: Str::lossy("/etc/sbx/wg.key"),
            api_port: 7443,
            image_port: 7444,
            tls_cert: Str::new(),
            tls_key: Str::new(),
            tls_ca: Str::new(),
            control_cidr: Str::new(),
            sandbox_cidr: Str::new(),
            egress_vip: Str::new(),
            pod_cidr: Str::new(),
            private_gateway: Str::new(),
            hub_peer: None,
            reserve_memory: 1536 << 20,
            registry_config_dir: Str::new(),
            image_source: Str::new(),
            mtu: 1420,
            gateway_mss: 1280,
            direct_egress: false,
            limits: Limits::default(),
        };
        let mut role: Str<16> = Str::new();
        let Val::Obj(o) = json::parse(text).map_err(|_| Error::new(400, "host.json is not JSON"))? else {
            return Err(Error::new(400, "host.json must be an object"));
        };
        json::object::<Error>(o, |k, v| {
            let key = |name: &str| k.is(name);
            if key("name") {
                c.name = s(v, "name")?;
            } else if key("role") {
                role = s(v, "role")?;
            } else if key("gateway") {
                c.gateway = matches!(v, Val::Bool(true));
            } else if key("directEgress") {
                c.direct_egress = matches!(v, Val::Bool(true));
            } else if key("wgAddress") {
                c.wg_address = s(v, "wgAddress")?;
            } else if key("hostCidr") {
                c.host_cidr = s(v, "hostCidr")?;
            } else if key("wgPort") {
                c.wg_port = n(v, "wgPort")? as u16;
            } else if key("wgKeyFile") {
                c.wg_key_file = s(v, "wgKeyFile")?;
            } else if key("apiPort") {
                c.api_port = n(v, "apiPort")? as u16;
            } else if key("imagePort") {
                c.image_port = n(v, "imagePort")? as u16;
            } else if key("tls") {
                let Val::Obj(t) = v else { return Err(Error::new(400, "host.json: tls must be an object")) };
                json::object::<Error>(t, |k, v| {
                    if k.is("cert") {
                        c.tls_cert = s(v, "tls.cert")?;
                    } else if k.is("key") {
                        c.tls_key = s(v, "tls.key")?;
                    } else if k.is("ca") {
                        c.tls_ca = s(v, "tls.ca")?;
                    }
                    Ok(())
                })?;
            } else if key("controlCidr") {
                c.control_cidr = s(v, "controlCidr")?;
            } else if key("sandboxCidr") {
                c.sandbox_cidr = s(v, "sandboxCidr")?;
            } else if key("egressVip") {
                c.egress_vip = s(v, "egressVip")?;
            } else if key("podCidr") {
                c.pod_cidr = s(v, "podCidr")?;
            } else if key("privateGateway") {
                c.private_gateway = s(v, "privateGateway")?;
            } else if key("hub") {
                let Val::Obj(h) = v else { return Err(Error::new(400, "host.json: hub must be an object")) };
                c.hub_peer = Some(parse_peer(h)?);
            } else if key("reserveMemory") {
                c.reserve_memory = n(v, "reserveMemory")? as u64;
            } else if key("registryConfigDir") {
                c.registry_config_dir = s(v, "registryConfigDir")?;
            } else if key("imageSource") {
                c.image_source = s(v, "imageSource")?;
            } else if key("mtu") {
                c.mtu = n(v, "mtu")? as u32;
            } else if key("gatewayMss") {
                c.gateway_mss = n(v, "gatewayMss")? as u32;
            } else if key("limits") {
                let Val::Obj(l) = v else { return Err(Error::new(400, "host.json: limits must be an object")) };
                let lim = &mut c.limits;
                json::object::<Error>(l, |k, v| {
                    let num = |v: Val| n(v, "limits");
                    if k.is("maxConnections") {
                        lim.max_connections = num(v)? as usize;
                    } else if k.is("maxSandboxes") {
                        lim.max_sandboxes = num(v)? as usize;
                    } else if k.is("maxMigrations") {
                        lim.max_migrations = num(v)? as usize;
                    } else if k.is("tlsArenaKiB") {
                        lim.tls_arena = (num(v)? as usize) << 10;
                    } else if k.is("maxBodyKiB") {
                        lim.max_body = (num(v)? as usize) << 10;
                    } else if k.is("imageConnections") {
                        lim.image_connections = num(v)? as usize;
                    } else if k.is("helperMemoryMaxMiB") {
                        lim.helper_memory_max = (num(v)? as u64) << 20;
                    } else if k.is("cgroups") {
                        lim.cgroups = matches!(v, Val::Bool(true));
                    } else if k.is("mlock") {
                        lim.mlock = matches!(v, Val::Bool(true));
                    }
                    Ok(())
                })?;
            }
            Ok(())
        })?;
        if c.name.is_empty() || role.is_empty() || c.wg_address.is_empty() {
            return Err(Error::new(400, "host.json: name, role and wgAddress are required"));
        }
        match role.as_str() {
            "hub" => c.hub = true,
            "burst" => c.hub = false,
            _ => return Err(Error::new(400, "host.json: role must be hub or burst")),
        }
        if c.gateway && !c.hub {
            return Err(Error::new(400, "host.json: only the hub can be a gateway"));
        }
        if !c.hub && c.hub_peer.is_none() {
            return Err(Error::new(400, "host.json: a burst host needs `hub`"));
        }
        // the deployment's addresses: no compiled-in ranges (docs/HOST.md "host.json")
        if c.host_cidr.is_empty() || c.control_cidr.is_empty() || c.sandbox_cidr.is_empty() || c.egress_vip.is_empty() {
            return Err(Error::new(400, "host.json: hostCidr, controlCidr, sandboxCidr and egressVip are required"));
        }
        if c.hub && c.private_gateway.is_empty() {
            return Err(Error::new(400, "host.json: a hub needs privateGateway"));
        }
        if c.gateway && c.pod_cidr.is_empty() {
            return Err(Error::new(400, "host.json: a gateway needs podCidr"));
        }
        if !c.pulls_from_registry() && c.image_source.is_empty() {
            return Err(Error::new(400, "host.json: a host without registry access needs imageSource"));
        }
        if !in_cidr(c.wg_address.as_str(), c.host_cidr.as_str()) {
            return Err(Error::new(400, "host.json: wgAddress must be in hostCidr"));
        }
        if !(536..=9000).contains(&c.gateway_mss) {
            return Err(Error::new(400, "host.json: gatewayMss must be 536-9000"));
        }
        let l = &c.limits;
        if l.max_connections == 0
            || l.max_connections > crate::alloc::MAX_ARENAS
            || l.max_sandboxes == 0
            || l.max_sandboxes > 4096
            || l.max_migrations == 0
            || l.max_migrations > 64
            || l.tls_arena < 2 * crate::server::TLS_HEADROOM
            || l.max_body < (16 << 10)
            || l.max_body > (16 << 20)
            || l.image_connections == 0
            || l.image_connections > 64
        {
            return Err(Error::new(400, "host.json: limits out of range"));
        }
        Ok(c)
    }
}

// ---- the sandbox spec -----------------------------------------------------------------------------------------------

pub const ENV_BYTES: usize = 16 << 10;
pub const ENV_MAX: usize = 128;

#[derive(Clone, Copy)]
pub struct Spec {
    /// sbx-<hex>: the runsc container id, netns name and the controller's name for it
    pub name: Str<48>,
    pub ip: Str<16>,
    /// image digest (hex)
    pub image: Str<64>,
    pub image_ref: Str<256>,
    /// "KEY=VALUE\0" pairs in the controller's order
    pub env: Str<ENV_BYTES>,
    pub cpu: f64,
    pub memory: u64,
    pub cpu_features: Str<2048>,
    /// the conversation's `/workspace` (docs/HOST.md): folder `ws/<workspace>` of the host's JuiceFS mount;
    /// empty = none (a plain folder in scratch)
    pub workspace: Str<64>,
    /// the deployment's shared folder `/drive` (docs/HOST.md): folder `<drive>` of the host's JuiceFS mount, the same
    /// one in every sandbox; empty = none
    pub drive: Str<64>,
    /// the pids limit (hermit spec 13.2 item 9); 0 = not given (the bundle uses [`DEFAULT_PIDS`])
    pub pids: u64,
    /// the size of the scratch filesystem, bytes (docs/HOST.md "Storage"); 0 = not given ([`DEFAULT_SCRATCH_BYTES`])
    pub scratch: u64,
    /// the deployment's other persistent folders (docs/HOST.md "Storage"): "folder\0path\0" for each, in the
    /// controller's order ([`Spec::mounts`]); empty = none
    pub mounts: Str<MOUNTS_BYTES>,
}

/// The most mounts a spec may name (sandboxd-api's `MOUNTS_MAX`), and the bytes all of them may take.
pub const MOUNTS_MAX: usize = 8;
pub const MOUNTS_BYTES: usize = 2048;
/// The paths sandboxd mounts itself: a spec's mount is never on, under or above one of them.
const OWN_MOUNTS: [&str; 8] = ["/workspace", "/drive", "/tmp", "/var/lib/docker", "/proc", "/dev", "/sys", "/run"];
/// The supervisor's FUSE mount point (hermit spec 13.2 item 2): a spec's mount is never on or under it (a home
/// above it is a mount like any other).
const CLAUDE_DIR: &str = "/home/sandbox/.claude";

/// Is `a` the path `b` or a directory above it.
fn at_or_above(a: &str, b: &str) -> bool {
    b == a || (b.len() > a.len() && b.starts_with(a) && b.as_bytes()[a.len()] == b'/')
}

/// A mount's folder: lowercase names joined by `/`, relative to the top of the JuiceFS mount, never the
/// conversations' `ws` tree (a workspace is mounted as the spec's `workspace` only).
pub fn valid_mount_folder(f: &str) -> bool {
    f.len() <= 128 && f.split('/').all(valid_workspace) && f.split('/').next() != Some("ws")
}

/// A mount's path in the sandbox: absolute and plain, not the root, and clear of sandboxd's own mounts.
pub fn valid_mount_path(p: &str) -> bool {
    let plain = |n: &str| {
        !n.is_empty()
            && n != "."
            && n != ".."
            && n.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || c == b'-')
    };
    p.len() <= 128
        && p.starts_with('/')
        && p[1..].split('/').all(plain)
        && !OWN_MOUNTS.iter().any(|own| at_or_above(own, p) || at_or_above(p, own))
        && !at_or_above(CLAUDE_DIR, p)
}

/// The pids limit of a spec that gives none.
pub const DEFAULT_PIDS: u64 = 4096;
/// The scratch filesystem of a spec that gives no size (sandboxd-api's `DEFAULT_SCRATCH_BYTES`).
pub const DEFAULT_SCRATCH_BYTES: u64 = 5 << 30;
/// The sizes a spec may ask for: 1 GiB to 1 TiB.
pub const SCRATCH_RANGE: core::ops::RangeInclusive<u64> = (1 << 30)..=(1 << 40);
// SAFETY: Spec is only fixed strings, numbers and bools, for which all zeros is a valid value.
unsafe impl crate::fixed::Zeroed for Spec {}

pub fn valid_workspace(w: &str) -> bool {
    !w.is_empty() && w.len() <= 64 && w.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// A shared folder's name: a folder at the top of the JuiceFS mount, never `ws` (the conversations' workspaces).
pub fn valid_drive(d: &str) -> bool {
    valid_workspace(d) && d != "ws"
}

pub fn valid_name(n: &str) -> bool {
    n.len() > 4
        && n.len() <= 44
        && n.starts_with("sbx-")
        && n[4..].bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn valid_env_key(k: &str) -> bool {
    let b = k.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

impl Spec {
    /// The spec's mounts as (folder, path), in the controller's order.
    pub fn mounts(&self) -> impl Iterator<Item = (&str, &str)> {
        let mut parts = self.mounts.as_str().split('\0');
        core::iter::from_fn(move || match (parts.next(), parts.next()) {
            (Some(f), Some(p)) if !f.is_empty() => Some((f, p)),
            _ => None,
        })
    }

    pub fn env_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.env.as_str().split('\0').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, "")))
    }

    /// validateSpec(): the controller's spec, checked (an unknown field included). Errors are 400s.
    pub fn parse(body: &[u8], sandbox_cidr: &str) -> Res<Spec> {
        let Val::Obj(o) = json::parse(body)? else { return Err(Error::new(400, "spec: must be an object")) };
        Self::parse_obj(o, sandbox_cidr, true)
    }

    /// `strict`: unknown fields are a 400 (the API); a state file's spec skips them (one written by an older version).
    pub fn parse_obj(o: &[u8], sandbox_cidr: &str, strict: bool) -> Res<Spec> {
        let mut sp = Spec {
            name: Str::new(),
            ip: Str::new(),
            image: Str::new(),
            image_ref: Str::new(),
            env: Str::new(),
            cpu: f64::NAN,
            memory: 0,
            cpu_features: Str::new(),
            workspace: Str::new(),
            drive: Str::new(),
            pids: 0,
            scratch: 0,
            mounts: Str::new(),
        };
        let mut memory = f64::NAN;
        let mut pids = f64::NAN;
        let mut scratch = f64::NAN;
        let (mut have_name, mut have_ip, mut have_image) = (false, false, false);
        let mut envn = 0usize;
        let e400 = |m: &str| Error::new(400, m);
        json::object::<Error>(o, |k, v| {
            if k.is("name") {
                let n: Str<64> = spec_str(v).ok_or_else(|| e400("spec: bad name"))?;
                if !valid_name(n.as_str()) {
                    return Err(e400("spec: bad name"));
                }
                sp.name = Str::lossy(n.as_str());
                have_name = true;
            } else if k.is("ip") {
                let ip: Str<16> = spec_str(v).ok_or_else(|| e400("spec: ip must be in the sandbox range"))?;
                if !in_cidr(ip.as_str(), sandbox_cidr) {
                    return Err(e400("spec: ip must be in the sandbox range"));
                }
                sp.ip = ip;
                have_ip = true;
            } else if k.is("image") {
                let im: Str<64> = spec_str(v).ok_or_else(|| e400("spec: image must be a sha256 digest (hex)"))?;
                if im.len() != 64 || !im.as_bytes().iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
                    return Err(e400("spec: image must be a sha256 digest (hex)"));
                }
                sp.image = im;
                have_image = true;
            } else if k.is("imageRef") {
                if matches!(v, Val::Null) {
                    return Ok(());
                }
                let r: Str<256> = spec_str(v).ok_or_else(|| e400("spec: bad imageRef"))?;
                if r.is_empty() || !r.as_bytes().iter().all(|c| c.is_ascii_alphanumeric() || b"./:@_-".contains(c)) {
                    return Err(e400("spec: bad imageRef"));
                }
                sp.image_ref = r;
            } else if k.is("env") {
                let Val::Obj(eo) = v else {
                    if matches!(v, Val::Null) {
                        return Ok(());
                    }
                    return Err(e400("spec: env must be an object"));
                };
                json::object::<Error>(eo, |ek, ev| {
                    let mut kb = [0u8; 256];
                    let key = ek.decode(&mut kb).filter(|k| valid_env_key(k));
                    let Some(key) = key else {
                        return Err(err!(
                            400,
                            "spec: bad env {}",
                            Str::<64>::lossy(core::str::from_utf8(ek.0).unwrap_or("?"))
                        ));
                    };
                    let mut vb = [0u8; ENV_BYTES];
                    let val = ev.str().and_then(|s| s.decode(&mut vb)).filter(|v| !v.contains('\0'));
                    let Some(val) = val else { return Err(err!(400, "spec: bad env {key}")) };
                    envn += 1;
                    if envn > ENV_MAX
                        || !(sp.env.push(key) && sp.env.push("=") && sp.env.push(val) && sp.env.push("\0"))
                    {
                        return Err(e400("spec: env is too large"));
                    }
                    Ok(())
                })?;
            } else if k.is("cpu") {
                sp.cpu = match v {
                    Val::Num(x) => x,
                    Val::Str(s) => num_from_str(s),
                    _ => f64::NAN,
                };
            } else if k.is("memory") {
                memory = match v {
                    Val::Num(x) => x,
                    Val::Str(s) => num_from_str(s),
                    _ => f64::NAN,
                };
            } else if k.is("workspace") {
                if matches!(v, Val::Null) {
                    return Ok(());
                }
                let ws: Str<64> = spec_str(v).ok_or_else(|| e400("spec: bad workspace"))?;
                if !valid_workspace(ws.as_str()) {
                    return Err(e400("spec: bad workspace"));
                }
                sp.workspace = ws;
            } else if k.is("drive") {
                if matches!(v, Val::Null) {
                    return Ok(());
                }
                let d: Str<64> = spec_str(v).ok_or_else(|| e400("spec: bad drive"))?;
                if !valid_drive(d.as_str()) {
                    return Err(e400("spec: bad drive"));
                }
                sp.drive = d;
            } else if k.is("pids") {
                pids = match v {
                    Val::Null => f64::NAN,
                    Val::Num(x) => x,
                    Val::Str(s) => num_from_str(s),
                    _ => -1.0,
                };
                if !(pids.is_nan() || (pids == pids.trunc() && (64.0..=1_048_576.0).contains(&pids))) {
                    return Err(e400("spec: pids must be a whole number in [64, 1048576]"));
                }
            } else if k.is("scratchBytes") {
                scratch = match v {
                    Val::Null => f64::NAN,
                    Val::Num(x) => x,
                    // a string that is no number is refused, not read as "not given"
                    Val::Str(s) => Some(num_from_str(s)).filter(|n| !n.is_nan()).unwrap_or(-1.0),
                    _ => -1.0,
                };
                let (lo, hi) = (*SCRATCH_RANGE.start() as f64, *SCRATCH_RANGE.end() as f64);
                if !(scratch.is_nan() || (scratch == scratch.trunc() && (lo..=hi).contains(&scratch))) {
                    return Err(e400("spec: scratchBytes must be a whole number of bytes in [1 GiB, 1 TiB]"));
                }
            } else if k.is("mounts") {
                let bad = || e400("spec: bad mounts");
                let Val::Arr(list) = v else {
                    if matches!(v, Val::Null) {
                        return Ok(());
                    }
                    return Err(bad());
                };
                let mut n = 0usize;
                json::array::<Error>(list, |m| {
                    let Val::Obj(mo) = m else { return Err(bad()) };
                    let (mut folder, mut path): (Str<128>, Str<128>) = (Str::new(), Str::new());
                    json::object::<Error>(mo, |mk, mv| {
                        if mk.is("folder") {
                            folder =
                                spec_str(mv).filter(|f: &Str<128>| valid_mount_folder(f.as_str())).ok_or_else(bad)?;
                        } else if mk.is("path") {
                            path = spec_str(mv).filter(|p: &Str<128>| valid_mount_path(p.as_str())).ok_or_else(bad)?;
                        } else {
                            return Err(bad());
                        }
                        Ok(())
                    })?;
                    n += 1;
                    let nested =
                        sp.mounts().any(|(_, p)| at_or_above(p, path.as_str()) || at_or_above(path.as_str(), p));
                    if folder.is_empty() || path.is_empty() || n > MOUNTS_MAX || nested {
                        return Err(bad());
                    }
                    let ok = sp.mounts.push(folder.as_str())
                        && sp.mounts.push("\0")
                        && sp.mounts.push(path.as_str())
                        && sp.mounts.push("\0");
                    if ok { Ok(()) } else { Err(bad()) }
                })?;
            } else if k.is("cpuFeatures") {
                if matches!(v, Val::Null) {
                    return Ok(());
                }
                let f: Str<2048> = spec_str(v).ok_or_else(|| e400("spec: bad cpuFeatures"))?;
                if !f.as_bytes().iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.,-".contains(c)) {
                    return Err(e400("spec: bad cpuFeatures"));
                }
                sp.cpu_features = f;
            } else if strict {
                // an old controller's `user` (the `~/.claude` bind, gone with hermit spec 13.2 item 2) or any other
                // field this version does not know is refused, never dropped
                return Err(err!(
                    400,
                    "spec: unknown field {}",
                    Str::<64>::lossy(core::str::from_utf8(k.0).unwrap_or("?"))
                ));
            }
            Ok(())
        })?;
        if !have_name {
            return Err(e400("spec: bad name"));
        }
        if !have_ip {
            return Err(e400("spec: ip must be in the sandbox range"));
        }
        if !have_image {
            return Err(e400("spec: image must be a sha256 digest (hex)"));
        }
        if !(sp.cpu > 0.0 && sp.cpu <= 64.0) {
            return Err(e400("spec: cpu must be in (0, 64]"));
        }
        if !(memory.is_finite() && memory == memory.trunc() && memory >= (128u64 << 20) as f64 && memory < 1e15) {
            return Err(e400("spec: memory must be bytes >= 128 MiB"));
        }
        sp.memory = memory as u64;
        if !pids.is_nan() {
            sp.pids = pids as u64;
        }
        if !scratch.is_nan() {
            sp.scratch = scratch as u64;
        }
        Ok(sp)
    }

    pub fn write(&self, w: &mut W) {
        w.obj().kstr("name", self.name.as_str()).kstr("ip", self.ip.as_str()).kstr("image", self.image.as_str());
        if !self.image_ref.is_empty() {
            w.kstr("imageRef", self.image_ref.as_str());
        }
        w.key("env").obj();
        for (k, v) in self.env_pairs() {
            w.kstr(k, v);
        }
        w.end();
        w.kf64("cpu", self.cpu).ku64("memory", self.memory).kstr("cpuFeatures", self.cpu_features.as_str());
        // the persistent folder: a migration's destination (GET spec → receive) and the state file need it, or the
        // restored sandbox lacks /workspace (runsc refuses the restore: "Mounts" differ)
        if !self.workspace.is_empty() {
            w.kstr("workspace", self.workspace.as_str());
        }
        // the shared folder travels like the workspace: a migration's destination must mount it too
        if !self.drive.is_empty() {
            w.kstr("drive", self.drive.as_str());
        }
        if self.pids != 0 {
            w.ku64("pids", self.pids);
        }
        // a migration's destination makes the scratch from this (GET spec → receive), so it must travel
        if self.scratch != 0 {
            w.ku64("scratchBytes", self.scratch);
        }
        // the other persistent folders travel like the workspace: a migration's destination must mount them too
        if !self.mounts.is_empty() {
            w.key("mounts").arr();
            for (folder, path) in self.mounts() {
                w.obj().kstr("folder", folder).kstr("path", path).end();
            }
            w.end_arr();
        }
        w.end();
    }

    /// The size of the scratch filesystem the sandbox gets, bytes.
    pub fn scratch_bytes(&self) -> u64 {
        if self.scratch == 0 { DEFAULT_SCRATCH_BYTES } else { self.scratch }
    }

    /// The pids limit the bundle gets.
    pub fn pids_limit(&self) -> u64 {
        if self.pids == 0 { DEFAULT_PIDS } else { self.pids }
    }

    #[cfg(test)]
    pub fn write_json<'a>(&self, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        let mut b = crate::fixed::Buf::new(buf);
        self.write(&mut W::new(&mut b));
        if b.full {
            return None;
        }
        let n = b.len;
        Some(&buf[..n])
    }
}

fn spec_str<const N: usize>(v: Val) -> Option<Str<N>> {
    let mut tmp = [0u8; 4096];
    let s = v.str()?.decode(&mut tmp)?;
    Str::from(s)
}

/// Number("...") for numeric strings (the old code did Number(o.cpu)).
fn num_from_str(s: JStr) -> f64 {
    let mut tmp = [0u8; 64];
    match s.decode(&mut tmp) {
        Some(t) if !t.trim().is_empty() => t.trim().parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_mounts() {
        // the deployment's other persistent folders (docs/HOST.md "Storage"): a list of (folder of the JuiceFS
        // mount, path in the sandbox); sandboxd knows nothing of what a path means
        let with = |m: &str| {
            let mut t = String::from(r#"{"name":"sbx-abc","ip":"10.98.0.10","image":""#);
            t.push_str(&"b".repeat(64));
            t.push_str(r#"","cpu":1,"memory":2147483648,"workspace":"c0ffee-1","mounts":"#);
            t.push_str(m);
            t.push('}');
            Spec::parse(t.as_bytes(), "10.98.0.0/16")
        };
        // absent, null and empty are the same: none, and none written (an older sandboxd refuses the field)
        let plain = br#"{"name":"sbx-abc","ip":"10.98.0.10","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":2147483648}"#;
        let none = Spec::parse(plain, "10.98.0.0/16").ok().unwrap();
        assert_eq!(none.mounts().count(), 0);
        let mut out = [0u8; 4096];
        assert!(!core::str::from_utf8(none.write_json(&mut out).unwrap()).unwrap().contains("mounts"));
        assert_eq!(with("null").ok().unwrap().mounts().count(), 0);
        assert_eq!(with("[]").ok().unwrap().mounts().count(), 0);
        // kept in order through the round trip (GET spec → a migration's receive; the state file)
        let s =
            with(r#"[{"folder":"home/c0ffee-1","path":"/home/claude"},{"folder":"cache","path":"/var/cache/x.y_z"}]"#)
                .ok()
                .unwrap();
        let got: Vec<(&str, &str)> = s.mounts().collect();
        assert_eq!(got, [("home/c0ffee-1", "/home/claude"), ("cache", "/var/cache/x.y_z")]);
        let mut out2 = [0u8; 4096];
        let j = s.write_json(&mut out2).unwrap();
        assert!(
            core::str::from_utf8(j).unwrap().contains(
                r#""mounts":[{"folder":"home/c0ffee-1","path":"/home/claude"},{"folder":"cache","path":"/var/cache/x.y_z"}]"#
            ),
            "{}",
            core::str::from_utf8(j).unwrap()
        );
        let back = Spec::parse(j, "10.98.0.0/16").ok().unwrap();
        assert_eq!(back.mounts().collect::<Vec<_>>(), got);
        assert_eq!(back.workspace.as_str(), "c0ffee-1");
        let no = [
            // not a list of objects with exactly folder and path
            r"{}",
            r#""home""#,
            r"[7]",
            r#"[{"folder":"home"}]"#,
            r#"[{"path":"/home/claude"}]"#,
            r#"[{"folder":"home","path":"/home/claude","mode":"ro"}]"#,
            // a folder is lowercase names joined by /, never the conversations' `ws` tree or a way out
            r#"[{"folder":"","path":"/home/claude"}]"#,
            r#"[{"folder":"/home","path":"/home/claude"}]"#,
            r#"[{"folder":"home/","path":"/home/claude"}]"#,
            r#"[{"folder":"home/../ws","path":"/home/claude"}]"#,
            r#"[{"folder":"ws/c0ffee-2","path":"/home/claude"}]"#,
            r#"[{"folder":"ws","path":"/home/claude"}]"#,
            r#"[{"folder":"Home","path":"/home/claude"}]"#,
            // a path is absolute and plain, and neither the root nor on, under or above a mount of sandboxd's own
            r#"[{"folder":"home","path":"home/claude"}]"#,
            r#"[{"folder":"home","path":"/"}]"#,
            r#"[{"folder":"home","path":"/home/claude/"}]"#,
            r#"[{"folder":"home","path":"/home//claude"}]"#,
            r#"[{"folder":"home","path":"/home/../etc"}]"#,
            r#"[{"folder":"home","path":"/home/./claude"}]"#,
            r#"[{"folder":"home","path":"/home/cl aude"}]"#,
            r#"[{"folder":"home","path":"/workspace"}]"#,
            r#"[{"folder":"home","path":"/workspace/sub"}]"#,
            r#"[{"folder":"home","path":"/drive"}]"#,
            r#"[{"folder":"home","path":"/tmp/x"}]"#,
            r#"[{"folder":"home","path":"/var"}]"#,
            r#"[{"folder":"home","path":"/var/lib"}]"#,
            r#"[{"folder":"home","path":"/var/lib/docker/x"}]"#,
            r#"[{"folder":"home","path":"/proc/1"}]"#,
            r#"[{"folder":"home","path":"/dev"}]"#,
            r#"[{"folder":"home","path":"/sys/fs"}]"#,
            r#"[{"folder":"home","path":"/run"}]"#,
            r#"[{"folder":"home","path":"/home/sandbox/.claude"}]"#,
            r#"[{"folder":"home","path":"/home/sandbox/.claude/x"}]"#,
            // two mounts never share a path or sit inside one another
            r#"[{"folder":"a","path":"/home/claude"},{"folder":"b","path":"/home/claude"}]"#,
            r#"[{"folder":"a","path":"/home/claude"},{"folder":"b","path":"/home/claude/x"}]"#,
            r#"[{"folder":"a","path":"/home/claude/x"},{"folder":"b","path":"/home/claude"}]"#,
        ];
        for m in no {
            let e = with(m).err().unwrap();
            assert!(e.status == 400 && e.msg.as_str().contains("bad mounts"), "{m}: {}", e.msg);
        }
        // a home above the supervisor's FUSE mount point is a mount like any other
        assert_eq!(with(r#"[{"folder":"home/x","path":"/home/sandbox"}]"#).ok().unwrap().mounts().count(), 1);
        // a sibling whose name only begins like another's is no nesting
        assert_eq!(
            with(r#"[{"folder":"a","path":"/home/claude"},{"folder":"b","path":"/home/claude2"}]"#)
                .ok()
                .unwrap()
                .mounts()
                .count(),
            2
        );
        // at most 8
        let many = |n: usize| {
            use std::fmt::Write as _;
            let mut m = String::from("[");
            for i in 0..n {
                if i > 0 {
                    m.push(',');
                }
                let _ = write!(m, r#"{{"folder":"f{i}","path":"/m/p{i}"}}"#);
            }
            m.push(']');
            m
        };
        assert_eq!(with(&many(8)).ok().unwrap().mounts().count(), 8);
        assert!(with(&many(9)).err().unwrap().msg.as_str().contains("bad mounts"));
    }

    #[test]
    fn spec_checks() {
        let ok = br#"{"name":"sbx-abc","ip":"10.98.0.10","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","env":{"HTTP_PROXY":"http://sbx:t@10.99.1.1:3128","EXEC_TOKEN":"e"},"cpu":1,"memory":2147483648,"cpuFeatures":"fpu,sse2"}"#;
        let s = Spec::parse(ok, "10.98.0.0/16").ok().unwrap();
        assert_eq!(s.name.as_str(), "sbx-abc");
        assert_eq!(s.env_pairs().count(), 2);
        let mut out = [0u8; 4096];
        let j = s.write_json(&mut out).unwrap();
        let back = Spec::parse(j, "10.98.0.0/16").ok().unwrap();
        assert_eq!(back.env.as_str(), s.env.as_str());
        assert_eq!(back.memory, 2 << 30);
        // the persistent folders survive the round trip (GET spec → a migration's receive; the state file)
        let cc = br#"{"name":"sbx-abc","ip":"10.98.0.10","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","env":{},"cpu":1,"memory":2147483648,"cpuFeatures":"fpu","workspace":"c0ffee-1"}"#;
        let s2 = Spec::parse(cc, "10.98.0.0/16").ok().unwrap();
        let mut out2 = [0u8; 4096];
        let back2 = Spec::parse(s2.write_json(&mut out2).unwrap(), "10.98.0.0/16").ok().unwrap();
        assert_eq!(back2.workspace.as_str(), "c0ffee-1");
        // the shared folder: absent by default, kept through the round trip, never `ws` or a path
        assert!(s2.drive.is_empty() && back2.drive.is_empty());
        let with = |d: &str| {
            let mut t = String::from(r#"{"name":"sbx-abc","ip":"10.98.0.10","image":""#);
            t.push_str(&"b".repeat(64));
            t.push_str(r#"","cpu":1,"memory":2147483648,"workspace":"c0ffee-1","drive":"#);
            t.push_str(d);
            t.push('}');
            Spec::parse(t.as_bytes(), "10.98.0.0/16")
        };
        let s3 = with("\"drive\"").ok().unwrap();
        assert_eq!(s3.drive.as_str(), "drive");
        let mut out3 = [0u8; 4096];
        let back3 = Spec::parse(s3.write_json(&mut out3).unwrap(), "10.98.0.0/16").ok().unwrap();
        assert_eq!((back3.drive.as_str(), back3.workspace.as_str()), ("drive", "c0ffee-1"));
        assert!(with("null").ok().unwrap().drive.is_empty());
        for no in ["\"ws\"", "\"../etc\"", "\"a/b\"", "\"\"", "\"Drive\"", "7", "true"] {
            let e = with(no).err().unwrap();
            assert!(e.status == 400 && e.msg.as_str().contains("bad drive"), "{no}: {}", e.msg);
        }
        let bad = |b: &[u8], m: &str| {
            let e = Spec::parse(b, "10.98.0.0/16").err().unwrap();
            assert_eq!(e.status, 400);
            assert!(e.msg.as_str().contains(m), "{} vs {m}", e.msg);
        };
        bad(br#"{"name":"sbx-abc","ip":"10.97.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":2147483648}"#, "sandbox range");
        bad(br#"{"name":"../x","ip":"10.98.0.5","image":"b","cpu":1,"memory":2147483648}"#, "bad name");
        bad(br#"{"name":"sbx-a","ip":"10.98.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","env":{"A B":"x"},"cpu":1,"memory":2147483648}"#, "bad env");
        bad(br#"{"name":"sbx-a","ip":"10.98.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":0,"memory":2147483648}"#, "cpu");
        bad(br#"{"name":"sbx-a","ip":"10.98.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":1000}"#, "memory");
        // hermit spec 13.2 item 2: no `user` (an old controller's ~/.claude bind), nor any other unknown field
        bad(br#"{"name":"sbx-a","ip":"10.98.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":2147483648,"user":"u-1"}"#, "unknown field user");
        bad(br#"{"name":"sbx-a","ip":"10.98.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":2147483648,"args":["x"]}"#, "unknown field args");
        // a state file's spec (an older version's) skips them
        let Ok(Val::Obj(o)) = json::parse(br#"{"name":"sbx-a","ip":"10.98.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":2147483648,"user":"u-1"}"#) else { panic!() };
        assert!(Spec::parse_obj(o, "10.98.0.0/16", false).is_ok());
        bad(b"[1]", "object");
        bad(b"{", "JSON");
    }

    #[test]
    fn host_config() {
        let c = HostConfig::parse(br#"{"hostCidr":"10.99.0.0/24","controlCidr":"10.97.0.0/16","sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1","imageSource":"http://10.99.0.1:7444","name":"h","role":"burst","wgAddress":"10.99.0.10","hub":{"publicKey":"k","endpoint":"1.2.3.4:51820","allowedIps":["10.99.0.1/32","x"]},"limits":{"maxSandboxes":30}}"#).ok().unwrap();
        assert!(!c.hub);
        assert_eq!(c.limits.max_sandboxes, 30);
        assert_eq!(c.hub_peer.unwrap().allowed.as_str(), "10.99.0.1/32");
        assert!(HostConfig::parse(br#"{"hostCidr":"10.99.0.0/24","controlCidr":"10.97.0.0/16","sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1","imageSource":"http://10.99.0.1:7444","name":"h","role":"burst","wgAddress":"10.99.0.10"}"#).is_err());
        assert!(HostConfig::parse(br#"{"hostCidr":"10.99.0.0/24","controlCidr":"10.97.0.0/16","sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1","privateGateway":"10.97.0.1","name":"h","role":"hub","wgAddress":"10.9.0.1"}"#).is_err());
    }

    #[test]
    fn addresses_are_required() {
        // no compiled-in ranges: every deployment names its own (docs/HOST.md "host.json")
        let msg = |t: &str| HostConfig::parse(t.as_bytes()).err().map(|e| (e.status, e.msg.as_str().to_owned()));
        let burst = r#""name":"h","role":"burst","wgAddress":"10.99.0.10","hub":{"publicKey":"k"}"#;
        let addrs = r#""hostCidr":"10.99.0.0/24","controlCidr":"10.97.0.0/16","sandboxCidr":"10.98.0.0/16","egressVip":"10.99.1.1""#;
        for drop in ["hostCidr", "controlCidr", "sandboxCidr", "egressVip"] {
            let kept: Vec<&str> = addrs.split(',').filter(|kv| !kv.contains(drop)).collect();
            let t = format!(r#"{{{burst},{},"imageSource":"http://10.99.0.1:7444"}}"#, kept.join(","));
            let (st, m) = msg(&t).unwrap();
            assert!(st == 400 && m.contains("are required"), "{drop}: {m}");
        }
        let (_, m) = msg(&format!("{{{burst},{addrs}}}")).unwrap();
        assert!(m.contains("imageSource"), "{m}");
        assert!(msg(&format!(r#"{{{burst},{addrs},"registryConfigDir":"/etc/sbx/registry"}}"#)).is_none());
        let hub = r#""name":"g","role":"hub","wgAddress":"10.99.0.1""#;
        assert!(msg(&format!("{{{hub},{addrs}}}")).unwrap().1.contains("privateGateway"));
        let gw = format!(r#"{{{hub},"gateway":true,{addrs},"privateGateway":"10.97.0.1"}}"#);
        assert!(msg(&gw).unwrap().1.contains("podCidr"));
        assert!(msg(&gw.replace(r#""gateway":true,"#, r#""gateway":true,"podCidr":"10.96.0.0/16","#)).is_none());
    }
}
