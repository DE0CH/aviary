//! A keeper on a thread with its own directories, for tests anywhere in the crate.

#![allow(clippy::unwrap_used, clippy::expect_used, unsafe_code, dead_code)]

use std::os::fd::RawFd;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::server::{Config, Keeper, Signals};
use super::{Client, SpawnSpec, sys};

pub(crate) struct Running {
    pub dir: PathBuf,
    pub client: Client,
    stop_w: RawFd,
    thread: Option<JoinHandle<i32>>,
}

impl Running {
    pub fn start(supervisor: Option<Vec<String>>) -> Self {
        let dir = std::env::temp_dir().join(format!("keeper-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let (stop_r, stop_w) = sys::pipe().unwrap();
        let cfg = Config {
            run_dir: dir.join("run"),
            mount_run_tmpfs: false,
            spool_dir: dir.join("spool"),
            supervisor,
            protect_self: false,
            signals: Signals::Pipe(stop_r),
        };
        let mut k = Keeper::new(cfg).unwrap();
        let thread = std::thread::spawn(move || k.run());
        Self {
            client: Client::new(&dir.join("run")),
            dir,
            stop_w,
            thread: Some(thread),
        }
    }

    pub fn stop(&mut self) -> i32 {
        sys::write_all(self.stop_w, b"T").unwrap();
        self.thread.take().unwrap().join().unwrap()
    }

    pub fn file(&self, name: &str) -> String {
        self.dir.join(name).to_string_lossy().into_owned()
    }

    pub fn spec(&self, name: &str, argv: &[&str]) -> SpawnSpec {
        SpawnSpec {
            name: name.into(),
            argv: argv.iter().map(|s| (*s).to_owned()).collect(),
            env: vec!["PATH=/usr/bin:/bin".into(), "ONLY=this".into()],
            cwd: Some(self.dir.to_string_lossy().into_owned()),
            // SAFETY: getuid/getgid have no memory effects.
            uid: unsafe { libc::getuid() },
            // SAFETY: as above.
            gid: unsafe { libc::getgid() },
            setsid: true,
            oom_score_adj: 100,
            cgroup: None,
            stdout: Some(self.file(&format!("{name}.out"))),
            stderr: Some(self.file(&format!("{name}.err"))),
            stdin_pipe: false,
        }
    }

    /// Waits for `pid`'s exit record.
    pub fn wait_exit(&self, pid: i32) -> super::ExitRecord {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(e) = self.client.exits(0).unwrap().into_iter().find(|e| e.pid == pid) {
                return e;
            }
            assert!(Instant::now() < deadline, "no exit for {pid}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
