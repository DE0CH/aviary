//! Host networking (docs/SANDBOX-VM.md, "Networking per sandbox", "Cross-cloud network"); the same commands and
//! nftables rules as the Node sandboxd.
//!
//! Per sandbox: a netns named like the sandbox with a veth pair; the sandbox side has <ip>/32 and a default route via
//! 169.254.1.1 (proxy ARP on the host side, proxy_delay 0); the host side has the same MAC on every host, the sandbox
//! side a MAC derived from its IP, so ARP caches stay valid across a move. One stateless nftables table per sandbox
//! (a connection arriving mid-stream on a new host has no conntrack entry).
use crate::config::{HostConfig, Peer};
use crate::fixed::{Buf, Str};
use crate::proc::{quiet, run, sh};
use crate::util::{Error, Res, h12};
use crate::{err, path};
use core::fmt::Write as _;
use std::sync::Mutex;

pub const GW_MAC: &str = "ee:ee:ee:ee:ee:ee";

pub fn veth_of(name: &str) -> Str<16> {
    let mut s = Str::new();
    let _ = write!(s, "v{}", h12(name));
    s
}
pub fn table_of(name: &str) -> Str<20> {
    let mut s = Str::new();
    let _ = write!(s, "sbx_{}", h12(name));
    s
}
pub fn mac_of(ip: &str) -> Str<20> {
    let mut s = Str::new();
    let _ = s.push("02:5b");
    for p in ip.split('.') {
        let _ = write!(s, ":{:02x}", p.parse::<u8>().unwrap_or(0));
    }
    s
}

/// nft -f with a file (Ubuntu 24.04's nft refuses a pipe on stdin)
pub fn nft_apply(rules: &[u8]) -> Res<()> {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static N: AtomicU64 = AtomicU64::new(0);
    let _ = crate::util::mkdir(crate::paths::run_dir().as_str(), 0o700);
    let f = path!("/run/sbx/.nft-{}-{}", std::process::id(), N.fetch_add(1, Relaxed));
    crate::util::write_file_atomic(f.as_str(), rules, 0o600)?;
    let r = sh(&["nft", "-f", f.as_str()]);
    crate::util::remove_file(f.as_str());
    r
}

/// The per-sandbox nftables table (pure: tested).
pub fn sandbox_rules(b: &mut Buf, name: &str, ip: &str, egress_vip: &str, control_cidr: &str) {
    let v = veth_of(name);
    let t = table_of(name);
    let _ = write!(
        b,
        "table inet {t}
delete table inet {t}
table inet {t} {{
  chain from_sandbox {{
    type filter hook prerouting priority -300; policy accept;
    iifname \"{v}\" ip saddr {ip} ip daddr {egress_vip} tcp dport 3128 accept
    iifname \"{v}\" ip saddr {ip} ip daddr {control_cidr} tcp sport 7070 accept
    iifname \"{v}\" counter drop
  }}
  chain to_sandbox {{
    type filter hook postrouting priority 300; policy accept;
    oifname \"{v}\" ip saddr {egress_vip} tcp sport 3128 ip daddr {ip} accept
    oifname \"{v}\" ip saddr {control_cidr} tcp dport 7070 ip daddr {ip} accept
    oifname \"{v}\" counter drop
  }}
}}
"
    );
}

/// netns + veth + nft for a sandbox; the /32 route only with `route` (a migration target adds it after restore).
pub fn add_sandbox_net(cfg: &HostConfig, name: &str, ip: &str, route: bool) -> Res<()> {
    remove_sandbox_net(name, Some(ip));
    let v = veth_of(name);
    let v = v.as_str();
    let mut mtu: Str<8> = Str::new();
    let _ = write!(mtu, "{}", cfg.mtu);
    let mac = mac_of(ip);
    let mut ip32: Str<20> = Str::new();
    let _ = write!(ip32, "{ip}/32");
    sh(&["ip", "netns", "add", name])?;
    sh(&["ip", "link", "add", v, "type", "veth", "peer", "name", "eth0", "netns", name])?;
    sh(&["ip", "link", "set", v, "mtu", mtu.as_str(), "address", GW_MAC, "up"])?;
    sh(&["ip", "-n", name, "link", "set", "eth0", "mtu", mtu.as_str(), "address", mac.as_str(), "up"])?;
    sh(&["ip", "-n", name, "link", "set", "lo", "up"])?;
    sh(&["ip", "-n", name, "addr", "add", ip32.as_str(), "dev", "eth0"])?;
    sh(&["ip", "-n", name, "route", "add", "169.254.1.1", "dev", "eth0", "scope", "link"])?;
    sh(&["ip", "-n", name, "route", "add", "default", "via", "169.254.1.1", "dev", "eth0"])?;
    let (mut a, mut b2, mut c, mut d): (Str<64>, Str<64>, Str<64>, Str<64>) =
        (Str::new(), Str::new(), Str::new(), Str::new());
    let _ = write!(a, "net.ipv4.conf.{v}.proxy_arp=1");
    let _ = write!(b2, "net.ipv4.conf.{v}.rp_filter=0");
    let _ = write!(c, "net.ipv4.neigh.{v}.proxy_delay=0");
    let _ = write!(d, "net.ipv6.conf.{v}.disable_ipv6=1");
    sh(&["sysctl", "-qw", a.as_str(), b2.as_str(), c.as_str(), d.as_str()])?;
    let mut mem = [0u8; 2048];
    let mut rb = Buf::new(&mut mem);
    sandbox_rules(&mut rb, name, ip, cfg.egress_vip.as_str(), cfg.control_cidr.as_str());
    if rb.full {
        return Err(Error::new(500, "nft rules too long"));
    }
    nft_apply(rb.as_bytes())?;
    if route {
        route_sandbox(name, ip)?;
    }
    Ok(())
}

