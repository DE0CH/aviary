//! The host configuration (/etc/sbx/host.json, mode 600) and the sandbox spec the controller sends
//! (docs/SANDBOX-VM.md, "sandboxd"). Same fields as the Node sandboxd; `limits` is new (docs/SANDBOXD-MEMORY.md).
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
    pub fn parse(text: &[u8]) -> Res<HostConfig> {
        let mut c = HostConfig {
            name: Str::new(),
            hub: false,
            gateway: false,
            wg_address: Str::new(),
            host_cidr: Str::lossy("10.201.0.0/24"),
            wg_port: 51820,
            wg_key_file: Str::lossy("/etc/sbx/wg.key"),
            api_port: 7443,
            image_port: 7444,
            tls_cert: Str::new(),
            tls_key: Str::new(),
            tls_ca: Str::new(),
            control_cidr: Str::lossy("10.0.0.0/16"),
            sandbox_cidr: Str::lossy("10.200.0.0/16"),
            egress_vip: Str::lossy("10.201.1.1"),
            pod_cidr: Str::new(),
            private_gateway: Str::new(),
            hub_peer: None,
            reserve_memory: 1536 << 20,
            registry_config_dir: Str::new(),
            image_source: Str::new(),
            mtu: 1420,
            limits: Limits::default(),
        };
        let mut role: Str<16> = Str::new();
        let Val::Obj(o) = json::parse(text).map_err(|_| Error::new(400, "host.json is not JSON"))? else {
            return Err(Error::new(400, "host.json must be an object"));
        };
        json::object::<Error>(o, |k, v| {
            let key = |name: &str| k.is(name);
            if key("name") {
                c.name = s(v, "name")?
            } else if key("role") {
                role = s(v, "role")?
            } else if key("gateway") {
                c.gateway = matches!(v, Val::Bool(true))
            } else if key("wgAddress") {
                c.wg_address = s(v, "wgAddress")?
            } else if key("hostCidr") {
                c.host_cidr = s(v, "hostCidr")?
            } else if key("wgPort") {
                c.wg_port = n(v, "wgPort")? as u16
            } else if key("wgKeyFile") {
                c.wg_key_file = s(v, "wgKeyFile")?
            } else if key("apiPort") {
                c.api_port = n(v, "apiPort")? as u16
            } else if key("imagePort") {
                c.image_port = n(v, "imagePort")? as u16
            } else if key("tls") {
                let Val::Obj(t) = v else { return Err(Error::new(400, "host.json: tls must be an object")) };
                json::object::<Error>(t, |k, v| {
                    if k.is("cert") {
                        c.tls_cert = s(v, "tls.cert")?
                    } else if k.is("key") {
                        c.tls_key = s(v, "tls.key")?
                    } else if k.is("ca") {
                        c.tls_ca = s(v, "tls.ca")?
                    }
                    Ok(())
                })?;
            } else if key("controlCidr") {
                c.control_cidr = s(v, "controlCidr")?
            } else if key("sandboxCidr") {
                c.sandbox_cidr = s(v, "sandboxCidr")?
            } else if key("egressVip") {
                c.egress_vip = s(v, "egressVip")?
            } else if key("podCidr") {
                c.pod_cidr = s(v, "podCidr")?
            } else if key("privateGateway") {
                c.private_gateway = s(v, "privateGateway")?
            } else if key("hub") {
                let Val::Obj(h) = v else { return Err(Error::new(400, "host.json: hub must be an object")) };
                c.hub_peer = Some(parse_peer(h)?);
            } else if key("reserveMemory") {
                c.reserve_memory = n(v, "reserveMemory")? as u64
            } else if key("registryConfigDir") {
                c.registry_config_dir = s(v, "registryConfigDir")?
            } else if key("imageSource") {
                c.image_source = s(v, "imageSource")?
            } else if key("mtu") {
                c.mtu = n(v, "mtu")? as u32
            } else if key("limits") {
                let Val::Obj(l) = v else { return Err(Error::new(400, "host.json: limits must be an object")) };
                let lim = &mut c.limits;
                json::object::<Error>(l, |k, v| {
                    let num = |v: Val| n(v, "limits");
                    if k.is("maxConnections") {
                        lim.max_connections = num(v)? as usize
                    } else if k.is("maxSandboxes") {
                        lim.max_sandboxes = num(v)? as usize
                    } else if k.is("maxMigrations") {
                        lim.max_migrations = num(v)? as usize
                    } else if k.is("tlsArenaKiB") {
                        lim.tls_arena = (num(v)? as usize) << 10
                    } else if k.is("maxBodyKiB") {
                        lim.max_body = (num(v)? as usize) << 10
                    } else if k.is("imageConnections") {
                        lim.image_connections = num(v)? as usize
                    } else if k.is("helperMemoryMaxMiB") {
                        lim.helper_memory_max = (num(v)? as u64) << 20
                    } else if k.is("cgroups") {
                        lim.cgroups = matches!(v, Val::Bool(true))
                    } else if k.is("mlock") {
                        lim.mlock = matches!(v, Val::Bool(true))
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
        if !in_cidr(c.wg_address.as_str(), c.host_cidr.as_str()) {
            return Err(Error::new(400, "host.json: wgAddress must be in hostCidr"));
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
    /// the conversation's persistent storage (docs/CLAUDE-CODE.md): `/workspace` and `~/.claude` are subfolders
    /// `ws/<workspace>/…` of the host's JuiceFS mount; empty = none (both are then plain scratch)
    pub workspace: Str<64>,
}
unsafe impl crate::fixed::Zeroed for Spec {}

pub fn valid_workspace(w: &str) -> bool {
    !w.is_empty() && w.len() <= 64 && w.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
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
    pub fn env_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.env.as_str().split('\0').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, "")))
    }

    /// validateSpec(): the controller's spec, checked. Errors are 400s.
    pub fn parse(body: &[u8], sandbox_cidr: &str) -> Res<Spec> {
        let Val::Obj(o) = json::parse(body)? else { return Err(Error::new(400, "spec: must be an object")) };
        Self::parse_obj(o, sandbox_cidr)
    }

    pub fn parse_obj(o: &[u8], sandbox_cidr: &str) -> Res<Spec> {
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
        };
        let mut memory = f64::NAN;
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
            } else if k.is("cpuFeatures") {
                if matches!(v, Val::Null) {
                    return Ok(());
                }
                let f: Str<2048> = spec_str(v).ok_or_else(|| e400("spec: bad cpuFeatures"))?;
                if !f.as_bytes().iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.,-".contains(c)) {
                    return Err(e400("spec: bad cpuFeatures"));
                }
                sp.cpu_features = f;
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
        w.kf64("cpu", self.cpu).ku64("memory", self.memory).kstr("cpuFeatures", self.cpu_features.as_str()).end();
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
    fn spec_checks() {
        let ok = br#"{"name":"sbx-abc","ip":"10.200.0.10","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","env":{"HTTP_PROXY":"http://sbx:t@10.201.1.1:3128","JARVIS_EXEC_TOKEN":"e"},"cpu":1,"memory":2147483648,"cpuFeatures":"fpu,sse2"}"#;
        let s = Spec::parse(ok, "10.200.0.0/16").ok().unwrap();
        assert_eq!(s.name.as_str(), "sbx-abc");
        assert_eq!(s.env_pairs().count(), 2);
        let mut out = [0u8; 4096];
        let j = s.write_json(&mut out).unwrap();
        let back = Spec::parse(j, "10.200.0.0/16").ok().unwrap();
        assert_eq!(back.env.as_str(), s.env.as_str());
        assert_eq!(back.memory, 2 << 30);
        let bad = |b: &[u8], m: &str| {
            let e = Spec::parse(b, "10.200.0.0/16").err().unwrap();
            assert_eq!(e.status, 400);
            assert!(e.msg.as_str().contains(m), "{} vs {m}", e.msg);
        };
        bad(br#"{"name":"sbx-abc","ip":"10.0.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":2147483648}"#, "sandbox range");
        bad(br#"{"name":"../x","ip":"10.200.0.5","image":"b","cpu":1,"memory":2147483648}"#, "bad name");
        bad(br#"{"name":"sbx-a","ip":"10.200.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","env":{"A B":"x"},"cpu":1,"memory":2147483648}"#, "bad env");
        bad(br#"{"name":"sbx-a","ip":"10.200.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":0,"memory":2147483648}"#, "cpu");
        bad(br#"{"name":"sbx-a","ip":"10.200.0.5","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":1000}"#, "memory");
        bad(b"[1]", "object");
        bad(b"{", "JSON");
    }

    #[test]
    fn host_config() {
        let c = HostConfig::parse(br#"{"name":"h","role":"burst","wgAddress":"10.201.0.10","hub":{"publicKey":"k","endpoint":"1.2.3.4:51820","allowedIps":["10.201.0.1/32","x"]},"limits":{"maxSandboxes":30}}"#).ok().unwrap();
        assert!(!c.hub);
        assert_eq!(c.limits.max_sandboxes, 30);
        assert_eq!(c.hub_peer.unwrap().allowed.as_str(), "10.201.0.1/32");
        assert!(HostConfig::parse(br#"{"name":"h","role":"burst","wgAddress":"10.201.0.10"}"#).is_err());
        assert!(HostConfig::parse(br#"{"name":"h","role":"hub","wgAddress":"10.9.0.1"}"#).is_err());
    }
}
