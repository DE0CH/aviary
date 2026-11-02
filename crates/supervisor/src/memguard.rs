//! Memory and OOM protection (supervisor 5.2, 5.3).
//!
//! - [`agent_cgroup`]: a cgroup for Claude Code with `memory.max = L - 128 MiB` where the runtime offers a writable
//!   one (cgroup v2, else v1's memory hierarchy). Where the runtime enforces it, the kernel kills inside the agent's
//!   cgroup before it touches the supervisor; where it does not, the writes succeed and change nothing. Failures are
//!   logged once and not fatal.
//! - [`Guard`]: the user-space guard, the protection that works in every runtime. `U = MemTotal - MemAvailable`,
//!   sampled every 500 ms (100 ms above 80 % of `L`); at `U >= L - 128 MiB` it SIGKILLs the largest process other
//!   than PID 1, the supervisor and Claude Code's main process (if at least 32 MiB), else Claude Code's main process,
//!   then waits 2 s before the next decision. Each kill is logged and handed to the caller (an activity event; a
//!   Claude Code kill ends the running turn with `run.oom`). Inside the sandbox this protects against accidents, not
//!   against root (supervisor 6.3).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::limits::{
    AGENT_CGROUP_RESERVE, KILL_COOLDOWN, MEM_FAST_PERCENT, MEM_RESERVE, MEM_SAMPLE, MEM_SAMPLE_FAST, MIN_VICTIM_RSS,
};

/// A view of `/proc` (a fake tree in tests).
#[derive(Clone, Debug)]
pub struct Proc {
    root: PathBuf,
}

/// One process as the guard sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: i32,
    pub comm: String,
    pub rss: u64,
}

/// `/proc/meminfo`'s two numbers, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
}

impl MemInfo {
    /// `U`.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
}

/// The page size `statm` counts in.
fn page_size() -> u64 {
    // SAFETY: sysconf has no memory effects.
    #[allow(unsafe_code)]
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(p).ok().filter(|p| *p > 0).unwrap_or(4096)
}

impl Proc {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The system's `/proc`.
    #[must_use]
    pub fn system() -> Self {
        Self::new("/proc")
    }

    /// `MemTotal` and `MemAvailable`.
    #[must_use]
    pub fn meminfo(&self) -> Option<MemInfo> {
        let text = std::fs::read_to_string(self.root.join("meminfo")).ok()?;
        let field = |name: &str| -> Option<u64> {
            let line = text.lines().find(|l| l.starts_with(name))?;
            let kb: u64 = line[name.len()..].trim().trim_end_matches("kB").trim().parse().ok()?;
            Some(kb * 1024)
        };
        Some(MemInfo {
            total: field("MemTotal:")?,
            available: field("MemAvailable:")?,
        })
    }

    /// A process's resident bytes from `statm`.
    #[must_use]
    pub fn rss(&self, pid: i32) -> Option<u64> {
        let statm = std::fs::read_to_string(self.root.join(pid.to_string()).join("statm")).ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * page_size())
    }

    /// Every process with its RSS.
    #[must_use]
    pub fn processes(&self) -> Vec<ProcInfo> {
        let Ok(dir) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        dir.filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(|pid| {
                let rss = self.rss(pid)?;
                let comm = std::fs::read_to_string(self.root.join(pid.to_string()).join("comm"))
                    .map(|s| s.trim_end().to_owned())
                    .unwrap_or_default();
                Some(ProcInfo { pid, comm, rss })
            })
            .collect()
    }
}

/// The guard's choice (5.3): the largest process other than PID 1, the supervisor and Claude Code's main process,
/// if it is at least 32 MiB; otherwise Claude Code's main process (if known and alive). Never PID 1 or the
/// supervisor.
#[must_use]
pub fn choose_victim(procs: &[ProcInfo], supervisor: i32, claude: Option<i32>) -> Option<ProcInfo> {
    let protected = |p: &ProcInfo| p.pid == 1 || p.pid == supervisor || Some(p.pid) == claude;
    if let Some(big) = procs
        .iter()
        .filter(|p| !protected(p))
        .max_by_key(|p| p.rss)
        .filter(|p| p.rss >= MIN_VICTIM_RSS)
    {
        return Some(big.clone());
    }
    let c = claude?;
    procs.iter().find(|p| p.pid == c).cloned()
}

/// `L`: hermit's memory limit, or `MemTotal` when that is smaller.
#[must_use]
pub fn limit(descriptor_bytes: u64, mem: Option<MemInfo>) -> u64 {
    match mem {
        Some(m) if m.total > 0 && m.total < descriptor_bytes => m.total,
        _ => descriptor_bytes,
    }
}

/// A kill the guard made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kill {
    pub victim: ProcInfo,
    /// It was Claude Code's main process (the generation ends; the turn fails `run.oom`).
    pub claude: bool,
    pub used: u64,
    pub limit: u64,
}

/// What the guard needs from the rest of the supervisor.
pub trait GuardHost: Send + Sync + 'static {
    /// Claude Code's main pid, if a generation runs.
    fn claude_pid(&self) -> Option<i32>;
    /// Sends SIGKILL.
    fn kill(&self, pid: i32);
    /// After the kill: report it (activity event; for Claude Code, end the turn with `run.oom`).
    fn killed(&self, kill: &Kill);
}