pub fn route_sandbox(name: &str, ip: &str) -> Res<()> {
    let mut ip32: Str<20> = Str::new();
    let _ = write!(ip32, "{ip}/32");
    sh(&["ip", "route", "replace", ip32.as_str(), "dev", veth_of(name).as_str()])
}

pub fn remove_sandbox_net(name: &str, ip: Option<&str>) {
    let v = veth_of(name);
    if let Some(ip) = ip {
        let mut ip32: Str<20> = Str::new();
        let _ = write!(ip32, "{ip}/32");
        quiet(&["ip", "route", "del", ip32.as_str(), "dev", v.as_str()]);
    }
    quiet(&["nft", "delete", "table", "inet", table_of(name).as_str()]);
    quiet(&["ip", "link", "del", v.as_str()]);
    quiet(&["ip", "netns", "del", name]);
}

// ---- host level ---------------------------------------------------------------------------------------------------

fn iface_from_route(out: &str) -> Option<Str<32>> {
    let i = out.find(" dev ")?;
    let rest = &out[i + 5..];
    Str::from(rest.split_whitespace().next()?)
}

/// Host rules: forwarding only between wg0, the private network (hub) and sandbox veths; the hub's egress table.
/// A gateway (a shared k3s node) only fences wg0's own traffic and leaves everything else on the machine alone.
pub fn host_rules(
    b: &mut Buf,
    cfg: &HostConfig,
    public_if: &str,
    private_if: Option<&str>,
    private_addr: &str,
    targets: &[Str<16>],
) {
    b.s("table ip sbxhost\ndelete table ip sbxhost\ntable ip sbxhost {");
    if cfg.hub {
        b.s("\n  chain egress_dnat {\n    type nat hook prerouting priority dstnat; policy accept;\n");
        if !targets.is_empty() {
            let _ = write!(
                b,
                "    ip daddr {} tcp dport 3128 dnat to numgen random mod {} map {{ ",
                cfg.egress_vip,
                targets.len()
            );
            for (i, t) in targets.iter().enumerate() {
                let _ = write!(b, "{}{} : {}", if i > 0 { ", " } else { "" }, i, t);
            }
            b.s(" }");
        }
        b.s("\n  }");
    }
    if cfg.gateway {
        let (vip, api, img) = (cfg.egress_vip.as_str(), cfg.api_port, cfg.image_port);
        // a k3s node masquerades every non-pod source going to a pod (flannel's FLANNEL-POSTRTG): what comes in on wg0
        // keeps the sandbox's own address (egress admits a sandbox's token only from it; its NetworkPolicy only
        // 10.200.0.0/16). An identity SNAT, earlier than iptables' nat (priority srcnat), binds the connection's NAT
        // so the later masquerade never applies; such packets leave over the private network (policy route, init).
        let sbx = cfg.sandbox_cidr.as_str();
        let _ = write!(
            b,
            "\n  chain keep_source {{\n    type nat hook postrouting priority srcnat - 10; policy accept;\n    iifname \"wg0\" ip saddr {sbx} snat ip prefix to ip saddr map {{ {sbx} : {sbx} }}"
        );
        // this node's own pods reaching a sandbox (the ui pod's Claude Code runs, the controller): they leave through
        // wg0, where flannel's masquerade would give them wg0's address 10.201.0.1, which a sandbox's lock drops (it
        // admits the controlCidr only); other nodes' pods arrive masqueraded to their node's private address. So
        // these get this node's private address too, before iptables' nat runs.
        if !private_addr.is_empty() {
            let pods = if cfg.pod_cidr.is_empty() { "10.244.0.0/16" } else { cfg.pod_cidr.as_str() };
            let _ = write!(b, "\n    oifname \"wg0\" ip saddr {pods} ip daddr {sbx} snat to {private_addr}");
        }
        b.s("\n  }");
        let _ = write!(
            b,
            "\n  chain forward {{\n    type filter hook forward priority 0; policy accept;\n    iifname \"wg0\" ip daddr {vip} drop\n    iifname \"wg0\" oifname \"{public_if}\" drop\n    iifname \"{public_if}\" oifname \"wg0\" drop\n  }}\n  chain input {{\n    type filter hook input priority 0; policy accept;\n    iifname \"wg0\" tcp dport {{ {api}, {img} }} accept\n    iifname \"wg0\" icmp type echo-request accept\n    iifname \"wg0\" ct state established,related accept\n    iifname \"wg0\" drop\n    tcp dport {{ {api}, {img} }} iifname \"{public_if}\" drop\n  }}\n}}\n"
        );
        return;
    }
    b.s("\n  chain forward {\n    type filter hook forward priority 0; policy accept;\n");
    // the hub drops what reaches the egress address untranslated; a burst host forwards it into wg0 (to the hub)
    if cfg.hub {
        let _ = writeln!(b, "    ip daddr {} drop", cfg.egress_vip);
    }
    let _ = write!(b, "    iifname \"{public_if}\" drop\n    oifname \"{public_if}\" drop\n");
    if private_if.is_none() {
        let _ = writeln!(b, "    ip daddr {} oifname != \"wg0\" drop", cfg.control_cidr);
    }
    let _ = write!(
        b,
        "  }}\n  chain input {{\n    type filter hook input priority 0; policy accept;\n    tcp dport {{ {}, {} }} iifname \"{public_if}\" drop\n  }}\n}}\n",
        cfg.api_port, cfg.image_port
    );
}

