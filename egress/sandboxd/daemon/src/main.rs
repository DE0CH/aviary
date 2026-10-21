//! sandboxd: the host agent of sandbox provider "vm" (docs/SANDBOX-VM.md), with a fixed memory budget
//! (docs/SANDBOXD-MEMORY.md). One per host. The controller (the web role) calls it over HTTPS with mutual TLS on the
//! host's overlay address; only the client certificate CN sandbox-controller is accepted.
//!
//! Start-up: config → budget → one region mapped (worker arenas, buffers, tables) → cgroups (sandboxd's own leaf with
//! memory.max/min = the budget; commands in a "helpers" leaf) → TLS config → host network, sandboxes from their state
//! files → all threads spawned → mlockall. After that nothing maps memory and sandboxd's own code doesn't allocate.
//! Stopping sandboxd never touches sandboxes (KillMode=process; state is in files and runsc).
// Errors carry their message inline (no heap), so they are a few hundred bytes by design.
#![allow(clippy::result_large_err)]

mod alloc;
mod api;
mod budget;
mod config;
mod fixed;
mod images;
mod json;
mod net;
mod paths;
mod proc;
mod runsc;
mod sandboxes;
mod server;
mod sock;
mod stack;
mod util;

use crate::api::{App, Facts, Worker};
use crate::budget::Budget;
use crate::config::HostConfig;
use crate::fixed::{Region, Str};
use crate::sandboxes::{Mig, Sandboxes, Scratch, Slot};
use crate::util::{Log, iso, now_ms, read_small};
use core::fmt::Write as _;

#[global_allocator]
static GLOBAL: alloc::ArenaAlloc = alloc::ArenaAlloc;

extern "C" fn on_term(_: i32) {
    let m = b"{\"msg\":\"sandboxd stopping (sandboxes keep running)\"}\n";
    unsafe {
        libc::write(1, m.as_ptr() as *const _, m.len());
        libc::_exit(0);
    }
}

fn fatal(msg: &str, detail: &str) -> ! {
    Log::error(msg).s("error", detail).emit();
    std::process::exit(1);
}

fn load_config() -> &'static HostConfig {
    let path = std::env::var("SBX_CONFIG").unwrap_or_else(|_| "/etc/sbx/host.json".into());
    let text = std::fs::read(&path).unwrap_or_else(|e| fatal("host.json", &e.to_string()));
    match HostConfig::parse(&text) {
        Ok(c) => Box::leak(Box::new(c)),
        Err(e) => fatal("host.json", e.msg.as_str()),
    }
}

fn write_cg(dir: &str, file: &str, v: &str) -> std::io::Result<()> {
    let p = format!("{dir}/{file}");
    std::fs::write(&p, v).map_err(|e| std::io::Error::new(e.kind(), format!("{p}: {e}")))
}

/// sandboxd → <unit cgroup>/daemon (memory.min = memory.max ≈ the budget), commands → <unit cgroup>/helpers
/// (memory.max = limits.helperMemoryMaxMiB). Needs Delegate=yes. Returns an fd of helpers/cgroup.procs.
fn setup_cgroups(b: &Budget, cfg: &HostConfig) -> std::io::Result<i32> {
    let own = std::fs::read_to_string("/proc/self/cgroup")?;
    let rel = own.lines().find_map(|l| l.strip_prefix("0::")).unwrap_or("/").trim().to_string();
    let rel = rel.strip_suffix("/daemon").unwrap_or(&rel).to_string();
    let base = format!("{}/sys/fs/cgroup{}", paths::root(), rel);
    let daemon = format!("{base}/daemon");
    let helpers = format!("{base}/helpers");
    std::fs::create_dir_all(&daemon)?;
    std::fs::create_dir_all(&helpers)?;
    write_cg(&daemon, "cgroup.procs", &std::process::id().to_string())?;
    // anything else still in the unit's own cgroup (left by an earlier sandboxd: runsc-fd-parking, a restore in
    // progress) goes to helpers, so the unit's cgroup has no processes and can enable controllers below it
    for pid in std::fs::read_to_string(format!("{base}/cgroup.procs"))?.lines() {
        let _ = write_cg(&helpers, "cgroup.procs", pid);
    }
    write_cg(&base, "cgroup.subtree_control", "+memory +pids")?;
    let max = b.total() + (8 << 20);
    write_cg(&daemon, "memory.max", &max.to_string())?;
    write_cg(&daemon, "memory.min", &b.total().to_string())?;
    let _ = write_cg(&daemon, "memory.swap.max", "0");
    write_cg(&helpers, "memory.max", &cfg.limits.helper_memory_max.to_string())?;
    let _ = write_cg(&helpers, "memory.swap.max", "0");
    let f = std::fs::OpenOptions::new().write(true).open(format!("{helpers}/cgroup.procs"))?;
    use std::os::fd::IntoRawFd;
    Ok(f.into_raw_fd())
}

