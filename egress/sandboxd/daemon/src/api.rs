//! The API (same as the Node sandboxd; docs/SANDBOX-VM.md "sandboxd"):
//!
//!   GET    /v1/host                        host facts: capacity, CPU features, images, WireGuard key/peers, sandboxes
//!                                          (+ `memory`: sandboxd's own budget and use; new, additive)
//!   PUT    /v1/peers {peers}               wg0's peers, exactly these
//!   PUT    /v1/egress {targets}            hub: egress pod addresses behind the egress VIP
//!   POST   /v1/images {ref?, digest?}      make sure an image is unpacked here; returns its digest
//!   GET    /v1/sandboxes                   the sandboxes here
//!   POST   /v1/sandboxes {spec}            create and start one (idempotent for the same name and ip)
//!   DELETE /v1/sandboxes/{name}            remove it (a missing one is fine)
//!   POST   /v1/sandboxes/{name}/receive {spec}   migration target: prepare and listen; returns {port, address}
//!   GET    /v1/sandboxes/{name}/receive?wait=ms  migration target: the receive's result
//!   GET    /v1/sandboxes/{name}/spec       its spec
//!   POST   /v1/sandboxes/{name}/send {to, downtimeMs?, maxRounds?, maxDowntimeMs?}   migration source
//!
//! Every handler writes its JSON answer into the worker's fixed `out` buffer.
use crate::config::{Peer, Spec, parse_peer};
use crate::fixed::{Buf, Str};
use crate::json::{self, Val, W};
use crate::net::{self, HostNet, MAX_PEERS, MAX_TARGETS};
use crate::sandboxes::{Sandboxes, Scratch};
use crate::server::{self, Conn, IoErr, ReadErr, Req, Stream, Tls, error_body, respond};
use crate::util::{Error, Log, Res, in_cidr, read_small};
use crate::{alloc, err, images};
use core::fmt::Write as _;
use rustls::ServerConfig;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// Facts read once at start.
pub struct Facts {
    pub cpu_features: Str<2048>,
    pub runsc: Str<200>,
    pub release: Str<100>,
    pub boot_id: Str<64>,
    pub started_at: Str<24>,
    pub budget: crate::budget::Budget,
}

pub struct App {
    pub cfg: &'static crate::config::HostConfig,
    pub net: HostNet,
    pub sbx: Sandboxes,
    pub facts: Facts,
    pub tls: Arc<ServerConfig>,
}

/// One API worker's buffers (from the region).
pub struct Worker {
    pub head: &'static mut [u8],
    pub body: &'static mut [u8],
    pub out: &'static mut [u8],
    pub big: &'static mut [u8],
    pub small: &'static mut [u8],
}

pub fn set_sock_opts(s: &TcpStream) {
    let _ = s.set_nodelay(true);
    crate::sock::bound_buffers(s);
}

/// The worker loop: one connection at a time, each in catch_unwind (a bug answers nothing and closes that
/// connection; the process and the other workers go on).
pub fn api_worker(app: &'static App, l: &TcpListener, w: &mut Worker) -> ! {
    loop {
        let Ok((sock, _)) = l.accept() else {
            crate::util::sleep_ms(10);
            continue;
        };
        set_sock_opts(&sock);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| connection(app, sock, w)));
        if r.is_err() {
            Log::error("request handler panicked; connection closed").emit();
        }
        let (used, _) = alloc::current_usage();
        if used != 0 {
            Log::error("worker arena not empty after a connection").n("bytes", used as u64).emit();
        }
    }
}

