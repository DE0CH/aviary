//! `POST /v1/exec` (supervisor 15.4): for hermit's operator tooling and acceptance checks, never a product path.
//! Runs as uid 1000 (never root) through the keeper; argv non-empty without NUL, cwd `/workspace` or below, stdin
//! at most 1 MiB, timeout at most 60 s, stdout and stderr at most 1 MiB each (`truncated` beyond), at most 2 at once
//! (a third is 429).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aviary_core::http::ApiError;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hermit_api::server::Answer;
use supervisor_api::{ExecRequest, ExecResult};
use tokio::sync::Semaphore;

use crate::boot::{AGENT_HOME, BootEnv, valid_name};
use crate::keeper::watch::Exits;
use crate::keeper::{self, SpawnSpec};
use crate::limits::{EXEC_CONCURRENCY, EXEC_GID, EXEC_IO_MAX, EXEC_TIMEOUT_MAX, EXEC_UID};

/// Where exec runs keep their output while they run (the scratch disk).
pub const EXEC_DIR: &str = "/var/lib/supervisor/exec";
/// The workspace every exec runs in or below.
pub const WORKSPACE: &str = "/workspace";

/// The exec runner.
#[derive(Debug)]
pub struct Exec {
    keeper: keeper::Client,
    exits: Arc<Exits>,
    boot: BootEnv,
    dir: PathBuf,
    workspace: String,
    uid: u32,
    gid: u32,
    slots: Semaphore,
}

impl Exec {
    /// The sandbox's exec runner (uid 1000, `/workspace`).
    #[must_use]
    pub fn new(keeper: keeper::Client, exits: Arc<Exits>, boot: BootEnv) -> Arc<Self> {
        Self::with(
            keeper,
            exits,
            boot,
            EXEC_DIR.into(),
            WORKSPACE.into(),
            (EXEC_UID, EXEC_GID),
        )
    }

    /// With other directories and ids (tests run without root).
    #[must_use]
    pub fn with(
        keeper: keeper::Client,
        exits: Arc<Exits>,
        boot: BootEnv,
        dir: PathBuf,
        workspace: String,
        (uid, gid): (u32, u32),
    ) -> Arc<Self> {
        Arc::new(Self {
            keeper,
            exits,
            boot,
            dir,
            workspace,
            uid,
            gid,
            slots: Semaphore::new(EXEC_CONCURRENCY),
        })
    }

    /// The cwd if it is the workspace or below it (no `.` or `..` components).
    fn cwd(&self, cwd: Option<&str>) -> Answer<String> {
        let cwd = cwd.unwrap_or(&self.workspace);
        let below = cwd == self.workspace
            || cwd
                .strip_prefix(&self.workspace)
                .is_some_and(|rest| rest.starts_with('/'));
        let clean = !cwd.split('/').any(|c| c == "." || c == "..") && !cwd.contains('\0');
        if below && clean {
            Ok(cwd.trim_end_matches('/').to_owned())
        } else {
            Err(ApiError::bad_request("cwd must be /workspace or below"))
        }
    }

