//! Sandboxes on this host: create, delete, list, and both ends of a pre-copy live migration (docs/SANDBOX-VM.md,
//! "Migration"). State per sandbox in /var/lib/sbx/sandboxes/<name>.json (mode 600, the same format as the Node
//! sandboxd wrote), so a restart finds its sandboxes again: they run on in their own cgroups (KillMode=process).
//!
//! Memory: the entries, the per-name operation locks and the migration slots are fixed tables carved from the region
//! at start (sizes from `limits`); a full table is a 503, never growth.
use crate::config::{HostConfig, Spec};
use crate::fixed::{Buf, Str, Zeroed};
use crate::images::{self, Pulls};
use crate::json::{self, Val, W};
use crate::net;
use crate::proc::{self, Io, collect, quiet, run, spawn};
use crate::runsc;
use crate::util::{Error, Log, Res, each_dir_entry, iso, mono_ms, now_ms, read_file};
use crate::{err, path, paths};
use core::fmt::Write as _;
use std::sync::{Condvar, Mutex, MutexGuard};

pub const RUNNING: u8 = 1;
pub const RECEIVING: u8 = 2;

const PORT_MIN: u16 = 7100;
const PORT_MAX: u16 = 7199;

#[derive(Clone, Copy)]
pub struct Entry {
    pub spec: Spec,
    pub state: u8,
    pub created_at: Str<24>,
    /// receiving: the port the restore listens on
    pub port: u16,
    /// the last migration stream's statistics (target side), raw JSON; empty = none
    pub migrated: Str<1024>,
    cpu_at: u64,
    cpu_usec: u64,
    cpu_rate: f64,
    cpu_have: bool,
}
unsafe impl Zeroed for Entry {}

#[derive(Clone, Copy)]
pub struct Slot {
    /// the name this slot is locked for or holds an entry of (empty: free)
    name: Str<48>,
    used: bool,
    busy: bool,
    waiters: u32,
    entry: Entry,
}
unsafe impl Zeroed for Slot {}

#[derive(Clone, Copy)]
pub struct Mig {
    name: Str<48>,
    pid: i32,
    /// the restore child runs
    active: bool,
    /// a slot thread waits for pid
    assigned: bool,
    done: bool,
    in_use: bool,
    error: Str<200>,
    finished_at: u64,
}
unsafe impl Zeroed for Mig {}

/// Scratch space an operation may use (a worker's or a background thread's own buffers).
pub struct Scratch<'a> {
    pub big: &'a mut [u8],
    pub small: &'a mut [u8],
}

pub struct Sandboxes {
    pub cfg: &'static HostConfig,
    slots: Mutex<&'static mut [Slot]>,
    cv: Condvar,
    migs: Mutex<&'static mut [Mig]>,
    mcv: Condvar,
    sends: Mutex<usize>,
    pub pulls: Pulls,
    max: usize,
}

/// Holding the operation lock of one sandbox name (KeyedLock in the Node code).
pub struct OpLock<'a> {
    s: &'a Sandboxes,
    idx: usize,
}

impl Drop for OpLock<'_> {
    fn drop(&mut self) {
        let mut t = self.s.table();
        let sl = &mut t[self.idx];
        sl.busy = false;
        if !sl.used && sl.waiters == 0 {
            sl.name.clear();
        }
        self.s.cv.notify_all();
    }
}

fn status_of(out: &str) -> Str<16> {
    let mut st: Str<16> = Str::lossy("unknown");
    if let Ok(Val::Obj(o)) = json::parse(out.as_bytes()) {
        let _ = json::object::<Error>(o, |k, v| {
            if k.is("status") {
                let mut b = [0u8; 16];
                if let Some(s) = v.str().and_then(|s| s.decode(&mut b)) {
                    st = Str::lossy(s);
                }
            }
            Ok(())
        });
    }
    st
}

impl Sandboxes {
    pub fn new(cfg: &'static HostConfig, slots: &'static mut [Slot], migs: &'static mut [Mig]) -> Sandboxes {
        Sandboxes {
            cfg,
            slots: Mutex::new(slots),
            cv: Condvar::new(),
            migs: Mutex::new(migs),
            mcv: Condvar::new(),
            sends: Mutex::new(0),
            pulls: Pulls::new(),
            max: cfg.limits.max_sandboxes,
        }
    }

    /// Slots needed for `limits`: every sandbox, plus a lock for each request and migration in flight.
    pub fn slot_count(l: &crate::config::Limits) -> usize {
        l.max_sandboxes + l.max_connections + l.max_migrations + 4
    }