fn connection(app: &App, sock: TcpStream, w: &mut Worker) {
    let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let _ = sock.set_write_timeout(Some(std::time::Duration::from_secs(10)));
    let deadline = crate::util::mono_ms() + 10_000;
    let mut tls = match Tls::accept(&app.tls, sock, deadline) {
        Ok(t) => t,
        Err(IoErr::Budget) => {
            Log::error("TLS handshake refused: connection memory budget").emit();
            return;
        }
        Err(IoErr::Closed) => return,
    };
    let controller = tls.peer_cn().is_some_and(|cn| cn.as_str() == server::CONTROLLER_CN);
    let mut conn = Conn { head: &mut *w.head, have: 0 };
    loop {
        let req = match conn.next(&mut tls, w.body) {
            Ok(r) => r,
            Err(ReadErr::Closed) => break,
            Err(ReadErr::Budget) => {
                Log::error("connection closed: memory budget").emit();
                break;
            }
            Err(ReadErr::Status(st, msg)) => {
                let _ = respond(&mut tls, st, error_body(w.out, msg), true);
                break;
            }
        };
        let close = req.close;
        let t0 = crate::util::mono_ms();
        let (status, len) = if !controller {
            let b = error_body(w.out, "not the controller");
            (403, b.len())
        } else {
            let mut sc = Scratch { big: &mut *w.big, small: &mut *w.small };
            handle(app, &req, w.out, &mut sc)
        };
        if status >= 500 {
            let msg = core::str::from_utf8(&w.out[..len]).unwrap_or("");
            Log::error("request")
                .s("method", req.method)
                .s("url", req.path)
                .n("status", status as u64)
                .j("body", msg.as_bytes())
                .n("ms", crate::util::mono_ms() - t0)
                .emit();
        }
        if respond(&mut tls, status, &w.out[..len], close || !controller).is_err() || close || !controller {
            break;
        }
    }
    tls.close();
}

/// Runs one request; returns (status, length of the body in `out`).
pub fn handle(app: &App, req: &Req, out: &mut [u8], sc: &mut Scratch) -> (u16, usize) {
    let mut b = Buf::new(out);
    let r = route(app, req, &mut W::new(&mut b), sc);
    match r {
        Ok(status) if !b.full => (status, b.len),
        Ok(_) => {
            let n = error_body(out, "response too large").len();
            (500, n)
        }
        Err(e) => {
            let n = error_body(out, e.msg.as_str()).len();
            (e.status, n)
        }
    }
}

fn body_obj<'a>(req: &Req<'a>) -> Res<&'a [u8]> {
    if req.body.iter().all(|c| c.is_ascii_whitespace()) {
        return Ok(b"{}");
    }
    match json::parse(req.body)? {
        Val::Obj(o) => Ok(o),
        _ => Ok(b"{}"),
    }
}

fn get_str<const N: usize>(o: &[u8], key: &str) -> Res<Option<Str<N>>> {
    let mut r = None;
    json::object::<Error>(o, |k, v| {
        if k.is(key) {
            let mut tmp = [0u8; 1024];
            r = match v {
                Val::Str(s) => Some(Str::lossy(s.decode(&mut tmp).ok_or_else(|| Error::new(400, "value too long"))?)),
                Val::Num(n) => {
                    let mut s = Str::new();
                    let _ = write!(s, "{n}");
                    Some(s)
                }
                Val::Null => None,
                Val::Bool(b) => Some(Str::lossy(if b { "true" } else { "false" })),
                _ => None,
            };
        }
        Ok(())
    })?;
    Ok(r)
}

fn get_num(o: &[u8], key: &str) -> Res<Option<f64>> {
    let mut r = None;
    json::object::<Error>(o, |k, v| {
        if k.is(key) {
            r = match v {
                Val::Num(n) => Some(n),
                Val::Str(s) => {
                    let mut tmp = [0u8; 64];
                    s.decode(&mut tmp).and_then(|t| t.trim().parse().ok())
                }
                _ => None,
            };
        }
        Ok(())
    })?;
    Ok(r)
}

fn clamp(v: Option<f64>, default: f64, lo: f64, hi: f64) -> f64 {
    let v = v.filter(|x| x.is_finite()).unwrap_or(default);
    v.max(lo).min(hi)
}

