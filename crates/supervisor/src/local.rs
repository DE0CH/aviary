//! The local socket `/run/supervisor/agent.sock` (mode 0666) for processes inside the sandbox (supervisor 13,
//! 15.2), and `sandbox-secret <KEY>`, its command-line client.
//!
//! - `GET /v1/secrets/{key}`: asks hermit at that moment (no cache); 200 `{"key","value"}` or 404 `not_granted`.
//!   The key is logged, never the value, and no value is written to disk or an environment.
//! - `POST /v1/hooks/send-user-file`: the `SendUserFile` hook, with its descriptors, handed to the driver
//!   ([`crate::lifecycle::Agent::send_user_file`]); 502 `upstream_failed` with the reason when the files were not kept.

use std::io::Write as _;
use std::path::Path;

use axum::Router;
use hermit_api::sandbox_surface::{SecretValue, valid_secret_key};
use hermit_api::server::{Answer, Ctx};
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use supervisor_api::local::{AGENT_SOCKET, SECRET_NOT_GRANTED_EXIT};
use supervisor_api::server::LocalServer;

use crate::hermit::Hermit;

/// The local socket's server.
#[derive(Clone)]
pub struct Local {
    pub hermit: Hermit,
    pub agent: std::sync::Arc<dyn crate::lifecycle::Agent>,
}

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local").finish_non_exhaustive()
    }
}

impl LocalServer for Local {
    async fn send_user_file(
        &self,
        _ctx: Ctx,
        body: supervisor_api::local::SendUserFileRequest,
        fds: Vec<std::os::fd::OwnedFd>,
    ) -> Answer<supervisor_api::local::SendUserFileReply> {
        self.agent.send_user_file(body, fds).await.map_err(|reason| {
            aviary_core::http::ApiError::new(axum::http::StatusCode::BAD_GATEWAY, "upstream_failed", reason)
        })
    }

    async fn secret(&self, _ctx: Ctx, key: String) -> Answer<SecretValue> {
        if !valid_secret_key(&key) {
            return Err(hermit_api::error::api(
                404,
                hermit_api::error::codes::NOT_GRANTED,
                "not granted",
            ));
        }
        self.hermit.secret(&key).await.map_err(|e| match e.status() {
            Some(404) => hermit_api::error::api(404, hermit_api::error::codes::NOT_GRANTED, "not granted"),
            _ => aviary_core::http::ApiError::unavailable("hermit unavailable", 1),
        })
    }
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

/// What `sandbox-secret` got.
#[derive(Debug, PartialEq, Eq)]
pub enum SecretAnswer {
    Value(String),
    NotGranted,
    Failed(String),
}

/// Asks the supervisor at `socket` for `key`.
pub async fn ask_secret(socket: &Path, key: &str) -> SecretAnswer {
    let attempt = async {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .map_err(|e| e.to_string())?;
        let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        let req = hyper::Request::get(format!("/v1/secrets/{key}"))
            .header(hyper::header::HOST, "supervisor")
            .body(Empty::<Bytes>::new())
            .map_err(|e| e.to_string())?;
        let res = send.send_request(req).await.map_err(|e| e.to_string())?;
        let status = res.status();
        let body = res.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
        Ok::<_, String>((status, body))
    };
    match attempt.await {
        Ok((s, body)) if s.is_success() => match serde_json::from_slice::<SecretValue>(&body) {
            Ok(v) => SecretAnswer::Value(v.value),
            Err(e) => SecretAnswer::Failed(e.to_string()),
        },
        Ok((s, _)) if s.as_u16() == 404 => SecretAnswer::NotGranted,
        Ok((s, _)) => SecretAnswer::Failed(format!("supervisor answered {s}")),
        Err(e) => SecretAnswer::Failed(e),
    }
}

/// `sandbox-secret <KEY>`: the value on stdout, or exit 3 with `not granted` on stderr.
#[must_use]
pub fn secret_cli(args: &[String]) -> i32 {
    let [key] = args else {
        let _ = writeln!(std::io::stderr(), "usage: sandbox-secret <KEY>");
        return 2;
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return 1;
    };
    match rt.block_on(ask_secret(Path::new(AGENT_SOCKET), key)) {
        SecretAnswer::Value(v) => {
            let mut out = std::io::stdout();
            let _ = out.write_all(v.as_bytes()).and_then(|()| out.flush());
            0
        }
        SecretAnswer::NotGranted => {
            let _ = writeln!(std::io::stderr(), "not granted");
            SECRET_NOT_GRANTED_EXIT
        }
        SecretAnswer::Failed(e) => {
            let _ = writeln!(std::io::stderr(), "sandbox-secret: {e}");
            1
        }
    }
}