    fn table(&self) -> MutexGuard<'_, &'static mut [Slot]> {
        self.slots.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn migt(&self) -> MutexGuard<'_, &'static mut [Mig]> {
        self.migs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Serialises operations per sandbox name.
    pub fn lock(&self, name: &str) -> Res<OpLock<'_>> {
        let mut t = self.table();
        let idx = match t.iter().position(|s| s.name.as_str() == name) {
            Some(i) => i,
            None => {
                let i = t
                    .iter()
                    .position(|s| s.name.is_empty() && !s.used && !s.busy && s.waiters == 0)
                    .ok_or_else(|| Error::new(503, "too many operations at once"))?;
                t[i].name = Str::lossy(name);
                i
            }
        };
        let deadline = mono_ms() + 20 * 60_000;
        while t[idx].busy {
            if mono_ms() > deadline {
                return Err(err!(503, "{name} is busy"));
            }
            t[idx].waiters += 1;
            t = self
                .cv
                .wait_timeout(t, std::time::Duration::from_secs(5))
                .map(|r| r.0)
                .unwrap_or_else(|p| p.into_inner().0);
            t[idx].waiters -= 1;
        }
        t[idx].busy = true;
        Ok(OpLock { s: self, idx })
    }

    /// A copy of the entry (None if not here).
    pub fn get(&self, name: &str) -> Option<Entry> {
        let t = self.table();
        t.iter().find(|s| s.used && s.name.as_str() == name).map(|s| s.entry)
    }

    pub fn count(&self) -> usize {
        self.table().iter().filter(|s| s.used).count()
    }

    fn state_file(name: &str) -> paths::P {
        path!("/var/lib/sbx/sandboxes/{name}.json")
    }

    /// Stores the entry (in the table, under the op lock's slot) and its state file.
    fn save(&self, lk: &OpLock, e: &Entry, buf: &mut [u8]) -> Res<()> {
        {
            let mut t = self.table();
            let sl = &mut t[lk.idx];
            let keep = if sl.used {
                (sl.entry.cpu_at, sl.entry.cpu_usec, sl.entry.cpu_rate, sl.entry.cpu_have)
            } else {
                (0, 0, 0.0, false)
            };
            sl.entry = *e;
            (sl.entry.cpu_at, sl.entry.cpu_usec, sl.entry.cpu_rate, sl.entry.cpu_have) = keep;
            sl.used = true;
        }
        let mut b = Buf::new(buf);
        let mut w = W::new(&mut b);
        w.obj().key("spec");
        e.spec.write(&mut w);
        w.kstr("state", if e.state == RUNNING { "running" } else { "receiving" })
            .kstr("createdAt", e.created_at.as_str());
        if e.port != 0 {
            w.ku64("port", e.port as u64);
        }
        if !e.migrated.is_empty() {
            w.key("migrated").raw(e.migrated.as_bytes());
        }
        w.end();
        if b.full {
            return Err(Error::new(500, "state too large"));
        }
        let n = b.len;
        crate::util::write_file_atomic(Self::state_file(e.spec.name.as_str()).as_str(), &buf[..n], 0o600)?;
        Ok(())
    }

    fn forget(&self, lk: &OpLock, name: &str) {
        {
            let mut t = self.table();
            t[lk.idx].used = false;
        }
        crate::util::remove_file(Self::state_file(name).as_str());
    }

    fn parse_state(text: &[u8], sandbox_cidr: &str) -> Res<Entry> {
        let mut spec: Option<Spec> = None;
        let mut state = 0u8;
        let mut created: Str<24> = Str::new();
        let mut port = 0u16;
        let mut migrated: Str<1024> = Str::new();
        let Val::Obj(o) = json::parse(text)? else { return Err(Error::new(500, "bad state")) };
        json::object::<Error>(o, |k, v| {
            if k.is("spec") {
                if let Val::Obj(so) = v {
                    spec = Some(Spec::parse_obj(so, sandbox_cidr)?);
                }
            } else if k.is("state") {
                state = if v.str().is_some_and(|s| s.is("running")) { RUNNING } else { RECEIVING };
            } else if k.is("createdAt") {
                let mut b = [0u8; 64];
                created = Str::lossy(v.str().and_then(|s| s.decode(&mut b)).unwrap_or(""));
            } else if k.is("port") {
                port = v.num().unwrap_or(0.0) as u16;
            } else if k.is("migrated") {
                // keep the raw JSON (an object or null) if it fits
                let raw: &[u8] = match v {
                    Val::Obj(r) => r,
                    Val::Null => b"null",
                    _ => b"",
                };
                if let Ok(s) = core::str::from_utf8(raw) {
                    migrated = Str::from(s).unwrap_or_default();
                }
            }
            Ok(())
        })?;
        let spec = spec.ok_or_else(|| Error::new(500, "state without spec"))?;
        Ok(Entry {
            spec,
            state,
            created_at: created,
            port,
            migrated,
            cpu_at: 0,
            cpu_usec: 0,
            cpu_rate: 0.0,
            cpu_have: false,
        })
    }

    pub fn status(&self, name: &str, out: &mut [u8]) -> Str<16> {
        let mut a = [""; 40];
        let Ok(argv) = runsc::argv(&mut a, &["state", name]) else { return Str::lossy("unknown") };
        match run(argv, out, 10_000) {
            Ok(o) if o.code == 0 => status_of(o.out_str()),
            Ok(_) => Str::lossy("missing"),
            Err(_) => Str::lossy("unknown"),
        }
    }

    /// At start: read state files; drop sandboxes that no longer run (e.g. after a reboot).
    pub fn init(&self, sc: &mut Scratch) -> Res<()> {
        for d in [paths::sandboxes(), paths::bundles(), paths::images(), paths::logs(), paths::ckpt()] {
            crate::util::mkdir(d.as_str(), 0o700)?;
        }
        // names first (the directory listing must not be held while we run commands)
        let mut names: [Str<48>; 256] = [Str::new(); 256];
        let mut n = 0;
        each_dir_entry(paths::sandboxes().as_str(), |f| {
            if let Some(stem) = f.strip_suffix(".json")
                && n < names.len()
                && crate::config::valid_name(stem)
            {
                names[n] = Str::lossy(stem);
                n += 1;
            }
        })?;
        for name in &names[..n] {
            let name = name.as_str();
            let e = match read_file(Self::state_file(name).as_str(), sc.big)
                .map_err(Error::from)
                .and_then(|t| Self::parse_state(t, self.cfg.sandbox_cidr.as_str()))
            {
                Ok(e) => e,
                Err(e) => {
                    Log::error("unreadable state file, removed").s("sandbox", name).s("error", e.msg.as_str()).emit();
                    crate::util::remove_file(Self::state_file(name).as_str());
                    continue;
                }
            };
            let lk = self.lock(name)?;
            let st = self.status(name, sc.small);
            if st.as_str() == "running" {
                let mut e = e;
                if e.state == RECEIVING {
                    e.state = RUNNING;
                    e.port = 0;
                    net::route_sandbox(name, e.spec.ip.as_str())?;
                }
                if self.count() >= self.max {
                    Log::error("more sandboxes than limits.maxSandboxes; not managing").s("sandbox", name).emit();
                    continue;
                }
                self.save(&lk, &e, sc.big)?;
            } else {
                Log::info("sandbox gone while sandboxd was down").s("sandbox", name).s("status", st.as_str()).emit();
                self.cleanup(&lk, name, Some(e.spec.ip.as_str()));
            }
        }
        Ok(())
    }

    /// Every 10 s: CPU use per sandbox (cores, averaged over the interval).
    pub fn sample_cpu(&self) {
        let now = mono_ms();
        let mut t = self.table();
        for sl in t.iter_mut().filter(|s| s.used) {
            let p = paths::sbx_cgroup(sl.entry.spec.name.as_str(), "cpu.stat");
            let mut b = [0u8; 1024];
            let Ok(text) = read_file(p.as_str(), &mut b) else { continue };
            let text = core::str::from_utf8(text).unwrap_or("");
            let usec: u64 = text
                .lines()
                .find_map(|l| l.strip_prefix("usage_usec "))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let e = &mut sl.entry;
            e.cpu_rate = if e.cpu_have {
                (usec.saturating_sub(e.cpu_usec)) as f64 / 1000.0 / ((now - e.cpu_at).max(1)) as f64
            } else {
                0.0
            };
            e.cpu_at = now;
            e.cpu_usec = usec;
            e.cpu_have = true;
        }
    }

    fn write_listed(w: &mut W, e: &Entry) {
        let mem = paths::sbx_cgroup(e.spec.name.as_str(), "memory.current");
        let cur: u64 = crate::util::read_small::<32>(mem.as_str()).and_then(|s| s.as_str().parse().ok()).unwrap_or(0);
        w.obj()
            .kstr("name", e.spec.name.as_str())
            .kstr("ip", e.spec.ip.as_str())
            .kstr("state", if e.state == RUNNING { "running" } else { "receiving" })
            .kstr("createdAt", e.created_at.as_str())
            .kf64("cpu", e.spec.cpu)
            .ku64("memory", e.spec.memory)
            .kf64("cpuUsage", (e.cpu_rate * 1000.0).round() / 1000.0)
            .ku64("memoryCurrent", cur)
            .end();
    }

    /// The Listed[] array.
    pub fn write_list(&self, w: &mut W) {
        w.arr();
        let t = self.table();
        for sl in t.iter().filter(|s| s.used) {
            Self::write_listed(w, &sl.entry);
        }
        w.end_arr();
    }

    pub fn write_one(&self, w: &mut W, name: &str) -> bool {
        let t = self.table();
        match t.iter().find(|s| s.used && s.name.as_str() == name) {
            Some(sl) => {
                Self::write_listed(w, &sl.entry);
                true
            }
            None => false,
        }
    }

    fn check_ip(&self, spec: &Spec) -> Res<()> {
        let t = self.table();
        for sl in t.iter().filter(|s| s.used) {
            if sl.entry.spec.name != spec.name && sl.entry.spec.ip == spec.ip {
                return Err(err!(409, "ip {} is used by {}", spec.ip, sl.entry.spec.name));
            }
        }
        Ok(())
    }

    fn check_capacity(&self) -> Res<()> {
        if self.count() >= self.max {
            return Err(err!(503, "this host has limits.maxSandboxes ({}) sandboxes", self.max));
        }
        Ok(())
    }

    /// Image, netns, nft and bundle for `spec`; returns the bundle directory.
    fn prepare(&self, spec: &Spec, route: bool, sc: &mut Scratch) -> Res<paths::P> {
        let image_ref = if spec.image_ref.is_empty() { None } else { Some(spec.image_ref.as_str()) };
        images::ensure_image(self.cfg, &self.pulls, Some(spec.image.as_str()), image_ref)?;
        let mut a = [""; 40];
        quiet(runsc::argv(&mut a, &["delete", "-force", spec.name.as_str()])?);
        net::add_sandbox_net(self.cfg, spec.name.as_str(), spec.ip.as_str(), route)?;
        // the image's Env, copied out of the config (read into `big`) into `small`, NUL-separated
        let mut used = 0usize;
        let mut overflow = false;
        {
            let small = &mut *sc.small;
            images::image_env(spec.image.as_str(), sc.big, |kv| {
                if used + kv.len() + 1 > small.len() {
                    overflow = true;
                    return;
                }
                small[used..used + kv.len()].copy_from_slice(kv.as_bytes());
                small[used + kv.len()] = 0;
                used += kv.len() + 1;
            })?;
        }
        if overflow {
            return Err(Error::new(500, "image Env is too large"));
        }
        let env_text =
            core::str::from_utf8(&sc.small[..used]).map_err(|_| Error::new(500, "image Env is not UTF-8"))?;
        let mut env: [&str; 256] = [""; 256];
        let mut n = 0;
        for kv in env_text.split('\0').filter(|s| !s.is_empty()) {
            if n == env.len() {
                return Err(Error::new(500, "image Env has too many entries"));
            }
            env[n] = kv;
            n += 1;
        }
        let dir = path!("/var/lib/sbx/bundles/{}", spec.name);
        crate::util::mkdir(dir.as_str(), 0o700)?;
        let mut b = Buf::new(sc.big);
        let root = crate::storage::setup(spec)?;
        let scratch = crate::storage::scratch_dir(spec.name.as_str());
        runsc::bundle_spec(&mut b, spec, &env[..n], root.as_str(), scratch.as_str());
        if b.full {
            return Err(Error::new(500, "bundle too large"));
        }
        let len = b.len;
        crate::util::write_file_atomic(
            crate::paths::join(dir.as_str(), "/config.json").as_str(),
            &sc.big[..len],
            0o600,
        )?;
        Ok(dir)
    }

    pub fn create(&self, spec: &Spec, sc: &mut Scratch, w: &mut W) -> Res<()> {
        let name = spec.name.as_str();
        let lk = self.lock(name)?;
        if let Some(cur) = self.get(name) {
            if cur.spec.ip == spec.ip && cur.state == RUNNING {
                self.write_one(w, name);
                return Ok(());
            }
            return Err(err!(409, "{name} exists ({})", if cur.state == RUNNING { "running" } else { "receiving" }));
        }
        self.check_ip(spec)?;
        self.check_capacity()?;
        let t0 = mono_ms();
        let r = (|| -> Res<()> {
            let bundle = self.prepare(spec, true, sc)?;
            let log = log_file(name, false)?;
            use std::os::fd::AsRawFd;
            let mut a = [""; 40];
            // --detach: runsc returns once the sandbox runs; the sandbox keeps the log file, not a pipe of ours
            let argv = runsc::argv(&mut a, &["run", "--bundle", bundle.as_str(), "--detach", name])?;
            let c = spawn(argv, &[], Io::Null, Io::Fd(log.as_raw_fd()), Io::Fd(log.as_raw_fd()))?;
            drop(log);
            let mut none = [0u8; 0];
            let o = collect(&c, &mut none, 5 * 60_000);
            if o.code != 0 {
                return Err(err!(500, "runsc run exited {} (see /var/log/sbx/{name}.log)", o.code));
            }
            let e = Entry {
                spec: *spec,
                state: RUNNING,
                created_at: iso(now_ms()),
                port: 0,
                migrated: Str::new(),
                cpu_at: 0,
                cpu_usec: 0,
                cpu_rate: 0.0,
                cpu_have: false,
            };
            self.save(&lk, &e, sc.big)
        })();
        if let Err(e) = r {
            self.cleanup(&lk, name, Some(spec.ip.as_str()));
            return Err(e);
        }
        Log::info("sandbox created").s("sandbox", name).s("ip", spec.ip.as_str()).n("ms", mono_ms() - t0).emit();
        self.write_one(w, name);
        Ok(())
    }

    fn cleanup(&self, lk: &OpLock, name: &str, ip: Option<&str>) {
        let mut a = [""; 40];
        if let Ok(argv) = runsc::argv(&mut a, &["kill", name, "KILL"]) {
            let mut o = [0u8; 256];
            let _ = run(argv, &mut o, 10_000);
        }
        let mut a = [""; 40];
        if let Ok(argv) = runsc::argv(&mut a, &["delete", "-force", name]) {
            let mut o = [0u8; 256];
            let _ = run(argv, &mut o, 30_000);
        }
        net::remove_sandbox_net(name, ip);
        quiet(&["rm", "-rf", path!("/var/lib/sbx/bundles/{name}").as_str(), path!("/run/sbx/ckpt/in-{name}").as_str()]);
        // older sandboxes (--overlay2=root:self) kept their upper layer as a file in the image's rootfs
        let _ = each_dir_entry(paths::images().as_str(), |d| {
            crate::util::remove_file(path!("/var/lib/sbx/images/{d}/rootfs/.gvisor.filestore.{name}").as_str());
        });
        crate::storage::teardown(name);
        self.forget(lk, name);
    }

    pub fn delete(&self, name: &str) -> Res<()> {
        // a receive in progress: stop its restore first (as the Node code did)
        {
            let m = self.migt();
            if let Some(x) = m.iter().find(|x| x.in_use && x.active && x.name.as_str() == name) {
                proc::kill(x.pid);
            }
        }
        let lk = self.lock(name)?;
        let ip = self.get(name).map(|e| e.spec.ip);
        self.cleanup(&lk, name, ip.as_ref().map(|s| s.as_str()));
        Log::info("sandbox deleted").s("sandbox", name).emit();
        Ok(())
    }

    fn free_port(&self) -> Res<u16> {
        let t = self.table();
        (PORT_MIN..=PORT_MAX)
            .find(|p| !t.iter().any(|s| s.used && s.entry.port == *p))
            .ok_or_else(|| Error::new(503, "no free migration port"))
    }

    /// Target side of a migration: same network (without the /32 route: traffic still goes to the source until the
    /// restore is done) and bundle, then `runsc restore --migrate-listen` on the overlay address. Returns the port.
    pub fn receive(&self, spec: &Spec, sc: &mut Scratch) -> Res<(u16, &'static str)> {
        let name = spec.name.as_str();
        let lk = self.lock(name)?;
        if self.get(name).is_some() {
            return Err(err!(409, "{name} is already here"));
        }
        self.check_ip(spec)?;
        self.check_capacity()?;
        // a migration slot (a finished one older than 60 s, or any finished one, may be reused)
        let mi = {
            let mut m = self.migt();
            let now = mono_ms();
            let pick = m
                .iter()
                .position(|x| !x.in_use)
                .or_else(|| m.iter().position(|x| x.done && !x.assigned && now - x.finished_at > 60_000))
                .or_else(|| m.iter().position(|x| x.done && !x.assigned));
            let i = pick.ok_or_else(|| Error::new(503, "too many migrations in flight (limits.maxMigrations)"))?;
            m[i] = Mig {
                name: spec.name,
                pid: 0,
                active: false,
                assigned: false,
                done: false,
                in_use: true,
                error: Str::new(),
                finished_at: 0,
            };
            i
        };
        let r = (|| -> Res<(u16, i32)> {
            let port = self.free_port()?;
            let bundle = self.prepare(spec, false, sc)?;
            let dir = path!("/run/sbx/ckpt/in-{name}");
            quiet(&["rm", "-rf", dir.as_str()]);
            crate::util::mkdir(dir.as_str(), 0o700)?;
            let e = Entry {
                spec: *spec,
                state: RECEIVING,
                created_at: iso(now_ms()),
                port,
                migrated: Str::new(),
                cpu_at: 0,
                cpu_usec: 0,
                cpu_rate: 0.0,
                cpu_have: false,
            };
            self.save(&lk, &e, sc.big)?;
            let out = log_file(name, true)?;
            let err = log_file(name, false)?;
            let mut listen: Str<32> = Str::new();
            let _ = write!(listen, "{}:{port}", self.cfg.wg_address);
            let mut a = [""; 40];
            let argv = runsc::argv(
                &mut a,
                &[
                    "restore",
                    "--migrate-listen",
                    listen.as_str(),
                    "--image-path",
                    dir.as_str(),
                    "--background",
                    "--bundle",
                    bundle.as_str(),
                    "--detach",
                    name,
                ],
            )?;
            use std::os::fd::AsRawFd;
            let c = spawn(argv, &[], Io::Null, Io::Fd(out.as_raw_fd()), Io::Fd(err.as_raw_fd()))?;
            Ok((port, c.pid))
        })();
        let (port, pid) = match r {
            Ok(x) => x,
            Err(e) => {
                self.migt()[mi].in_use = false;
                self.cleanup(&lk, name, Some(spec.ip.as_str()));
                return Err(e);
            }
        };
        {
            let mut m = self.migt();
            m[mi].pid = pid;
            m[mi].active = true;
            m[mi].assigned = true;
        }
        self.mcv.notify_all();
        // listening?
        let until = mono_ms() + 30_000;
        while mono_ms() < until {
            if listening(port) {
                return Ok((port, self.cfg.wg_address.as_str()));
            }
            if !self.migt()[mi].active {
                break;
            }
            crate::util::sleep_ms(50);
        }
        proc::kill(pid);
        drop(lk);
        Err(Error::new(500, "the restore did not start listening"))
    }

    /// Slot thread `i`: waits for its restore child, then finishes the receive (route, stats, state) or cleans up.
    pub fn migration_thread(&self, i: usize, sc: &mut Scratch) -> ! {
        loop {
            let (pid, name) = {
                let mut m = self.migt();
                while !m[i].assigned {
                    m = self.mcv.wait(m).unwrap_or_else(|p| p.into_inner());
                }
                (m[i].pid, m[i].name)
            };
            let code = proc::wait(pid, true).unwrap_or(-1);
            {
                self.migt()[i].active = false;
            }
            let name = name.as_str();
            let mut error: Str<200> = Str::new();
            match self.lock(name) {
                Ok(lk) => {
                    let e = self.get(name);
                    if code == 0 {
                        if let Some(mut e) = e {
                            let r = net::route_sandbox(name, e.spec.ip.as_str());
                            e.migrated = restore_stats(name, sc.big);
                            e.state = RUNNING;
                            e.port = 0;
                            quiet(&["rm", "-rf", path!("/run/sbx/ckpt/in-{name}").as_str()]);
                            match r.and_then(|_| self.save(&lk, &e, sc.big)) {
                                Ok(()) => Log::info("sandbox received")
                                    .s("sandbox", name)
                                    .j("stats", if e.migrated.is_empty() { b"null" } else { e.migrated.as_bytes() })
                                    .emit(),
                                Err(x) => {
                                    error = Str::lossy(x.msg.as_str());
                                    Log::error("sandbox receive: finishing failed")
                                        .s("sandbox", name)
                                        .s("error", x.msg.as_str())
                                        .emit();
                                }
                            }
                        } else {
                            error = Str::lossy("deleted while receiving");
                        }
                    } else {
                        let _ = write!(error, "restore exited {code}");
                        Log::error("sandbox receive failed").s("error", error.as_str()).s("sandbox", name).emit();
                        let ip = e.map(|e| e.spec.ip);
                        self.cleanup(&lk, name, ip.as_ref().map(|s| s.as_str()));
                    }
                }
                Err(x) => error = Str::lossy(x.msg.as_str()),
            }
            {
                let mut m = self.migt();
                m[i].error = error;
                m[i].done = true;
                m[i].assigned = false;
                m[i].finished_at = mono_ms();
            }
            self.mcv.notify_all();
        }
    }

    /// Target: waits (up to wait_ms) for a receive to finish; running = restored and routed here.
    pub fn receive_result(&self, name: &str, wait_ms: u64, w: &mut W) {
        let deadline = mono_ms() + wait_ms;
        let mut err: Option<Str<200>> = None;
        {
            let mut m = self.migt();
            while let Some(i) = m.iter().position(|x| x.in_use && x.name.as_str() == name) {
                if m[i].done {
                    if !m[i].error.is_empty() {
                        err = Some(m[i].error);
                    }
                    break;
                }
                let now = mono_ms();
                if now >= deadline {
                    break;
                }
                m = self
                    .mcv
                    .wait_timeout(m, std::time::Duration::from_millis((deadline - now).min(1000)))
                    .map(|r| r.0)
                    .unwrap_or_else(|p| p.into_inner().0);
            }
        }
        match self.get(name) {
            Some(e) if e.state == RUNNING => {
                w.obj().kstr("state", "running");
                if !e.migrated.is_empty() {
                    w.key("stats").raw(e.migrated.as_bytes());
                }
                w.end();
            }
            Some(_) => {
                w.obj().kstr("state", "receiving").end();
            }
            None => {
                w.obj()
                    .kstr("state", "failed")
                    .kstr("error", err.as_ref().map(|s| s.as_str()).unwrap_or("not here"))
                    .end();
            }
        }
    }

    /// Source side: copies the sandbox to `to` (host:port of a receive) while it runs, then stops it for the last
    /// dirty pages and its kernel state. 409 = not moved, still running here; 502 = lost after the switch.
    pub fn send(
        &self,
        name: &str,
        to: &str,
        downtime_ms: u64,
        rounds: u64,
        max_downtime_ms: u64,
        sc: &mut Scratch,
    ) -> Res<u64> {
        {
            let mut n = self.sends.lock().unwrap_or_else(|p| p.into_inner());
            if *n >= self.cfg.limits.max_migrations {
                return Err(Error::new(503, "too many migrations in flight (limits.maxMigrations)"));
            }
            *n += 1;
        }
        let r = self.send_locked(name, to, downtime_ms, rounds, max_downtime_ms, sc);
        *self.sends.lock().unwrap_or_else(|p| p.into_inner()) -= 1;
        r
    }

    fn send_locked(
        &self,
        name: &str,
        to: &str,
        downtime_ms: u64,
        rounds: u64,
        max_downtime_ms: u64,
        sc: &mut Scratch,
    ) -> Res<u64> {
        let lk = self.lock(name)?;
        let e = match self.get(name) {
            Some(e) if e.state == RUNNING => e,
            _ => return Err(err!(404, "{name} is not running here")),
        };
        let t0 = mono_ms();
        let (mut d, mut r, mut md): (Str<24>, Str<24>, Str<24>) = (Str::new(), Str::new(), Str::new());
        let _ = write!(d, "{downtime_ms}ms");
        let _ = write!(r, "{rounds}");
        let _ = write!(md, "{max_downtime_ms}ms");
        let mut args: [&str; 10] = [
            "checkpoint",
            "--migrate-to",
            to,
            "--migrate-downtime",
            d.as_str(),
            "--migrate-max-rounds",
            r.as_str(),
            "",
            "",
            "",
        ];
        let mut n = 7;
        // a migration that won't converge is aborted before the sandbox stops (the fork's --migrate-max-downtime)
        if max_downtime_ms > 0 {
            args[7] = "--migrate-max-downtime";
            args[8] = md.as_str();
            n = 9;
        }
        args[n] = name;
        let mut a = [""; 40];
        let argv = runsc::argv(&mut a, &args[..n + 1])?;
        let o = run(argv, sc.small, 15 * 60_000)?;
        if o.code != 0 {
            let stderr = o.err;
            let st = self.status(name, sc.big);
            let tail = tail_chars(stderr.as_str().trim(), 300);
            Log::error("sandbox send failed").s("error", tail).s("sandbox", name).s("status", st.as_str()).emit();
            if st.as_str() == "running" {
                return Err(err!(409, "migration failed before the switch; the sandbox still runs here: {tail}"));
            }
            self.cleanup(&lk, name, Some(e.spec.ip.as_str()));
            return Err(err!(502, "migration failed after the switch; the sandbox is lost: {tail}"));
        }
        self.cleanup(&lk, name, Some(e.spec.ip.as_str()));
        let ms = mono_ms() - t0;
        Log::info("sandbox sent").s("sandbox", name).s("to", to).n("ms", ms).emit();
        Ok(ms)
    }

    /// Removes runsc containers that have no state (left by a crash between run and save).
    pub fn sweep(&self, sc: &mut Scratch) {
        let mut a = [""; 40];
        let Ok(argv) = runsc::argv(&mut a, &["list", "-format", "json"]) else { return };
        let o = match run(argv, sc.big, 30_000) {
            Ok(o) if o.code == 0 && !o.truncated => o,
            Ok(o) if o.truncated => {
                Log::error("sweep: runsc list output too large").emit();
                return;
            }
            _ => return,
        };
        let mut ids: [Str<48>; 128] = [Str::new(); 128];
        let mut n = 0;
        if let Ok(Val::Arr(arr)) = json::parse(o.out) {
            let _ = json::array::<Error>(arr, |c| {
                if let Val::Obj(co) = c {
                    json::object::<Error>(co, |k, v| {
                        if k.is("id") {
                            let mut b = [0u8; 64];
                            if let Some(id) = v.str().and_then(|s| s.decode(&mut b))
                                && crate::config::valid_name(id)
                                && n < ids.len()
                            {
                                ids[n] = Str::lossy(id);
                                n += 1;
                            }
                        }
                        Ok(())
                    })?;
                }
                Ok(())
            });
        }
        for id in &ids[..n] {
            let id = id.as_str();
            if self.get(id).is_some() || self.receiving(id) {
                continue;
            }
            let Ok(lk) = self.lock(id) else { continue };
            if self.get(id).is_some() || self.receiving(id) {
                continue;
            }
            Log::info("sweeping a container without state").s("sandbox", id).emit();
            self.cleanup(&lk, id, None);
        }
    }

    fn receiving(&self, name: &str) -> bool {
        self.migt().iter().any(|x| x.in_use && !x.done && x.name.as_str() == name)
    }
}

fn tail_chars(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut i = s.len() - n;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

fn log_file(name: &str, restore_out: bool) -> Res<std::fs::File> {
    let p = if restore_out { path!("/var/log/sbx/{name}.restore.out") } else { path!("/var/log/sbx/{name}.log") };
    let mut o = std::fs::OpenOptions::new();
    if restore_out {
        o.write(true).create(true).truncate(true);
    } else {
        o.append(true).create(true);
    }
    o.open(p.as_str()).map_err(|e| crate::util::at(p.as_str(), e))
}

/// The stream statistics line ({"from"...) the restore printed.
fn restore_stats(name: &str, buf: &mut [u8]) -> Str<1024> {
    let p = path!("/var/log/sbx/{name}.restore.out");
    let Ok(text) = read_file(p.as_str(), buf) else { return Str::lossy("null") };
    let text = core::str::from_utf8(text).unwrap_or("");
    match text.lines().find(|l| l.starts_with("{\"from\"")) {
        Some(l) if json::parse(l.as_bytes()).is_ok() => Str::from(l).unwrap_or_else(|| Str::lossy("null")),
        _ => Str::lossy("null"),
    }
}

/// A TCP listener on `port` in this network namespace (/proc/net/tcp, state 0A).
pub fn listening(port: u16) -> bool {
    let mut hex: Str<8> = Str::new();
    let _ = write!(hex, ":{port:04X} ");
    for f in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(file) = std::fs::File::open(f) else { continue };
        use std::io::Read;
        let mut file = file;
        let mut buf = [0u8; 4096];
        let mut carry = 0usize;
        while let Ok(k) = file.read(&mut buf[carry..]) {
            if k == 0 {
                break;
            }
            let end = carry + k;
            let mut start = 0;
            while let Some(nl) = buf[start..end].iter().position(|c| *c == b'\n') {
                let line = core::str::from_utf8(&buf[start..start + nl]).unwrap_or("");
                // "  sl  local_address rem_address   st ..."
                let mut cols = line.split_whitespace();
                let local = cols.nth(1).unwrap_or("");
                let st = cols.nth(1).unwrap_or("");
                if st == "0A" && local.ends_with(&hex.as_str()[..5]) {
                    return true;
                }
                start += nl + 1;
            }
            buf.copy_within(start..end, 0);
            carry = end - start;
            if carry == buf.len() {
                carry = 0;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip_with_node_files() {
        let node = br#"{"spec":{"name":"sbx-memtestb1","ip":"10.200.250.21","image":"a18f79b442fee2d4bae0fc5da3b2d892f015af58446c3c4363863991b8d76eb5","imageRef":"ghcr.io/de0ch/jarvis3-sandbox:2712e71c5178","env":{"HOME":"/home/sandbox","HTTP_PROXY":"http://x"},"cpu":1,"memory":536870912,"cpuFeatures":"fpu"},"state":"receiving","createdAt":"2026-10-21T02:13:58.974Z","port":7100,"migrated":{"from":"10.201.0.10:58490","bytes":30306113,"rounds":1}}"#;
        let e = Sandboxes::parse_state(node, "10.200.0.0/16").ok().unwrap();
        assert_eq!(e.state, RECEIVING);
        assert_eq!(e.port, 7100);
        assert_eq!(e.created_at.as_str(), "2026-10-21T02:13:58.974Z");
        assert!(e.migrated.as_str().starts_with("{\"from\""));
        assert_eq!(status_of(r#"{"ociVersion":"1","id":"x","status":"running","pid":3}"#).as_str(), "running");
        assert_eq!(tail_chars("abcdef", 3), "def");
    }

    #[test]
    fn finds_listeners() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(listening(port));
        drop(l);
        assert!(!listening(1));
    }
}