fn route(app: &App, req: &Req, w: &mut W, sc: &mut Scratch) -> Res<u16> {
    let m = req.method;
    let path = req.path;
    let mut parts: [&str; 6] = [""; 6];
    let mut np = 0;
    for p in path.split('/').filter(|p| !p.is_empty()) {
        if np == parts.len() {
            np += 1;
            break;
        }
        parts[np] = p;
        np += 1;
    }
    if m == "GET" && path == "/v1/host" {
        host(app, w, sc)?;
        return Ok(200);
    }
    if m == "PUT" && path == "/v1/peers" {
        let o = body_obj(req)?;
        let mut peers = [Peer::default(); MAX_PEERS];
        let mut n = 0;
        let mut is_list = false;
        json::object::<Error>(o, |k, v| {
            if let (true, Val::Arr(a)) = (k.is("peers"), v) {
                {
                    is_list = true;
                    json::array::<Error>(a, |e| {
                        let Val::Obj(po) = e else { return Err(Error::new(400, "peers must be a list of objects")) };
                        if n == MAX_PEERS {
                            return Err(Error::new(400, "too many peers"));
                        }
                        peers[n] = parse_peer(po)?;
                        n += 1;
                        Ok(())
                    })?;
                }
            }
            Ok(())
        })?;
        if !is_list {
            return Err(Error::new(400, "peers must be a list"));
        }
        net::set_peers(app.cfg, &peers[..n], sc.big)?;
        w.obj().key("peers");
        write_peers(w, sc)?;
        w.end();
        return Ok(200);
    }
    if m == "PUT" && path == "/v1/egress" {
        if !app.cfg.hub {
            return Err(Error::new(400, "only the hub translates the egress address"));
        }
        let o = body_obj(req)?;
        let mut t = [Str::<16>::new(); MAX_TARGETS];
        let mut n = 0;
        json::object::<Error>(o, |k, v| {
            if let (true, Val::Arr(a)) = (k.is("targets"), v) {
                {
                    json::array::<Error>(a, |e| {
                        let mut tmp = [0u8; 64];
                        if let Some(s) = e.str().and_then(|s| s.decode(&mut tmp)) {
                            if n == MAX_TARGETS {
                                return Err(Error::new(400, "too many targets"));
                            }
                            t[n] = Str::lossy(s);
                            n += 1;
                        }
                        Ok(())
                    })?;
                }
            }
            Ok(())
        })?;
        app.net.set_egress_targets(app.cfg, &t[..n])?;
        let st = app.net.lock();
        w.obj().key("targets").arr();
        for x in &st.targets[..st.ntargets] {
            w.str(x.as_str());
        }
        w.end_arr().end();
        return Ok(200);
    }
    if m == "POST" && path == "/v1/images" {
        let o = body_obj(req)?;
        let digest: Option<Str<128>> = get_str(o, "digest")?.filter(|s: &Str<128>| !s.is_empty());
        let r: Option<Str<300>> = get_str(o, "ref")?.filter(|s: &Str<300>| !s.is_empty());
        let d = images::ensure_image(
            app.cfg,
            &app.sbx.pulls,
            digest.as_ref().map(|s| s.as_str()),
            r.as_ref().map(|s| s.as_str()),
        )?;
        w.obj().kstr("digest", d.as_str()).end();
        return Ok(200);
    }
    if np >= 2 && parts[0] == "v1" && parts[1] == "sandboxes" {
        if app.cfg.gateway {
            if np == 2 && m == "GET" {
                w.obj().key("sandboxes").arr().end_arr().end();
                return Ok(200);
            }
            return Err(Error::new(409, "a gateway runs no sandboxes"));
        }
        if np == 2 && m == "GET" {
            w.obj().key("sandboxes");
            app.sbx.write_list(w);
            w.end();
            return Ok(200);
        }
        if np == 2 && m == "POST" {
            let spec = Spec::parse(req.body, app.cfg.sandbox_cidr.as_str())?;
            app.sbx.create(&spec, sc, w)?;
            return Ok(200);
        }
        let name = parts[2];
        if np > 2 && !crate::config::valid_name(name) {
            return Err(Error::new(400, "bad sandbox name"));
        }
        if np == 3 && m == "DELETE" {
            app.sbx.delete(name)?;
            w.obj().kstr("deleted", name).end();
            return Ok(200);
        }
        if np == 4 && parts[3] == "spec" && m == "GET" {
            let e = app.sbx.get(name).ok_or_else(|| err!(404, "{name} is not here"))?;
            e.spec.write(w);
            return Ok(200);
        }
        if np == 4 && parts[3] == "receive" && m == "POST" {
            let spec = Spec::parse(req.body, app.cfg.sandbox_cidr.as_str())?;
            if spec.name.as_str() != name {
                return Err(Error::new(400, "name mismatch"));
            }
            let (port, address) = app.sbx.receive(&spec, sc)?;
            w.obj().ku64("port", port as u64).kstr("address", address).end();
            return Ok(200);
        }
        if np == 4 && parts[3] == "receive" && m == "GET" {
            let wait = req
                .query
                .split('&')
                .find_map(|kv| kv.strip_prefix("wait="))
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|x| x.is_finite())
                .unwrap_or(0.0);
            let wait = wait.clamp(0.0, 120_000.0) as u64;
            app.sbx.receive_result(name, wait, w);
            return Ok(200);
        }
        if np == 4 && parts[3] == "send" && m == "POST" {
            let o = body_obj(req)?;
            let to: Str<64> = get_str(o, "to")?.unwrap_or_default();
            let (host, port) = to.as_str().split_once(':').unwrap_or((to.as_str(), ""));
            if host.is_empty()
                || !in_cidr(host, app.cfg.host_cidr.as_str())
                || port.is_empty()
                || !port.bytes().all(|c| c.is_ascii_digit())
            {
                return Err(Error::new(400, "to must be <overlay host address>:<port>"));
            }
            let downtime = clamp(get_num(o, "downtimeMs")?, 100.0, 10.0, 5000.0) as u64;
            let rounds = clamp(get_num(o, "maxRounds")?, 20.0, 1.0, 50.0) as u64;
            let max_downtime = clamp(get_num(o, "maxDowntimeMs")?, 0.0, 0.0, 60_000.0) as u64;
            let ms = app.sbx.send(name, to.as_str(), downtime, rounds, max_downtime, sc)?;
            w.obj().ku64("ms", ms).end();
            return Ok(200);
        }
    }
    Err(Error::new(404, "not found"))
}