    /// Runs one exec.
    ///
    /// # Errors
    /// 400 for a request outside the rules, 429 with two runs in progress, 500 when the keeper cannot spawn.
    pub async fn run(&self, req: ExecRequest, descriptor_env: &BTreeMap<String, String>) -> Answer<ExecResult> {
        if req.argv.is_empty() || req.argv.iter().any(|a| a.contains('\0')) {
            return Err(ApiError::bad_request("argv must be non-empty, without NUL"));
        }
        let cwd = self.cwd(req.cwd.as_deref())?;
        let stdin = match &req.stdin {
            Some(b) => {
                let bytes = STANDARD
                    .decode(b)
                    .map_err(|_| ApiError::bad_request("stdin is not base64"))?;
                if bytes.len() > EXEC_IO_MAX {
                    return Err(ApiError::too_large("stdin over 1 MiB"));
                }
                Some(bytes)
            }
            None => None,
        };
        let timeout = Duration::from_millis(u64::from(req.timeout_ms.unwrap_or(60_000)));
        if timeout > EXEC_TIMEOUT_MAX || timeout.is_zero() {
            return Err(ApiError::bad_request("timeoutMs must be 1..=60000"));
        }
        if req.env.iter().any(|(k, v)| !valid_name(k) || v.contains('\0')) {
            return Err(ApiError::bad_request("bad env entry"));
        }
        let Ok(_slot) = self.slots.try_acquire() else {
            return Err(ApiError::rate_limited(1));
        };
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = self.dir.join(&id);
        std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(format!("exec dir: {e}")))?;
        let result = self
            .spawn_and_wait(&req, &cwd, stdin, timeout, &dir, descriptor_env)
            .await;
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    async fn spawn_and_wait(
        &self,
        req: &ExecRequest,
        cwd: &str,
        stdin: Option<Vec<u8>>,
        timeout: Duration,
        dir: &std::path::Path,
        descriptor_env: &BTreeMap<String, String>,
    ) -> Answer<ExecResult> {
        let extra: Vec<(&str, String)> = req.env.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let out = dir.join("stdout");
        let err = dir.join("stderr");
        let spec = SpawnSpec {
            name: "exec".into(),
            argv: req.argv.clone(),
            env: self.boot.child_env(AGENT_HOME, descriptor_env, &extra),
            cwd: Some(cwd.to_owned()),
            uid: self.uid,
            gid: self.gid,
            setsid: true,
            oom_score_adj: 100,
            cgroup: None,
            stdout: Some(out.to_string_lossy().into_owned()),
            stderr: Some(err.to_string_lossy().into_owned()),
            stdin_pipe: stdin.is_some(),
        };
        let k = self.keeper.clone();
        let keeper::Spawned { pid, since } = tokio::task::spawn_blocking(move || k.spawn_tracked(&spec))
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
            .map_err(|e| ApiError::internal(format!("spawn: {e}")))?;
        if let Some(bytes) = stdin {
            let k = self.keeper.clone();
            // in the background: a process that never reads must not hold up its timeout
            tokio::task::spawn_blocking(move || {
                let _ = k.stdin(pid, keeper::new_msg_id(), &bytes);
                let _ = k.close_stdin(pid);
            });
        }
        let exit = if let Ok(e) = tokio::time::timeout(timeout, self.exits.wait(pid, since)).await {
            e
        } else {
            let k = self.keeper.clone();
            let _ = tokio::task::spawn_blocking(move || k.signal(-pid, libc::SIGKILL)).await;
            tokio::time::timeout(Duration::from_secs(10), self.exits.wait(pid, since))
                .await
                .map_err(|_| ApiError::internal("exec did not end after SIGKILL"))?
        };
        let (stdout, t1) = read_capped(&out);
        let (stderr, t2) = read_capped(&err);
        Ok(ExecResult {
            exit_code: exit.code,
            signal: exit.signal,
            stdout: STANDARD.encode(stdout),
            stderr: STANDARD.encode(stderr),
            truncated: t1 || t2,
        })
    }
}