/// The user-space guard.
#[derive(Debug)]
pub struct Guard {
    proc: Proc,
    descriptor_limit: u64,
    supervisor: i32,
}

impl Guard {
    #[must_use]
    pub fn new(proc: Proc, descriptor_limit: u64, supervisor: i32) -> Self {
        Self {
            proc,
            descriptor_limit,
            supervisor,
        }
    }

    /// One decision: `Some(kill)` when usage is at the watermark and a victim was killed, plus the delay before the
    /// next sample.
    pub fn tick(&self, host: &dyn GuardHost) -> (Option<Kill>, Duration) {
        let mem = self.proc.meminfo();
        let l = limit(self.descriptor_limit, mem);
        let Some(mem) = mem else {
            return (None, MEM_SAMPLE);
        };
        let u = mem.used();
        let high = l.saturating_sub(MEM_RESERVE);
        if u >= high {
            let claude = host.claude_pid();
            if let Some(victim) = choose_victim(&self.proc.processes(), self.supervisor, claude) {
                host.kill(victim.pid);
                let kill = Kill {
                    claude: Some(victim.pid) == claude,
                    victim,
                    used: u,
                    limit: l,
                };
                tracing::warn!(
                    pid = kill.victim.pid,
                    comm = %kill.victim.comm,
                    rss = kill.victim.rss,
                    used = u,
                    limit = l,
                    "memory guard killed"
                );
                host.killed(&kill);
                return (Some(kill), KILL_COOLDOWN);
            }
            tracing::warn!(used = u, limit = l, "memory guard found no victim");
        }
        let fast = u.saturating_mul(100) > l.saturating_mul(MEM_FAST_PERCENT);
        (None, if fast { MEM_SAMPLE_FAST } else { MEM_SAMPLE })
    }

    /// The guard's task. Each tick reads `/proc` on the blocking pool: under memory pressure `/proc` is slow, and the
    /// runtime's two workers must stay free for the activity loop and `:7070`.
    pub async fn run(self, host: Arc<dyn GuardHost>) {
        let me = Arc::new(self);
        loop {
            let (g, h) = (me.clone(), host.clone());
            let wait = tokio::task::spawn_blocking(move || g.tick(h.as_ref()).1)
                .await
                .unwrap_or(MEM_SAMPLE);
            tokio::time::sleep(wait).await;
        }
    }
}

