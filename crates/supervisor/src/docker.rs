//! dockerd on demand (supervisor 4.6). The image's `docker` wrapper calls `GET /start` on
//! `/run/supervisor/docker.sock` (mode 0666, HTTP); the supervisor spawns dockerd through the keeper as root with a
//! fixed `daemon.json`, its output to `/var/log/dockerd.log`, then waits up to 60 s for `/var/run/docker.sock` to
//! answer `_ping`: `200 ok\n` or `500 <error>\n` (at once when dockerd exits during the wait, with its exit). Once a
//! release has begun it answers `409 the sandbox is being released\n` and spawns nothing (the release ends a
//! running dockerd, [`crate::start::leftovers`]). dockerd is never started unless asked; whether Docker works at all
//! is the runtime's business.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use tokio::sync::Mutex;

use crate::keeper::watch::Exits;
use crate::keeper::{self, SpawnSpec};
use crate::lifecycle::Lifecycle;
use crate::limits::DOCKERD_START_WAIT;
#[cfg(test)]
use crate::local::serve_unix;

/// `daemon.json` (4.6).
pub const DAEMON_JSON: &str =
    "{\"iptables\": false, \"ip6tables\": false, \"bridge\": \"none\", \"group\": \"node\", \"log-level\": \"warn\"}\n";

/// Where dockerd's pieces are.
#[derive(Clone, Debug)]
pub struct DockerPaths {
    pub dockerd: Vec<String>,
    pub daemon_json: PathBuf,
    pub log: PathBuf,
    /// The Docker API socket dockerd creates.
    pub api_socket: PathBuf,
}

impl DockerPaths {
    /// The sandbox image's paths.
    #[must_use]
    pub fn sandbox() -> Self {
        Self {
            dockerd: vec!["/usr/bin/dockerd".into()],
            daemon_json: "/etc/docker/daemon.json".into(),
            log: "/var/log/dockerd.log".into(),
            api_socket: "/var/run/docker.sock".into(),
        }
    }
}

/// Why `GET /start` did not answer `200`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// A release has begun: `409`.
    Releasing,
    /// `500` with the reason.
    Failed(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Releasing => f.write_str("the sandbox is being released"),
            Self::Failed(e) => f.write_str(e),
        }
    }
}