/// At most [`EXEC_IO_MAX`] bytes of a file, and whether there was more.
fn read_capped(path: &std::path::Path) -> (Vec<u8>, bool) {
    use std::io::Read as _;
    let Ok(f) = std::fs::File::open(path) else {
        return (Vec::new(), false);
    };
    let mut buf = Vec::new();
    let _ = f.take(EXEC_IO_MAX as u64 + 1).read_to_end(&mut buf);
    let more = buf.len() > EXEC_IO_MAX;
    buf.truncate(EXEC_IO_MAX);
    (buf, more)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keeper::testing::Running;

    fn boot() -> BootEnv {
        BootEnv {
            proxy: "http://sbx:t@192.0.2.1:3128".into(),
            path: "/usr/bin:/bin".into(),
            lang: None,
            lc_all: None,
        }
    }

    fn exec(k: &Running) -> (Arc<Exec>, tokio::task::JoinHandle<()>) {
        let exits = Exits::new(k.client.clone(), 0);
        let h = tokio::spawn(exits.clone().run());
        let ws = k.dir.join("workspace");
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        // SAFETY: getuid/getgid have no memory effects.
        #[allow(unsafe_code)]
        let ids = unsafe { (libc::getuid(), libc::getgid()) };
        let e = Exec::with(
            k.client.clone(),
            exits,
            boot(),
            k.dir.join("exec"),
            ws.to_string_lossy().into_owned(),
            ids,
        );
        (e, h)
    }

    fn req(argv: &[&str]) -> ExecRequest {
        ExecRequest {
            argv: argv.iter().map(|s| (*s).to_owned()).collect(),
            cwd: None,
            stdin: None,
            timeout_ms: Some(10_000),
            env: BTreeMap::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runs_with_stdin_env_and_cwd() {
        let k = Running::start(None);
        let (e, _h) = exec(&k);
        let mut r = req(&[
            "/bin/sh",
            "-c",
            "cat; echo \"$EXTRA $HOME $HTTP_PROXY\"; pwd; echo bad >&2; exit 4",
        ]);
        r.stdin = Some(STANDARD.encode(b"in\n"));
        r.env.insert("EXTRA".into(), "x".into());
        r.cwd = Some(format!("{}/sub", k.dir.join("workspace").display()));
        let mut desc = BTreeMap::new();
        desc.insert("HTTP_PROXY".into(), "http://p".into());
        let out = e.run(r, &desc).await.unwrap();
        assert_eq!((out.exit_code, out.signal, out.truncated), (Some(4), None, false));
        let stdout = String::from_utf8(STANDARD.decode(out.stdout).unwrap()).unwrap();
        assert!(stdout.starts_with("in\nx /home/sandbox http://p\n"), "{stdout}");
        assert!(stdout.trim_end().ends_with("/workspace/sub"), "{stdout}");
        assert_eq!(STANDARD.decode(out.stderr).unwrap(), b"bad\n");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_rules_are_enforced() {
        let k = Running::start(None);
        let (e, _h) = exec(&k);
        let none = BTreeMap::new();
        let status = |r: Answer<ExecResult>| r.unwrap_err().status().as_u16();
        assert_eq!(status(e.run(req(&[]), &none).await), 400);
        assert_eq!(status(e.run(req(&["a\0b"]), &none).await), 400);
        let mut r = req(&["/bin/true"]);
        r.cwd = Some("/etc".into());
        assert_eq!(status(e.run(r, &none).await), 400);
        let mut r = req(&["/bin/true"]);
        r.cwd = Some(format!("{}/../x", k.dir.join("workspace").display()));
        assert_eq!(status(e.run(r, &none).await), 400);
        let mut r = req(&["/bin/true"]);
        r.cwd = Some(format!("{}x", k.dir.join("workspace").display()));
        assert_eq!(status(e.run(r, &none).await), 400, "a sibling that shares the prefix");
        let mut r = req(&["/bin/true"]);
        r.timeout_ms = Some(60_001);
        assert_eq!(status(e.run(r, &none).await), 400);
        let mut r = req(&["/bin/true"]);
        r.stdin = Some(STANDARD.encode(vec![0u8; EXEC_IO_MAX + 1]));
        assert_eq!(status(e.run(r, &none).await), 413);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn timeout_kills_the_group_and_output_is_capped() {
        let k = Running::start(None);
        let (e, _h) = exec(&k);
        let none = BTreeMap::new();
        let mut r = req(&["/bin/sh", "-c", "sleep 100 & sleep 100"]);
        r.timeout_ms = Some(300);
        let t = std::time::Instant::now();
        let out = e.run(r, &none).await.unwrap();
        assert_eq!(out.signal, Some(libc::SIGKILL));
        assert!(t.elapsed() < Duration::from_secs(5));
        let out = e
            .run(req(&["/bin/sh", "-c", "head -c 1100000 /dev/zero"]), &none)
            .await
            .unwrap();
        assert!(out.truncated);
        assert_eq!(STANDARD.decode(out.stdout).unwrap().len(), EXEC_IO_MAX);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn at_most_two_at_once() {
        let k = Running::start(None);
        let (e, _h) = exec(&k);
        let slow = || {
            let e = e.clone();
            tokio::spawn(async move { e.run(req(&["/bin/sleep", "1"]), &BTreeMap::new()).await })
        };
        let (a, b) = (slow(), slow());
        tokio::time::sleep(Duration::from_millis(200)).await;
        let third = e.run(req(&["/bin/true"]), &BTreeMap::new()).await;
        assert_eq!(third.unwrap_err().status().as_u16(), 429);
        assert!(a.await.unwrap().is_ok() && b.await.unwrap().is_ok());
        assert!(e.run(req(&["/bin/true"]), &BTreeMap::new()).await.is_ok());
    }
}