fn write_peers(w: &mut W, sc: &mut Scratch) -> Res<()> {
    let dump = net::wg_dump(sc.small)?;
    w.arr();
    net::parse_dump(dump, |d| {
        w.obj().kstr("publicKey", d.key).kstr("endpoint", d.endpoint).key("allowedIps").arr();
        for a in d.allowed.split(',').filter(|s| !s.is_empty()) {
            w.str(a);
        }
        w.end_arr().ku64("latestHandshake", d.handshake).end();
    });
    w.end_arr();
    Ok(())
}

fn meminfo() -> (u64, u64) {
    let mut b = [0u8; 4096];
    let Ok(t) = crate::util::read_file("/proc/meminfo", &mut b) else { return (0, 0) };
    let t = core::str::from_utf8(t).unwrap_or("");
    let kb = |k: &str| -> u64 {
        t.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix(':')))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    (kb("MemTotal"), kb("MemAvailable"))
}

/// This process's resident memory (VmRSS), bytes.
pub fn rss() -> u64 {
    let mut b = [0u8; 2048];
    let Ok(t) = crate::util::read_file("/proc/self/status", &mut b) else { return 0 };
    let t = core::str::from_utf8(t).unwrap_or("");
    t.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        * 1024
}

fn host(app: &App, w: &mut W, sc: &mut Scratch) -> Res<()> {
    let cfg = app.cfg;
    let (total, avail) = meminfo();
    let (pk, public_if) = {
        let st = app.net.lock();
        (st.public_key, st.public_if)
    };
    w.obj()
        .kstr("name", cfg.name.as_str())
        .kstr("role", if cfg.hub { "hub" } else { "burst" })
        .key("gateway")
        .bool(cfg.gateway);
    w.kstr("wgAddress", cfg.wg_address.as_str()).kstr("wgPublicKey", pk.as_str()).ku64("wgPort", cfg.wg_port as u64);
    w.key("publicIpv6");
    match net::public_ipv6(public_if.as_str()) {
        Some(a) => w.str(a.as_str()),
        None => w.null(),
    };
    w.key("peers");
    write_peers(w, sc)?;
    {
        let st = app.net.lock();
        w.key("egressTargets").arr();
        for x in &st.targets[..st.ntargets] {
            w.str(x.as_str());
        }
        w.end_arr();
    }
    // a gateway offers no room for sandboxes
    w.ku64("memTotal", total)
        .ku64("memAvailable", avail)
        .ku64("reserveMemory", if cfg.gateway { total } else { cfg.reserve_memory });
    let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }.max(1) as u64;
    w.ku64("cpus", cpus);
    let la = read_small::<128>("/proc/loadavg").unwrap_or_default();
    w.key("loadAvg").arr();
    for x in la.as_str().split_whitespace().take(3) {
        w.f64(x.parse().unwrap_or(0.0));
    }
    w.end_arr();
    let up = read_small::<128>("/proc/uptime").unwrap_or_default();
    let up: f64 = up.as_str().split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
    w.kf64("uptimeS", up.round());
    let f = &app.facts;
    w.kstr("cpuFeatures", f.cpu_features.as_str())
        .kstr("runsc", f.runsc.as_str())
        .kstr("gvisorRelease", f.release.as_str());
    w.key("images").arr();
    images::each_image(|d| {
        w.str(d);
    });
    w.end_arr();
    w.kstr("bootId", f.boot_id.as_str()).kstr("startedAt", f.started_at.as_str());
    w.key("sandboxes");
    app.sbx.write_list(w);
    // new: sandboxd's own memory (docs/SANDBOXD-MEMORY.md)
    let s = alloc::stats();
    w.key("sandboxd")
        .obj()
        .kstr("implementation", "rust")
        .kstr("version", env!("CARGO_PKG_VERSION"))
        .key("memory")
        .obj();
    w.ku64("budget", f.budget.total())
        .ku64("rss", rss())
        .ku64("bootArena", s.boot_size as u64)
        .ku64("bootUsed", s.boot_used as u64)
        .ku64("bootPeak", s.boot_peak as u64);
    w.ku64("tlsArenas", s.arenas as u64)
        .ku64("tlsArenaSize", s.arena_size as u64)
        .ku64("tlsUsed", s.arena_used as u64)
        .ku64("tlsPeak", s.arena_peak as u64);
    w.ku64("apiStack", crate::budget::API_STACK as u64)
        .ku64("apiStackPeak", crate::stack::peak(crate::stack::API) as u64);
    w.ku64("otherStack", crate::budget::OTHER_STACK as u64)
        .ku64("otherStackPeak", crate::stack::peak(crate::stack::OTHER) as u64);
    w.ku64("stray", s.stray as u64).ku64("overflow", s.overflow as u64).ku64("failed", s.failed as u64);
    w.end().key("limits").obj();
    let l = &cfg.limits;
    w.ku64("maxConnections", l.max_connections as u64)
        .ku64("maxSandboxes", l.max_sandboxes as u64)
        .ku64("maxMigrations", l.max_migrations as u64);
    w.end().end();
    w.end();
    Ok(())
}