impl From<String> for StartError {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

/// Starts dockerd once, on the first ask.
#[derive(Debug)]
pub struct Docker {
    keeper: keeper::Client,
    exits: Arc<Exits>,
    paths: DockerPaths,
    /// The environment dockerd gets (supervisor 6.2: the image's basics plus the descriptor's `env`).
    env: Vec<String>,
    /// dockerd's pid and the exit counter to wait from, once known.
    pid: Mutex<Option<(i32, u64)>>,
    /// The spawn goes through its door ([`Lifecycle::admit_spawn`]) once set.
    lifecycle: OnceLock<Weak<Lifecycle>>,
}

impl Docker {
    #[must_use]
    pub fn new(keeper: keeper::Client, exits: Arc<Exits>, paths: DockerPaths, env: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            keeper,
            exits,
            paths,
            env,
            pid: Mutex::new(None),
            lifecycle: OnceLock::new(),
        })
    }

    /// Ties the start to the lifecycle: nothing is spawned once a release has begun.
    pub fn gate(&self, lifecycle: &Arc<Lifecycle>) {
        let _ = self.lifecycle.set(Arc::downgrade(lifecycle));
    }

    fn releasing(&self) -> bool {
        self.lifecycle.get().and_then(Weak::upgrade).is_some_and(|l| {
            matches!(
                l.phase(),
                crate::lifecycle::Phase::Releasing | crate::lifecycle::Phase::Released
            )
        })
    }

    /// Starts dockerd unless it answers already, and waits for `_ping`.
    ///
    /// # Errors
    /// [`StartError::Releasing`] once a release has begun; otherwise the reason, as the `500` body says it.
    pub async fn start(&self) -> Result<(), StartError> {
        if self.releasing() {
            return Err(StartError::Releasing);
        }
        let mut pid = self.pid.lock().await;
        // one that answers (a predecessor's, after a supervisor restart) is used as it is
        if ping(&self.paths.api_socket).await {
            return Ok(());
        }
        // one the keeper runs (this supervisor's or a predecessor's) is still starting: wait for it
        let k = self.keeper.clone();
        let live = tokio::task::spawn_blocking(move || k.status())
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|s| {
                s.children
                    .into_iter()
                    .find(|(c, n)| n == "dockerd" && pid.is_none_or(|(p, _)| p == *c))
                    .map(|(c, _)| c)
            });
        if let Some(p) = live {
            // this supervisor's own keeps its counter; a predecessor's was running when the watcher started
            let since = pid.filter(|(q, _)| *q == p).map_or(self.exits.origin(), |(_, s)| s);
            *pid = Some((p, since));
        } else {
            if let Some(dir) = self.paths.daemon_json.parent() {
                tokio::fs::create_dir_all(dir)
                    .await
                    .map_err(|e| format!("daemon.json: {e}"))?;
            }
            tokio::fs::write(&self.paths.daemon_json, DAEMON_JSON)
                .await
                .map_err(|e| format!("daemon.json: {e}"))?;
            let log = self.paths.log.to_string_lossy().into_owned();
            let spec = SpawnSpec {
                name: "dockerd".into(),
                argv: self.paths.dockerd.clone(),
                env: self.env.clone(),
                cwd: Some("/".into()),
                uid: 0,
                gid: 0,
                setsid: true,
                oom_score_adj: 0,
                cgroup: None,
                stdout: Some(log.clone()),
                stderr: Some(log),
                stdin_pipe: false,
            };
            let lifecycle = self.lifecycle.get().and_then(Weak::upgrade);
            let admitted = match &lifecycle {
                Some(l) => Some(l.admit_spawn().await.ok_or(StartError::Releasing)?),
                None => None,
            };
            let k = self.keeper.clone();
            let p = tokio::task::spawn_blocking(move || k.spawn_tracked(&spec))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("spawn dockerd: {e}"))?;
            drop(admitted);
            tracing::info!(pid = p.pid, "dockerd started");
            *pid = Some((p.pid, p.since));
        }
        let Some((p, since)) = *pid else {
            return Err(StartError::Failed("no dockerd".into()));
        };
        let answered = async {
            loop {
                if ping(&self.paths.api_socket).await {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        };
        tokio::select! {
            () = answered => Ok(()),
            e = self.exits.wait(p, since) => {
                *pid = None;
                Err(StartError::Failed(format!(
                    "dockerd exited before it answered (code {}, signal {}); see {}",
                    e.code.map_or_else(|| "none".into(), |c| c.to_string()),
                    e.signal.map_or_else(|| "none".into(), |s| s.to_string()),
                    self.paths.log.display()
                )))
            }
            () = tokio::time::sleep(DOCKERD_START_WAIT) => Err(StartError::Failed("dockerd did not answer within 60 s".into())),
        }
    }

    /// The `/run/supervisor/docker.sock` router.
    pub fn router(self: &Arc<Self>) -> Router {
        let me = self.clone();
        Router::new().route(
            "/start",
            get(move || {
                let me = me.clone();
                async move {
                    match me.start().await {
                        Ok(()) => (StatusCode::OK, "ok\n".to_owned()).into_response(),
                        Err(e @ StartError::Releasing) => (StatusCode::CONFLICT, format!("{e}\n")).into_response(),
                        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}\n")).into_response(),
                    }
                }
            }),
        )
    }
}

