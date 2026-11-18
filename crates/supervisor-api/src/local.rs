//! The local socket `/run/supervisor/agent.sock` (mode 0666), HTTP/1.1 over a Unix socket, for processes inside the
//! sandbox (supervisor 13, 15.2). No protocol number: both ends ship in the same supervisor binary.
//!
//! - `POST /v1/hooks/send-user-file`: the `SendUserFile` hook process sends [`SendUserFileRequest`] with the open file
//!   descriptors of `files` (in order) attached to the request's first message as `SCM_RIGHTS` (at most
//!   [`MAX_FDS`]); 200 [`SendUserFileReply`] when magpie stored every file, else an error envelope whose `message`
//!   is the reason the hook prints. The acceptor reads the descriptors and attaches them to the request as
//!   [`ReceivedFds`] before routing.
//! - `GET /v1/secrets/{key}`: 200 [`crate::SecretValue`] or 404 `not_granted`, answered from hermit at that moment.
//! - `POST /v1/questions`: the `ask` command opens a question ([`QuestionOpen`]); 200 [`QuestionOpened`]. The
//!   supervisor passes it to magpie with its lease and takes no other part: the question belongs to the command.
//! - `GET /v1/questions/{askId}?waitMs=`: the question's state ([`QuestionAnswer`]), held up to `waitMs` (at most
//!   [`QUESTION_WAIT_MAX`]) while it waits; 404 `not_found` for an id no `ask` opened.
//! - `POST /v1/person-waits/{key}?waitMs=`: a command that waits on the person for something other than a question
//!   (a deployment's request waiting for the person's approval, e.g. Jarvis 4's Inbox) says so, held up to `waitMs`
//!   (at most [`QUESTION_WAIT_MAX`]) and called again while it waits; `DELETE` the same when its wait ends. 204.
//!   Like a question poll, it keeps the run from being silent and sets the heartbeat's `waiting` (Deyao,
//!   2026-11-18: the conversation list shows the conversation as needing the person). `key`: 1 to 100 of
//!   `A-Za-z0-9._:-`, the command's own. The `POST` may carry [`PersonWaitCard`] (what is asked, why, and the text
//!   the command's output carries once it has the outcome): the chat then shows the wait as a card where it was
//!   asked until the AI has read the outcome (magpie rule 11). The `DELETE` may carry `?outcome=WORD`, the outcome
//!   the card shows; without one the wait ended for another reason (the command was cancelled) and the card waits.

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
pub const QUESTIONS: &str = "/v1/questions";
pub const QUESTION: &str = "/v1/questions/{askId}";
pub const PERSON_WAIT: &str = "/v1/person-waits/{key}";

/// The longest a question poll is held.
pub const QUESTION_WAIT_MAX: Duration = Duration::from_secs(25);

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

/// The body of `POST /v1/questions`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOpen {
    /// `AskUserQuestion`'s shape: `{"questions": [{question, header, options: [{label, description}], multiSelect}]}`.
    pub input: serde_json::Map<String, serde_json::Value>,
}

/// 200 of `POST /v1/questions`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOpened {
    pub ask_id: String,
}

/// The query of `GET /v1/questions/{askId}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionWait {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u32>,
}

/// The card of a person wait (`POST /v1/person-waits/{key}` body, optional).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonWaitCard {
    /// What the card calls the wait ("Inbox request"); empty: the app's own word.
    #[serde(default)]
    pub label: String,
    /// What is asked, in a line.
    pub title: String,
    /// Why.
    #[serde(default)]
    pub why: String,
    /// The text the command's output carries once it has the outcome.
    pub marker: String,
}

/// `DELETE /v1/person-waits/{key}` query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonWaitEnded {
    /// The outcome, one word (`approved`, `denied`, `lapsed`, …); none when the wait ended without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Whether `key` names a person wait (1 to 100 of `A-Za-z0-9._:-`).
#[must_use]
pub fn valid_wait_key(key: &str) -> bool {
    (1..=100).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// 200 of `GET /v1/questions/{askId}`: `answer` is magpie's resolution (`{"behavior", "updatedInput", "message"}`),
/// absent while the question waits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionAnswer {
    pub ask_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<serde_json::Value>,
}