// ---- the image server (role hub; plain HTTP on the overlay, overlay hosts only) ----------------------------------

pub fn image_worker(app: &'static App, l: &TcpListener, buf: &mut [u8], head: &mut [u8]) -> ! {
    loop {
        let Ok((mut sock, peer)) = l.accept() else {
            crate::util::sleep_ms(10);
            continue;
        };
        set_sock_opts(&sock);
        let mut ip: Str<48> = Str::new();
        let _ = write!(ip, "{}", peer.ip());
        let ip = ip.as_str().trim_start_matches("::ffff:");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve_image(app, &mut sock, ip, buf, head)));
    }
}

fn serve_image(app: &App, sock: &mut TcpStream, from: &str, buf: &mut [u8], head: &mut [u8]) {
    let mut conn = Conn { head, have: 0 };
    let mut body = [0u8; 2048];
    let Ok(req) = conn.next(sock, &mut body) else { return };
    let empty = |s: &mut TcpStream, st: u16| {
        let mut h: Str<128> = Str::new();
        let _ = write!(h, "HTTP/1.1 {st} {}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n", server::reason(st));
        let _ = s.write_all_bytes(h.as_bytes());
    };
    if !in_cidr(from, app.cfg.host_cidr.as_str()) {
        return empty(sock, 403);
    }
    let digest =
        req.path.strip_prefix("/images/").and_then(|p| p.strip_suffix(".tar.zst")).filter(|d| images::valid_digest(d));
    let Some(d) = digest.filter(|d| req.method == "GET" && images::have_image(d)) else { return empty(sock, 404) };
    let dir = crate::path!("/var/lib/sbx/images/{d}");
    let Ok(c) = crate::proc::spawn(
        &["tar", "--zstd", "-C", dir.as_str(), "--exclude=./rootfs/.gvisor.filestore.*", "-cf", "-", "."],
        &[],
        crate::proc::Io::Null,
        crate::proc::Io::Pipe,
        crate::proc::Io::Null,
    ) else {
        return empty(sock, 500);
    };
    let _ = sock.set_write_timeout(Some(std::time::Duration::from_secs(60)));
    if sock.write_all_bytes(b"HTTP/1.1 200 OK\r\ncontent-type: application/zstd\r\nconnection: close\r\n\r\n").is_ok() {
        loop {
            let n = unsafe { libc::read(c.stdout, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if n <= 0 {
                break;
            }
            if sock.write_all_bytes(&buf[..n as usize]).is_err() {
                break;
            }
        }
    }
    crate::proc::kill(c.pid);
    unsafe { libc::close(c.stdout) };
    crate::proc::wait(c.pid, true);
}
