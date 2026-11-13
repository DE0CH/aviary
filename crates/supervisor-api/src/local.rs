//! The local socket `/run/supervisor/agent.sock` (mode 0666), HTTP/1.1 over a Unix socket, for processes inside the
//! sandbox (supervisor 13, 15.2). No protocol number: both ends ship in the same supervisor binary.
//!
//! - `POST /v1/hooks/send-user-file`: the `SendUserFile` hook process sends [`SendUserFileRequest`] with the open file
//!   descriptors of `files` (in order) attached to the request's first message as `SCM_RIGHTS` (at most
//!   [`MAX_FDS`]); 200 [`SendUserFileReply`] when magpie stored every file, else an error envelope whose `message`
//!   is the reason the hook prints. The acceptor reads the descriptors and attaches them to the request as
//!   [`ReceivedFds`] before routing.
//! - `GET /v1/secrets/{key}`: 200 [`crate::SecretValue`] or 404 `not_granted`, answered from hermit at that moment.

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The socket path.
pub const AGENT_SOCKET: &str = "/run/supervisor/agent.sock";
/// The docker wrapper's socket (dockerd on demand, supervisor 4.6).
pub const DOCKER_SOCKET: &str = "/run/supervisor/docker.sock";

pub const SEND_USER_FILE: &str = "/v1/hooks/send-user-file";
pub const SECRET: &str = "/v1/secrets/{key}";

/// The most descriptors one hook call carries.
pub const MAX_FDS: usize = 100;
/// How long the hook waits for the answer.
pub const HOOK_WAIT: Duration = Duration::from_secs(110);
/// The hook's exit code when sending failed (it blocks the tool so the model knows).
pub const HOOK_FAILED_EXIT: i32 = 2;
/// `sandbox-secret <KEY>`'s exit code for a key that is not granted.
pub const SECRET_NOT_GRANTED_EXIT: i32 = 3;

/// What the hook prints to stderr on failure (today's text).
#[must_use]
pub fn hook_failure_text(reason: &str) -> String {
    format!("The files were not sent: they could not be saved for the user ({reason}). Try sending them again.")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendUserFileRequest {
    /// The tool call the files belong to; empty from the `send-file` command, whose call (the shell command that
    /// runs it) the supervisor knows.
    #[serde(default)]
    pub tool_use_id: String,
    #[serde(default)]
    pub caption: Option<String>,
    /// In the order of the attached descriptors.
    pub files: Vec<SentFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentFile {
    /// Absolute, under `/workspace`, no symlink component.
    pub path: String,
    pub size: u64,
}

/// 200 of the hook route: how many files magpie stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendUserFileReply {
    pub sent: u32,
}

/// The descriptors received with a request, as a request extension (taken once).
#[derive(Clone, Debug, Default)]
pub struct ReceivedFds(Arc<Mutex<Vec<OwnedFd>>>);

impl ReceivedFds {
    #[must_use]
    pub fn new(fds: Vec<OwnedFd>) -> Self {
        Self(Arc::new(Mutex::new(fds)))
    }

    /// Adds descriptors (the acceptor, once the connection's first message is read).
    pub fn put(&self, fds: Vec<OwnedFd>) {
        if let Ok(mut v) = self.0.lock() {
            v.extend(fds);
        }
    }

    /// Takes the descriptors (empty after the first call).
    #[must_use]
    pub fn take(&self) -> Vec<OwnedFd> {
        self.0.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
    }
}
