//! dockerd on demand (supervisor 4.6). The image's `docker` wrapper calls `GET /start` on
//! `/run/supervisor/docker.sock` (mode 0666, HTTP); the supervisor spawns dockerd through the keeper as root with a
//! fixed `daemon.json`, its output to `/var/log/dockerd.log`, then waits up to 60 s for `/var/run/docker.sock` to
//! answer `_ping`: `200 ok\n` or `500 <error>\n`. dockerd is never started unless asked; whether Docker works at
//! all is the runtime's business.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use tokio::sync::Mutex;

use crate::keeper::{self, SpawnSpec};
use crate::limits::DOCKERD_START_WAIT;

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

/// Starts dockerd once, on the first ask.
#[derive(Debug)]
pub struct Docker {
    keeper: keeper::Client,
    paths: DockerPaths,
    /// The environment dockerd gets (supervisor 6.2: the image's basics plus the descriptor's `env`).
    env: Vec<String>,
    /// dockerd's pid once spawned.
    pid: Mutex<Option<i32>>,
}

impl Docker {
    #[must_use]
    pub fn new(keeper: keeper::Client, paths: DockerPaths, env: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            keeper,
            paths,
            env,
            pid: Mutex::new(None),
        })
    }

    /// Starts dockerd unless it answers already, and waits for `_ping`.
    ///
    /// # Errors
    /// The reason, as the `500` body says it.
    pub async fn start(&self) -> Result<(), String> {
        let mut pid = self.pid.lock().await;
        if pid.is_some() && ping(&self.paths.api_socket).await {
            return Ok(());
        }
        let alive = match *pid {
            Some(p) => {
                let k = self.keeper.clone();
                tokio::task::spawn_blocking(move || k.status())
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .is_some_and(|s| s.children.iter().any(|(c, n)| *c == p && n == "dockerd"))
            }
            None => false,
        };
        if !alive {
            if let Some(dir) = self.paths.daemon_json.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("daemon.json: {e}"))?;
            }
            std::fs::write(&self.paths.daemon_json, DAEMON_JSON).map_err(|e| format!("daemon.json: {e}"))?;
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
            let k = self.keeper.clone();
            let p = tokio::task::spawn_blocking(move || k.spawn(&spec))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("spawn dockerd: {e}"))?;
            tracing::info!(pid = p, "dockerd started");
            *pid = Some(p);
        }
        let deadline = tokio::time::Instant::now() + DOCKERD_START_WAIT;
        while tokio::time::Instant::now() < deadline {
            if ping(&self.paths.api_socket).await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err("dockerd did not answer within 60 s".into())
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

/// Serves `router` on a Unix socket at `path` with `mode` (an old socket removed first).
///
/// # Errors
/// The bind error.
pub fn serve_unix(path: &Path, mode: u32, router: Router) -> std::io::Result<tokio::task::JoinHandle<()>> {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }))
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
        let d = Docker::new(k.client.clone(), paths, vec!["PATH=/usr/bin:/bin".into()]);
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
        } else {
            // not root: the keeper refuses uid 0, and the answer says why
            let e = d.start().await.unwrap_err();
            assert!(e.contains("spawn dockerd"), "{e}");
        }
        assert!(!ping(&dir.join("nothing.sock")).await);
    }
}