/// `GET /_ping` on the Docker API socket answers 200.
pub async fn ping(socket: &Path) -> bool {
    let attempt = async {
        let stream = tokio::net::UnixStream::connect(socket).await.ok()?;
        let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.ok()?;
        tokio::spawn(conn);
        let req = hyper::Request::get("/_ping")
            .header(hyper::header::HOST, "docker")
            .body(Empty::<Bytes>::new())
            .ok()?;
        let res = send.send_request(req).await.ok()?;
        let ok = res.status() == StatusCode::OK;
        let _ = res.into_body().collect().await;
        Some(ok)
    };
    tokio::time::timeout(Duration::from_secs(2), attempt)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in dockerd: serves `GET /_ping` on the socket named by its first argument, after `delay` seconds.
    const FAKE_DOCKERD: &str = r#"
import os, socket, sys, time
time.sleep(float(sys.argv[2]))
path = sys.argv[1]
try: os.unlink(path)
except FileNotFoundError: pass
s = socket.socket(socket.AF_UNIX); s.bind(path); s.listen(8)
while True:
    c, _ = s.accept()
    c.recv(4096)
    c.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
    c.close()
"#;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_spawns_once_and_waits_for_ping() {
        let k = crate::keeper::testing::Running::start(None);
        let dir = k.dir.clone();
        let script = dir.join("dockerd.py");
        std::fs::write(&script, FAKE_DOCKERD).unwrap();
        let api = dir.join("docker.sock");
        let paths = DockerPaths {
            dockerd: vec![
                "/usr/bin/python3".into(),
                script.to_string_lossy().into_owned(),
                api.to_string_lossy().into_owned(),
                "0.5".into(),
            ],
            daemon_json: dir.join("etc/daemon.json"),
            log: dir.join("dockerd.log"),
            api_socket: api.clone(),
        };
        // a non-root keeper spawns as its own user; uid 0 is asked for, so run the test only where that matches
        let root = std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0);
        let exits = Exits::new(k.client.clone(), 0);
        let _w = tokio::spawn(exits.clone().run());
        let d = Docker::new(k.client.clone(), exits, paths, vec!["PATH=/usr/bin:/bin".into()]);
        let sock = dir.join("ctl.sock");
        let _srv = serve_unix(&sock, 0o666, d.router()).unwrap();
        if root {
            let t = std::time::Instant::now();
            d.start().await.unwrap();
            assert!(t.elapsed() >= Duration::from_millis(400));
            assert_eq!(
                std::fs::read_to_string(dir.join("etc/daemon.json")).unwrap(),
                DAEMON_JSON
            );
            // a second ask finds it answering and spawns nothing
            d.start().await.unwrap();
            let st = crate::keeper::Client::new(&dir.join("run")).status().unwrap();
            assert_eq!(st.children.iter().filter(|(_, n)| n == "dockerd").count(), 1);
            // ci.yml's root step looks for this line
            #[allow(clippy::print_stderr)]
            {
                eprintln!("dockerd spawned as root and answered _ping");
            }
        } else {
            // not root: the keeper refuses uid 0, and the answer says why
            let e = d.start().await.unwrap_err().to_string();
            assert!(e.contains("spawn dockerd"), "{e}");
        }
        assert!(!ping(&dir.join("nothing.sock")).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn after_a_supervisor_restart_the_running_dockerd_is_used() {
        let k = crate::keeper::testing::Running::start(None);
        let dir = k.dir.clone();
        let script = dir.join("dockerd.py");
        std::fs::write(&script, FAKE_DOCKERD).unwrap();
        let api = dir.join("docker.sock");
        // a predecessor's dockerd, still starting (it answers after 1 s)
        let mut spec = k.spec(
            "dockerd",
            &["/usr/bin/python3", script.to_str().unwrap(), api.to_str().unwrap(), "1"],
        );
        spec.setsid = true;
        k.client.spawn(&spec).unwrap();
        // the new supervisor's Docker knows no pid; the spawn it would make (uid 0) is not needed
        let paths = DockerPaths {
            dockerd: vec!["/nonexistent/dockerd".into()],
            daemon_json: dir.join("etc/daemon.json"),
            log: dir.join("dockerd.log"),
            api_socket: api.clone(),
        };
        let exits = Exits::new(k.client.clone(), 0);
        let _w = tokio::spawn(exits.clone().run());
        let d = Docker::new(k.client.clone(), exits, paths, vec![]);
        d.start().await.unwrap();
        d.start().await.unwrap();
        let st = k.client.status().unwrap();
        assert_eq!(st.children.iter().filter(|(_, n)| n == "dockerd").count(), 1);
        assert!(!dir.join("etc/daemon.json").exists(), "nothing was spawned");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dockerd_that_exits_is_answered_at_once_and_a_release_refuses() {
        use crate::lifecycle::tests::{FakeAgent, FakeFs};
        let k = crate::keeper::testing::Running::start(None);
        let dir = k.dir.clone();
        let api = dir.join("docker.sock");
        // a predecessor's dockerd that dies during the wait (exit 3 after 0.3 s)
        let spec = k.spec("dockerd", &["/bin/sh", "-c", "sleep 0.3; exit 3"]);
        k.client.spawn(&spec).unwrap();
        let exits = Exits::new(k.client.clone(), 0);
        let _w = tokio::spawn(exits.clone().run());
        let paths = DockerPaths {
            dockerd: vec!["/nonexistent/dockerd".into()],
            daemon_json: dir.join("etc/daemon.json"),
            log: dir.join("dockerd.log"),
            api_socket: api,
        };
        let d = Docker::new(k.client.clone(), exits, paths, vec![]);
        let began = std::time::Instant::now();
        let why = d.start().await.unwrap_err().to_string();
        assert!(why.contains("exited before it answered (code 3"), "{why}");
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "not the 60 s wait: {:?}",
            began.elapsed()
        );
        // once a release has begun: 409, nothing spawned
        let l = Lifecycle::new(FakeAgent::new(), Arc::new(FakeFs::default()));
        d.gate(&l);
        let rel =
            supervisor_api::control::ReleaseRequest::new(hermit_api::sandbox::ReleaseReason::Deleted, true, 10_000)
                .unwrap();
        let _ = l.release(rel).await;
        assert_eq!(d.start().await, Err(StartError::Releasing));
        let sock = dir.join("ctl.sock");
        let _srv = serve_unix(&sock, 0o666, d.router()).unwrap();
        let stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(conn);
        let req = hyper::Request::get("/start")
            .header(hyper::header::HOST, "docker")
            .body(Empty::<Bytes>::new())
            .unwrap();
        assert_eq!(send.send_request(req).await.unwrap().status(), StatusCode::CONFLICT);
    }
}