fn facts(budget: Budget, gateway: bool) -> Facts {
    let mut out = vec![0u8; 64 << 10];
    if gateway {
        // a gateway runs no sandboxes and has no runsc
        return Facts {
            cpu_features: Str::new(),
            runsc: Str::new(),
            release: read_small::<100>("/usr/local/bin/gvisor-bin/.release").unwrap_or_default(),
            boot_id: read_small::<64>("/proc/sys/kernel/random/boot_id").unwrap_or_default(),
            started_at: iso(now_ms()),
            budget,
        };
    }
    let mut a = [""; 40];
    let mut cpu_features = Str::new();
    if let Ok(argv) = runsc::argv(&mut a, &["cpu-features"])
        && let Ok(o) = proc::run(argv, &mut out, 30_000)
    {
        cpu_features = Str::lossy(o.out_str().trim());
    }
    let mut a = [""; 40];
    let mut version = Str::new();
    if let Ok(argv) = runsc::argv(&mut a, &["--version"])
        && let Ok(o) = proc::run(argv, &mut out, 30_000)
    {
        version = Str::lossy(o.out_str().lines().next().unwrap_or("").trim());
    }
    Facts {
        cpu_features,
        runsc: version,
        release: read_small::<100>("/usr/local/bin/gvisor-bin/.release").unwrap_or_default(),
        boot_id: read_small::<64>("/proc/sys/kernel/random/boot_id").unwrap_or_default(),
        started_at: iso(now_ms()),
        budget,
    }
}

fn spawn_thread(name: &str, stack: usize, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.into())
        .stack_size(stack)
        .spawn(f)
        .unwrap_or_else(|e| fatal("thread", &e.to_string()));
}