/// gateway: the routing table for what comes in on wg0 (pods via the private network) and how its rule shows up
pub const GATEWAY_TABLE: &str = "51822";
const GATEWAY_RULE_MARK: &str = "iif wg0 lookup 51822";
const GATEWAY_LOCAL_MARK: &str = "iif wg0 lookup main suppress_prefixlength 16";

pub const MAX_TARGETS: usize = 64;
pub const MAX_PEERS: usize = 32;

pub struct NetState {
    pub public_if: Str<32>,
    pub private_if: Option<Str<32>>,
    /// gateway: the node's address on the private network (the controlCidr sandboxes admit on :7070)
    pub private_addr: Str<16>,
    pub targets: [Str<16>; MAX_TARGETS],
    pub ntargets: usize,
    pub public_key: Str<64>,
}

pub struct HostNet {
    pub st: Mutex<NetState>,
}

fn egress_file() -> crate::paths::P {
    path!("/var/lib/sbx/egress-targets")
}

impl HostNet {
    pub fn new() -> HostNet {
        HostNet {
            st: Mutex::new(NetState {
                public_if: Str::new(),
                private_if: None,
                private_addr: Str::new(),
                targets: [Str::new(); MAX_TARGETS],
                ntargets: 0,
                public_key: Str::new(),
            }),
        }
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, NetState> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn init(&self, cfg: &HostConfig) -> Res<()> {
        // a gateway shares its machine (a k3s node): forwarding only, the node's own rp_filter and limits stay
        if cfg.gateway {
            sh(&["sysctl", "-qw", "net.ipv4.ip_forward=1"])?;
        } else {
            sh(&[
                "sysctl",
                "-qw",
                "net.ipv4.ip_forward=1",
                "net.ipv4.conf.all.rp_filter=0",
                "net.ipv4.conf.default.rp_filter=0",
                "vm.max_map_count=262144",
            ])?;
        }
        let mut out = [0u8; 4096];
        let o = run(&["ip", "-o", "route", "show", "default"], &mut out, 10_000)?;
        let public_if = match iface_from_route(o.out_str()) {
            Some(i) => i,
            None if cfg.gateway => Str::lossy("eth0"),
            None => return Err(Error::new(500, "no default route")),
        };
        let mut st = self.lock();
        st.public_if = public_if;
        if quiet(&["ip", "link", "show", "wg0"]) != 0 {
            sh(&["ip", "link", "add", "wg0", "type", "wireguard"])?;
        }
        let mut port: Str<8> = Str::new();
        let _ = write!(port, "{}", cfg.wg_port);
        sh(&["wg", "set", "wg0", "private-key", cfg.wg_key_file.as_str(), "listen-port", port.as_str()])?;
        let prefix = cfg.host_cidr.as_str().split('/').nth(1).unwrap_or("24");
        let mut addr: Str<24> = Str::new();
        let _ = write!(addr, "{}/{prefix}", cfg.wg_address);
        sh(&["ip", "addr", "replace", addr.as_str(), "dev", "wg0"])?;
        let mut mtu: Str<8> = Str::new();
        let _ = write!(mtu, "{}", cfg.mtu);
        sh(&["ip", "link", "set", "wg0", "mtu", mtu.as_str(), "up"])?;
        let mut out2 = [0u8; 256];
        let o = run(&["wg", "show", "wg0", "public-key"], &mut out2, 10_000)?;
        st.public_key = Str::lossy(o.out_str().trim());
        if cfg.gateway {
            // every sandbox is behind WireGuard; the node's own pods reach them from its private address (they are
            // masqueraded to it, and sandboxes accept the controller only from the private network)
            let gw = if cfg.private_gateway.is_empty() { "10.0.0.1" } else { cfg.private_gateway.as_str() };
            let o = run(&["ip", "-o", "route", "get", gw], &mut out, 10_000)?;
            let pif = iface_from_route(o.out_str()).ok_or_else(|| err!(500, "no route to {gw}"))?;
            let mut out2 = [0u8; 4096];
            let o = run(&["ip", "-o", "-4", "addr", "show", "dev", pif.as_str()], &mut out2, 10_000)?;
            let src = o
                .out_str()
                .split_once("inet ")
                .and_then(|(_, r)| r.split('/').next())
                .filter(|s| crate::util::looks_like_ip(s));
            if let Some(src) = src {
                st.private_addr = Str::lossy(src);
            }
            match src {
                Some(src) => sh(&["ip", "route", "replace", cfg.sandbox_cidr.as_str(), "dev", "wg0", "src", src])?,
                None => sh(&["ip", "route", "replace", cfg.sandbox_cidr.as_str(), "dev", "wg0"])?,
            }
            // what comes in on wg0 (sandboxes, to egress pods) goes to pods over the private network, as from the old
            // hub, not through flannel's WireGuard (its peers drop a source outside the node's pod range)
            let pods = if cfg.pod_cidr.is_empty() { "10.244.0.0/16" } else { cfg.pod_cidr.as_str() };
            sh(&["ip", "route", "replace", pods, "via", gw, "dev", pif.as_str(), "table", GATEWAY_TABLE])?;
            let mut out3 = [0u8; 4096];
            let rules = run(&["ip", "rule", "show"], &mut out3, 10_000)?;
            if !rules.out_str().contains(GATEWAY_RULE_MARK) {
                sh(&["ip", "rule", "add", "iif", "wg0", "lookup", GATEWAY_TABLE, "priority", "100"])?;
            }
            // ... except routes more specific than /16 in the main table, e.g. this node's own pod /24 on cni0 (an
            // egress pod here must not go out to the private network and back)
            if !rules.out_str().contains(GATEWAY_LOCAL_MARK) {
                sh(&[
                    "ip",
                    "rule",
                    "add",
                    "iif",
                    "wg0",
                    "lookup",
                    "main",
                    "suppress_prefixlength",
                    "16",
                    "priority",
                    "99",
                ])?;
            }
            st.private_if = Some(pif);
            load_targets(&mut st);
        } else if cfg.hub {
            let gw = if cfg.private_gateway.is_empty() { "10.0.0.1" } else { cfg.private_gateway.as_str() };
            let o = run(&["ip", "-o", "route", "get", gw], &mut out, 10_000)?;
            let pif = iface_from_route(o.out_str()).ok_or_else(|| err!(500, "no route to {gw}"))?;
            sh(&["ip", "route", "replace", cfg.sandbox_cidr.as_str(), "dev", "wg0"])?;
            if !cfg.pod_cidr.is_empty() {
                sh(&["ip", "route", "replace", cfg.pod_cidr.as_str(), "via", gw, "dev", pif.as_str()])?;
            }
            st.private_if = Some(pif);
            load_targets(&mut st);
        } else {
            drop(st);
            // only make sure the hub is a peer: other peers (AWS hosts, for AWS<->AWS moves) stay until the
            // controller's next PUT /v1/peers (the Node sandboxd removed them at every start)
            let hub = cfg.hub_peer.as_ref().ok_or_else(|| Error::new(500, "no hub"))?;
            set_peer(hub)?;
            {
                let dst = cfg.control_cidr.as_str();
                sh(&["ip", "route", "replace", dst, "dev", "wg0"])?;
            }
            let mut vip: Str<20> = Str::new();
            let _ = write!(vip, "{}/32", cfg.egress_vip);
            sh(&["ip", "route", "replace", vip.as_str(), "dev", "wg0"])?;
            st = self.lock();
        }
        apply_host_rules(cfg, &st)
    }

    /// The host-level routes `init` makes, as `ip -o route show table all` should list them; true if one is gone.
    /// Something else on the machine can remove them: systemd-networkd drops routes it didn't make when it restarts
    /// or reconfigures (seen on the gateway node, 2026-10-21: sandboxes lost the controller and egress); host setup
    /// turns that off (ManageForeignRoutes=no) and sandboxd checks every 10 s and re-runs `init` if needed.
    pub fn routes_missing(cfg: &HostConfig, table: &str) -> bool {
        let has = |dst: &str, extra: &str| {
            table.lines().any(|l| {
                l.strip_prefix(dst).map(|r| r.starts_with(' ')).unwrap_or(false)
                    && l.contains(" dev wg0")
                    && (extra.is_empty() || l.contains(extra))
            })
        };
        let has_tbl = |dst: &str| {
            table
                .lines()
                .any(|l| l.strip_prefix(dst).map(|r| r.starts_with(' ')).unwrap_or(false) && l.contains(" table 51822"))
        };
        if cfg.gateway {
            let pods = if cfg.pod_cidr.is_empty() { "10.244.0.0/16" } else { cfg.pod_cidr.as_str() };
            !has(cfg.sandbox_cidr.as_str(), "") || !has_tbl(pods)
        } else if cfg.hub {
            !has(cfg.sandbox_cidr.as_str(), "")
        } else {
            !has(cfg.control_cidr.as_str(), "") || !has(cfg.egress_vip.as_str(), "")
        }
    }

    /// hub: the egress pods' addresses; new connections to the VIP go to one of them.
    pub fn set_egress_targets(&self, cfg: &HostConfig, list: &[Str<16>]) -> Res<()> {
        // dedupe + sort as strings (as the Node code did)
        let mut t = [Str::<16>::new(); MAX_TARGETS];
        let mut n = 0;
        for x in list {
            if crate::util::looks_like_ip(x.as_str()) && !t[..n].contains(x) {
                if n == MAX_TARGETS {
                    return Err(Error::new(400, "too many targets"));
                }
                t[n] = *x;
                n += 1;
            }
        }
        t[..n].sort_unstable_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut st = self.lock();
        if st.targets[..st.ntargets] == t[..n] {
            return Ok(());
        }
        let (old, oldn) = (st.targets, st.ntargets);
        st.targets = t;
        st.ntargets = n;
        if let Err(e) = apply_host_rules(cfg, &st) {
            st.targets = old;
            st.ntargets = oldn;
            return Err(e);
        }
        let mut mem = [0u8; 2048];
        let mut b = Buf::new(&mut mem);
        for x in &t[..n] {
            b.s(x.as_str());
            b.s("\n");
        }
        let _ = crate::util::write_file_atomic(egress_file().as_str(), b.as_bytes(), 0o600);
        Ok(())
    }
}

/// The egress targets the controller pushed before a restart (the Node sandboxd forgot them until the next push, so
/// sandboxes lost egress for a moment on every upgrade).
/// Without that file (the first start after the Node sandboxd), they are read back from the live nft table.
fn load_targets(st: &mut NetState) {
    let mut mem = [0u8; 2048];
    if let Ok(t) = crate::util::read_file(egress_file().as_str(), &mut mem) {
        let t = core::str::from_utf8(t).unwrap_or("");
        let mut n = 0;
        for x in t.split_whitespace() {
            if n < MAX_TARGETS && crate::util::looks_like_ip(x) {
                st.targets[n] = Str::lossy(x);
                n += 1;
            }
        }
        st.ntargets = n;
        return;
    }
    let mut out = [0u8; 16384];
    if let Ok(o) = run(&["nft", "list", "table", "ip", "sbxhost"], &mut out, 10_000)
        && o.code == 0
    {
        st.ntargets = targets_from_nft(o.out_str(), &mut st.targets);
    }
}

/// The DNAT map's addresses in `nft list table ip sbxhost` output ("... map { 0 : 10.244.3.5, 1 : 10.244.6.7 }").
pub fn targets_from_nft(out: &str, t: &mut [Str<16>; MAX_TARGETS]) -> usize {
    let Some(i) = out.find("dnat to numgen") else { return 0 };
    let Some(j) = out[i..].find('{') else { return 0 };
    let rest = &out[i + j + 1..];
    let body = &rest[..rest.find('}').unwrap_or(0)];
    let mut n = 0;
    for item in body.split(',') {
        if let Some((_, ip)) = item.split_once(':') {
            let ip = ip.trim();
            if n < MAX_TARGETS && crate::util::looks_like_ip(ip) {
                t[n] = Str::lossy(ip);
                n += 1;
            }
        }
    }
    n
}

/// This host's global IPv6 address on its public interface (AWS hosts: the gateway dials it), if any.
pub fn public_ipv6(public_if: &str) -> Option<Str<48>> {
    let mut out = [0u8; 4096];
    let o = run(&["ip", "-6", "-o", "addr", "show", "dev", public_if, "scope", "global"], &mut out, 10_000).ok()?;
    let (_, rest) = o.out_str().split_once("inet6 ")?;
    let a = rest.split('/').next()?;
    if !a.is_empty() && a.bytes().all(|c| c.is_ascii_hexdigit() || c == b':') { Str::from(a) } else { None }
}

impl Default for HostNet {
    fn default() -> Self {
        Self::new()
    }
}

pub fn apply_host_rules(cfg: &HostConfig, st: &NetState) -> Res<()> {
    let mut mem = [0u8; 8192];
    let mut b = Buf::new(&mut mem);
    host_rules(
        &mut b,
        cfg,
        st.public_if.as_str(),
        st.private_if.as_ref().map(|s| s.as_str()),
        st.private_addr.as_str(),
        &st.targets[..st.ntargets],
    );
    if b.full {
        return Err(Error::new(500, "host rules too long"));
    }
    nft_apply(b.as_bytes())
}

pub fn valid_wg_key(k: &str) -> bool {
    let b = k.as_bytes();
    b.len() == 44
        && b[..42].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/')
        && b"AEIMQUYcgkosw048".contains(&b[42])
        && b[43] == b'='
}

fn set_peer(p: &Peer) -> Res<()> {
    if !valid_wg_key(p.public_key.as_str()) {
        return Err(Error::new(400, "bad WireGuard public key"));
    }
    let mut args: [&str; 11] = [
        "wg",
        "set",
        "wg0",
        "peer",
        p.public_key.as_str(),
        "allowed-ips",
        p.allowed.as_str(),
        "persistent-keepalive",
        "25",
        "",
        "",
    ];
    let mut n = 9;
    if !p.endpoint.is_empty() {
        args[9] = "endpoint";
        args[10] = p.endpoint.as_str();
        n = 11;
    }
    sh(&args[..n])
}

/// One line of `wg show wg0 dump` (after the first): public key, psk, endpoint, allowed ips, handshake, ...
pub struct DumpPeer<'a> {
    pub key: &'a str,
    pub endpoint: &'a str,
    pub allowed: &'a str,
    pub handshake: u64,
}