/// Creates the agent's cgroup with `memory.max = limit - 128 MiB` (5.2) under `sys_cgroup` (`/sys/fs/cgroup`), the
/// path to spawn Claude Code into, or `None` (logged once) when the runtime has no writable cgroup.
#[must_use]
pub fn agent_cgroup(sys_cgroup: &Path, limit: u64) -> Option<PathBuf> {
    let max = limit.saturating_sub(AGENT_CGROUP_RESERVE).to_string();
    let attempt = || -> std::io::Result<PathBuf> {
        if sys_cgroup.join("cgroup.controllers").exists() {
            let dir = sys_cgroup.join("agent");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("memory.max"), &max)?;
            std::fs::write(dir.join("memory.oom.group"), "0")?;
            Ok(dir)
        } else if sys_cgroup.join("memory").is_dir() {
            let dir = sys_cgroup.join("memory").join("agent");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("memory.limit_in_bytes"), &max)?;
            Ok(dir)
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no cgroup hierarchy"))
        }
    };
    match attempt() {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::info!(error = %e, "agent cgroup not created");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const MIB: u64 = 1024 * 1024;

    struct FakeProc {
        root: PathBuf,
    }

    impl FakeProc {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("fakeproc-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }
        fn meminfo(&self, total: u64, available: u64) {
            std::fs::write(
                self.root.join("meminfo"),
                format!(
                    "MemTotal:       {} kB\nMemFree:        1 kB\nMemAvailable:   {} kB\n",
                    total / 1024,
                    available / 1024
                ),
            )
            .unwrap();
        }
        fn process(&self, pid: i32, comm: &str, rss: u64) {
            let d = self.root.join(pid.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("statm"), format!("100000 {} 10 1 0 5 0\n", rss / page_size())).unwrap();
            std::fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
        }
    }

    impl Drop for FakeProc {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Default)]
    struct Host {
        claude: Option<i32>,
        killed: Mutex<Vec<i32>>,
        reported: Mutex<Vec<Kill>>,
    }

    impl GuardHost for Host {
        fn claude_pid(&self) -> Option<i32> {
            self.claude
        }
        fn kill(&self, pid: i32) {
            self.killed.lock().unwrap().push(pid);
        }
        fn killed(&self, kill: &Kill) {
            self.reported.lock().unwrap().push(kill.clone());
        }
    }

    fn sandbox(p: &FakeProc) {
        p.process(1, "supervisor-keep", 2 * MIB);
        p.process(40, "supervisor", 30 * MIB);
        p.process(812, "claude", 500 * MIB);
        p.process(900, "bash", 5 * MIB);
        p.process(912, "python3", 384 * MIB);
        p.process(913, "node", 64 * MIB);
    }

    #[test]
    fn the_largest_tool_dies_first() {
        let p = FakeProc::new();
        sandbox(&p);
        p.meminfo(1024 * MIB, 100 * MIB); // U = 924 MiB >= 1024 - 128
        let host = Host {
            claude: Some(812),
            ..Host::default()
        };
        let g = Guard::new(Proc::new(&p.root), 1024 * MIB, 40);
        let (kill, wait) = g.tick(&host);
        let kill = kill.unwrap();
        assert_eq!(
            (kill.victim.pid, kill.victim.comm.as_str(), kill.claude),
            (912, "python3", false)
        );
        assert_eq!(kill.victim.rss, 384 * MIB);
        assert_eq!(wait, KILL_COOLDOWN);
        assert_eq!(*host.killed.lock().unwrap(), vec![912]);
        assert_eq!(host.reported.lock().unwrap().len(), 1);
    }

    #[test]
    fn claude_code_when_no_tool_is_big_enough() {
        let p = FakeProc::new();
        p.process(1, "supervisor-keep", 2 * MIB);
        p.process(40, "supervisor", 30 * MIB);
        p.process(812, "claude", 800 * MIB);
        p.process(900, "bash", 31 * MIB);
        p.meminfo(1024 * MIB, 50 * MIB);
        let host = Host {
            claude: Some(812),
            ..Host::default()
        };
        let (kill, _) = Guard::new(Proc::new(&p.root), 1024 * MIB, 40).tick(&host);
        let kill = kill.unwrap();
        assert_eq!((kill.victim.pid, kill.claude), (812, true));
    }

    #[test]
    fn never_the_keeper_or_the_supervisor() {
        let procs = vec![
            ProcInfo {
                pid: 1,
                comm: "k".into(),
                rss: 900 * MIB,
            },
            ProcInfo {
                pid: 40,
                comm: "s".into(),
                rss: 800 * MIB,
            },
        ];
        assert_eq!(choose_victim(&procs, 40, None), None);
        assert_eq!(choose_victim(&procs, 40, Some(99)), None, "a claude pid that is gone");
    }

    #[test]
    fn below_the_watermark_nothing_dies_and_the_pace_quickens_above_80_percent() {
        let p = FakeProc::new();
        sandbox(&p);
        let host = Host {
            claude: Some(812),
            ..Host::default()
        };
        let g = Guard::new(Proc::new(&p.root), 1024 * MIB, 40);
        p.meminfo(1024 * MIB, 600 * MIB); // 41 %
        assert_eq!(g.tick(&host), (None, MEM_SAMPLE));
        p.meminfo(1024 * MIB, 150 * MIB); // 85 %, below 896 MiB
        assert_eq!(g.tick(&host), (None, MEM_SAMPLE_FAST));
        assert!(host.killed.lock().unwrap().is_empty());
    }

    #[test]
    fn the_limit_is_the_smaller_of_hermits_and_memtotal() {
        let p = FakeProc::new();
        sandbox(&p);
        // hermit says 4 GiB but the sandbox has 1 GiB: L = 1 GiB, so 900 MiB used is over the watermark
        p.meminfo(1024 * MIB, 124 * MIB);
        let host = Host {
            claude: Some(812),
            ..Host::default()
        };
        let (kill, _) = Guard::new(Proc::new(&p.root), 4096 * MIB, 40).tick(&host);
        assert_eq!(kill.unwrap().limit, 1024 * MIB);
        assert_eq!(limit(512 * MIB, None), 512 * MIB);
    }

    #[test]
    fn the_real_proc_parses() {
        let p = Proc::system();
        let m = p.meminfo().unwrap();
        assert!(m.total > 0 && m.available <= m.total);
        let me = std::process::id().cast_signed();
        assert!(p.rss(me).unwrap() > 0);
        assert!(p.processes().iter().any(|x| x.pid == me));
    }

    #[test]
    fn the_agent_cgroup_in_both_hierarchies() {
        let base = std::env::temp_dir().join(format!("cg-{}", uuid::Uuid::new_v4().simple()));
        let v2 = base.join("v2");
        std::fs::create_dir_all(&v2).unwrap();
        std::fs::write(v2.join("cgroup.controllers"), "memory pids\n").unwrap();
        let d = agent_cgroup(&v2, 1024 * MIB).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("memory.max")).unwrap(),
            (896 * MIB).to_string()
        );
        assert_eq!(std::fs::read_to_string(d.join("memory.oom.group")).unwrap(), "0");
        let v1 = base.join("v1");
        std::fs::create_dir_all(v1.join("memory")).unwrap();
        let d = agent_cgroup(&v1, 1024 * MIB).unwrap();
        assert!(d.ends_with("memory/agent"));
        assert_eq!(
            std::fs::read_to_string(d.join("memory.limit_in_bytes")).unwrap(),
            (896 * MIB).to_string()
        );
        assert_eq!(agent_cgroup(&base.join("none"), 1024 * MIB), None);
        let _ = std::fs::remove_dir_all(base);
    }
}