fn start() {
    stack::paint(stack::OTHER);
    let cfg = load_config();
    let l = cfg.limits;
    let budget = Budget::of(&l, cfg.hub);
    // the region: every fixed buffer and table, and the TLS arenas
    let mut region = Region::map(budget.region()).unwrap_or_else(|e| fatal("mmap", &e.to_string()));
    let arenas = region.raw(l.max_connections * l.tls_arena);
    // SAFETY: the arenas' memory is handed to the allocator only, once.
    unsafe { alloc::init_arenas(arenas, l.max_connections, l.tls_arena) };
    let slots: &'static mut [Slot] = region.slice(Sandboxes::slot_count(&l));
    let migs: &'static mut [Mig] = region.slice(l.max_migrations);
    let mut workers = Vec::with_capacity(l.max_connections);
    for _ in 0..l.max_connections {
        workers.push(Worker {
            head: region.slice::<u8>(budget::HEAD),
            body: region.slice::<u8>(budget::body_size(&l)),
            out: region.slice::<u8>(budget::out_size(&l)),
            big: region.slice::<u8>(budget::BIG),
            small: region.slice::<u8>(budget::SMALL),
        });
    }
    let images = if cfg.hub { l.image_connections } else { 0 };
    let mut image_bufs = Vec::new();
    for _ in 0..images {
        image_bufs.push((region.slice::<u8>(budget::IMAGE_BUF), region.slice::<u8>(budget::HEAD)));
    }
    let bg_big = region.slice::<u8>(budget::BIG);
    let bg_small = region.slice::<u8>(budget::SMALL);
    let mut mig_bufs = Vec::new();
    for _ in 0..l.max_migrations {
        mig_bufs.push((region.slice::<u8>(budget::MIG_BIG), region.slice::<u8>(budget::MIG_SMALL)));
    }

    if l.cgroups {
        match setup_cgroups(&budget, cfg) {
            Ok(fd) => proc::set_helpers_cgroup(fd),
            Err(e) => fatal("cgroups (the unit needs Delegate=yes; or limits.cgroups false)", &e.to_string()),
        }
    }
    let tls = server::tls_config(cfg.tls_cert.as_str(), cfg.tls_key.as_str(), cfg.tls_ca.as_str())
        .unwrap_or_else(|e| fatal("tls", e.as_str()));

    let app: &'static App = Box::leak(Box::new(App {
        cfg,
        net: net::HostNet::new(),
        sbx: Sandboxes::new(cfg, slots, migs),
        facts: facts(budget, cfg.gateway),
        tls,
    }));
    if let Err(e) = app.net.init(cfg) {
        fatal("host network", e.msg.as_str());
    }
    let mut sc = Scratch { big: &mut *bg_big, small: &mut *bg_small };
    if !cfg.gateway {
        if let Err(e) = app.sbx.init(&mut sc) {
            fatal("sandboxes", e.msg.as_str());
        }
        app.sbx.sweep(&mut sc);
    }

    let api =
        sock::listen(cfg.wg_address.as_str(), cfg.api_port, 128).unwrap_or_else(|e| fatal("listen", &e.to_string()));
    let api: &'static std::net::TcpListener = Box::leak(Box::new(api));
    for (i, mut w) in workers.into_iter().enumerate() {
        spawn_thread("api", budget::API_STACK, move || {
            stack::paint(stack::API);
            alloc::use_arena(i + 1);
            alloc::set_zone(alloc::zone::APP);
            api::api_worker(app, api, &mut w)
        });
    }
    if cfg.hub {
        let img = sock::listen(cfg.wg_address.as_str(), cfg.image_port, 32)
            .unwrap_or_else(|e| fatal("listen", &e.to_string()));
        let img: &'static std::net::TcpListener = Box::leak(Box::new(img));
        for (buf, head) in image_bufs {
            spawn_thread("images", budget::OTHER_STACK, move || {
                stack::paint(stack::OTHER);
                alloc::set_zone(alloc::zone::APP);
                api::image_worker(app, img, buf, head)
            });
        }
    }
    for (i, (big, small)) in mig_bufs.into_iter().enumerate() {
        spawn_thread("migration", budget::OTHER_STACK, move || {
            stack::paint(stack::OTHER);
            alloc::set_zone(alloc::zone::APP);
            let mut sc = Scratch { big, small };
            app.sbx.migration_thread(i, &mut sc)
        });
    }
    // the region stays mapped for the life of the process (its slices are 'static)
    let _ = region;

    if l.mlock && unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) } != 0 {
        fatal("mlockall", &std::io::Error::last_os_error().to_string());
    }
    let s = alloc::stats();
    let mut addr: Str<32> = Str::new();
    let _ = write!(addr, "{}:{}", cfg.wg_address, cfg.api_port);
    Log::info("sandboxd listening")
        .s("name", cfg.name.as_str())
        .s("role", if cfg.hub { "hub" } else { "burst" })
        .s("address", addr.as_str())
        .s("runsc", app.facts.runsc.as_str())
        .s("release", app.facts.release.as_str())
        .n("sandboxes", app.sbx.count() as u64)
        .n("budget", budget.total())
        .n("rssBound", budget.rss() as u64)
        .n("rss", api::rss())
        .n("bootUsed", s.boot_used as u64)
        .emit();

    // this thread: the background jobs (CPU samples every 10 s, sweep every 5 min)
    alloc::set_zone(alloc::zone::APP);
    let mut n = 0u64;
    loop {
        util::sleep_ms(10_000);
        if cfg.gateway {
            continue;
        }
        app.sbx.sample_cpu();
        n += 1;
        if n.is_multiple_of(30) {
            app.sbx.sweep(&mut sc);
        }
    }
}

fn main() {
    paths::init();
    proc::init_runsc();
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--version") {
        println!("sandboxd {} (rust)", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("budget") {
        let cfg = if let Some(p) = args.get(2) {
            let t = std::fs::read(p).unwrap_or_else(|e| fatal("config", &e.to_string()));
            HostConfig::parse(&t).unwrap_or_else(|e| fatal("config", e.msg.as_str()))
        } else {
            HostConfig::parse(br#"{"name":"x","role":"burst","wgAddress":"10.201.0.10","hub":{"publicKey":"x"}}"#)
                .unwrap_or_else(|_| unreachable!())
        };
        let b = Budget::of(&cfg.limits, cfg.hub);
        let mut s = String::new();
        let _ = writeln!(s, "{:?}", cfg.limits);
        b.write_table(&mut s);
        let _ = writeln!(s, "total bytes: {}", b.total());
        print!("{s}");
        return;
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    spawn_thread("start", budget::START_STACK, start);
    loop {
        std::thread::park();
    }
}