pub fn parse_dump(out: &str, mut f: impl FnMut(DumpPeer)) {
    for line in out.lines().skip(1) {
        if line.is_empty() {
            continue;
        }
        let mut it = line.split('\t');
        let key = it.next().unwrap_or("");
        let _psk = it.next();
        let endpoint = it.next().unwrap_or("");
        let allowed = it.next().unwrap_or("");
        let handshake = it.next().and_then(|h| h.parse().ok()).unwrap_or(0);
        f(DumpPeer { key, endpoint, allowed: if allowed == "(none)" { "" } else { allowed }, handshake });
    }
}

pub fn wg_dump(out: &mut [u8]) -> Res<&str> {
    let o = run(&["wg", "show", "wg0", "dump"], out, 10_000)?;
    if o.code != 0 {
        return Err(o.failure(&["wg", "show", "wg0", "dump"]));
    }
    if o.truncated {
        return Err(Error::new(500, "wg show: too much output"));
    }
    let n = o.out.len();
    Ok(core::str::from_utf8(&out[..n]).unwrap_or(""))
}

fn sorted_eq(a: &str, b: &str) -> bool {
    let mut x = [""; 512];
    let mut y = [""; 512];
    let (mut nx, mut ny) = (0, 0);
    for p in a.split(',').filter(|s| !s.is_empty()) {
        if nx == 512 {
            return false;
        }
        x[nx] = p;
        nx += 1;
    }
    for p in b.split(',').filter(|s| !s.is_empty()) {
        if ny == 512 {
            return false;
        }
        y[ny] = p;
        ny += 1;
    }
    x[..nx].sort_unstable();
    y[..ny].sort_unstable();
    x[..nx] == y[..ny]
}

/// Makes wg0's peers exactly `peers` (burst hosts always keep the hub).
pub fn set_peers(cfg: &HostConfig, peers: &[Peer], scratch: &mut [u8]) -> Res<()> {
    for p in peers {
        if !valid_wg_key(p.public_key.as_str()) {
            return Err(Error::new(400, "bad WireGuard public key"));
        }
    }
    let hub = if cfg.hub {
        None
    } else {
        cfg.hub_peer.as_ref().filter(|h| !peers.iter().any(|p| p.public_key == h.public_key))
    };
    let dump = wg_dump(scratch)?;
    // remove peers not wanted
    let mut rm: [Str<64>; MAX_PEERS] = [Str::new(); MAX_PEERS];
    let mut nrm = 0;
    let mut have: [(Str<64>, Str<64>, bool); MAX_PEERS] = [(Str::new(), Str::new(), false); MAX_PEERS];
    let mut nhave = 0;
    let mut status = Ok(());
    parse_dump(dump, |d| {
        let wanted = peers.iter().chain(hub).find(|p| p.public_key.as_str() == d.key);
        match wanted {
            None => {
                if nrm < MAX_PEERS {
                    rm[nrm] = Str::lossy(d.key);
                    nrm += 1;
                } else {
                    status = Err(Error::new(500, "too many peers"));
                }
            }
            Some(p) => {
                let same = sorted_eq(d.allowed, p.allowed.as_str())
                    && (p.endpoint.is_empty() || p.endpoint.as_str() == d.endpoint);
                if nhave < MAX_PEERS {
                    have[nhave] = (Str::lossy(d.key), Str::new(), same);
                    nhave += 1;
                }
            }
        }
    });
    status?;
    for k in &rm[..nrm] {
        sh(&["wg", "set", "wg0", "peer", k.as_str(), "remove"])?;
    }
    for p in peers.iter().chain(hub) {
        if have[..nhave].iter().any(|(k, _, same)| *same && k.as_str() == p.public_key.as_str()) {
            continue;
        }
        set_peer(p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_routes() {
        let g =
            HostConfig::parse(br#"{"name":"g","role":"hub","gateway":true,"wgAddress":"10.201.0.1"}"#).ok().unwrap();
        let ok = "10.200.0.0/16 dev wg0 scope link src 10.0.0.6 \n10.200.0.0/15 via 10.0.0.1 dev enp7s0 \n10.244.0.0/16 via 10.0.0.1 dev enp7s0 table 51822 \n";
        assert!(!HostNet::routes_missing(&g, ok));
        // what the gateway node had after systemd-networkd restarted: only the Hetzner network's /15
        assert!(HostNet::routes_missing(
            &g,
            "10.200.0.0/15 via 10.0.0.1 dev enp7s0 \n10.201.0.0/24 dev wg0 proto kernel scope link src 10.201.0.1 \n"
        ));
        let b = HostConfig::parse(br#"{"name":"b","role":"burst","wgAddress":"10.201.0.10","controlCidr":"10.0.0.0/16","hub":{"publicKey":"k"}}"#).ok().unwrap();
        assert!(!HostNet::routes_missing(&b, "10.0.0.0/16 dev wg0 scope link \n10.201.1.1 dev wg0 scope link \n"));
        assert!(HostNet::routes_missing(&b, "10.201.1.1 dev wg0 scope link \n"));
    }

    #[test]
    fn rules() {
        let mut mem = [0u8; 4096];
        let mut b = Buf::new(&mut mem);
        sandbox_rules(&mut b, "sbx-abc", "10.200.0.10", "10.201.1.1", "10.0.0.0/16");
        let r = b.as_str();
        let v = veth_of("sbx-abc");
        assert_eq!(v.len(), 13);
        assert!(
            r.contains(
                {
                    let mut s: Str<200> = Str::new();
                    let _ = write!(s, "iifname \"{v}\" ip saddr 10.200.0.10 ip daddr 10.201.1.1 tcp dport 3128 accept");
                    s
                }
                .as_str()
            )
        );
        assert!(r.contains("ip saddr 10.0.0.0/16 tcp dport 7070 ip daddr 10.200.0.10 accept"));
        assert!(r.contains("counter drop"));
        assert!(!r.contains("ct state"));
        assert_eq!(mac_of("10.200.0.10").as_str(), "02:5b:0a:c8:00:0a");
        // same as the Node code: sha256("sbx-abc")[..12]
        assert_eq!(h12("sbx-abc").as_str(), "8709bfe435f5");
        let cfg = HostConfig::parse(br#"{"name":"h","role":"hub","wgAddress":"10.201.0.1"}"#).ok().unwrap();
        let mut mem = [0u8; 4096];
        let mut b = Buf::new(&mut mem);
        host_rules(&mut b, &cfg, "eth0", Some("enp7s0"), "", &[Str::lossy("10.244.3.5"), Str::lossy("10.244.6.7")]);
        assert!(
            b.as_str().contains("dnat to numgen random mod 2 map { 0 : 10.244.3.5, 1 : 10.244.6.7 }"),
            "{}",
            b.as_str()
        );
        assert!(!b.as_str().contains("snat") && !b.as_str().contains("masquerade"));
        assert!(
            b.as_str().contains("ip daddr 10.201.1.1 drop"),
            "the hub drops what reaches the egress address untranslated"
        );
        // a burst host forwards its sandboxes' egress traffic into wg0 untranslated: never dropped there
        let burst =
            HostConfig::parse(br#"{"name":"h","role":"burst","wgAddress":"10.201.0.10","hub":{"publicKey":"k"}}"#)
                .ok()
                .unwrap();
        let mut mem = [0u8; 4096];
        let mut b = Buf::new(&mut mem);
        host_rules(&mut b, &burst, "ens5", None, "", &[]);
        assert!(!b.as_str().contains("10.201.1.1") && !b.as_str().contains("egress_dnat"), "{}", b.as_str());
        assert!(b.as_str().contains("ip daddr 10.0.0.0/16 oifname != \"wg0\" drop"));
        // a gateway on a k3s node fences wg0's traffic only: the node's pods keep their own forwarding
        let gw =
            HostConfig::parse(br#"{"name":"g","role":"hub","gateway":true,"wgAddress":"10.201.0.1"}"#).ok().unwrap();
        let mut mem = [0u8; 4096];
        let mut b = Buf::new(&mut mem);
        host_rules(&mut b, &gw, "eth0", Some("enp7s0"), "10.0.0.6", &[Str::lossy("10.244.3.5")]);
        // the node's own pods reach sandboxes from its private address (not wg0's 10.201.0.1, which sandboxes drop)
        assert!(
            b.as_str().contains("oifname \"wg0\" ip saddr 10.244.0.0/16 ip daddr 10.200.0.0/16 snat to 10.0.0.6"),
            "{}",
            b.as_str()
        );
        let g = b.as_str();
        assert!(g.contains("dnat to numgen random mod 1 map { 0 : 10.244.3.5 }"));
        assert!(
            g.contains("iifname \"wg0\" oifname \"eth0\" drop") && g.contains("iifname \"eth0\" oifname \"wg0\" drop")
        );
        assert!(g.contains("    iifname \"wg0\" drop\n"));
        assert!(!g.contains("    iifname \"eth0\" drop\n") && !g.contains("    oifname \"eth0\" drop\n"));
        // the sandbox's own address survives the k3s node's masquerade (identity SNAT before iptables' nat)
        assert!(g.contains("type nat hook postrouting priority srcnat - 10"));
        assert!(g.contains(
            "iifname \"wg0\" ip saddr 10.200.0.0/16 snat ip prefix to ip saddr map { 10.200.0.0/16 : 10.200.0.0/16 }"
        ));
        assert!(
            HostConfig::parse(
                br#"{"name":"g","role":"burst","gateway":true,"wgAddress":"10.201.0.1","hub":{"publicKey":"k"}}"#
            )
            .is_err()
        );
        assert!(valid_wg_key("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ="));
        assert!(!valid_wg_key("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPB="));
        assert!(sorted_eq("10.0.0.1/32,10.0.0.2/32", "10.0.0.2/32,10.0.0.1/32"));
        let mut t = [Str::new(); MAX_TARGETS];
        let live = "table ip sbxhost {\n\tchain egress_dnat {\n\t\ttype nat hook prerouting priority dstnat; policy accept;\n\t\tip daddr 10.201.1.1 tcp dport 3128 dnat to numgen random mod 2 map { 0 : 10.244.3.5, 1 : 10.244.6.7 }\n\t}\n}";
        assert_eq!(targets_from_nft(live, &mut t), 2);
        assert_eq!((t[0].as_str(), t[1].as_str()), ("10.244.3.5", "10.244.6.7"));
    }
}
