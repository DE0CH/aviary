//! The local socket `/run/supervisor/agent.sock` (mode 0666) for processes inside the sandbox (supervisor 13,
//! 15.2), and `sandbox-secret <KEY>`, its command-line client.
//!
//! - `GET /v1/secrets/{key}`: asks hermit at that moment (no cache); 200 `{"key","value"}` or 404 `not_granted`.
//!   The key is logged, never the value, and no value is written to disk or an environment.
//! - `POST /v1/hooks/send-user-file`: the `SendUserFile` hook, with its descriptors, handed to the driver
//!   ([`crate::lifecycle::Agent::send_user_file`]); 502 `upstream_failed` with the reason when the files were not kept.
//! - `POST /v1/questions`, `GET /v1/questions/{askId}`: the `ask` command's question, passed to magpie with the
//!   lease ([`crate::ask`]); each poll is a wait on the person until the answer.
//! - `POST`/`DELETE /v1/person-waits/{key}`: a command that waits on the person for anything else (Jarvis 4's Inbox
//!   requests): the heartbeat's `waiting` while it calls (Deyao, 2026-11-18: the list's needs-you mark). Its card
//!   (what is asked, why, the marker) is passed to magpie once per key, and the outcome when the wait ends with one,
//!   before the `DELETE` answers (magpie rule 11: the chat shows the wait where it was asked until the AI has read the
//!   outcome, and reads only a settled card).

use std::io::Write as _;
use std::path::Path;

use axum::Router;
use hermit_api::sandbox_surface::{SecretValue, valid_secret_key};
use hermit_api::server::{Answer, Ctx};
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use supervisor_api::local::{
    AGENT_SOCKET, PersonWaitCard, PersonWaitEnded, QuestionAnswer, QuestionOpen, QuestionOpened, QuestionWait,
    SECRET_NOT_GRANTED_EXIT,
};
use supervisor_api::server::LocalServer;

use crate::hermit::Hermit;

/// The local socket's server.
#[derive(Clone)]
pub struct Local {
    pub hermit: Hermit,
    pub agent: std::sync::Arc<dyn crate::lifecycle::Agent>,
    /// The person waits whose card magpie has (opened once per key by this process).
    pub carded: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
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

    async fn question_open(&self, _ctx: Ctx, body: QuestionOpen) -> Answer<QuestionOpened> {
        let calls = self.agent.magpie_calls().ok_or_else(no_magpie)?;
        let r = calls
            .question_open(&magpie_api::sandbox::QuestionRequest {
                input: body.input,
                // this binary's `ask` prints the answer's marker on every line (rule 11)
                marks_answers: true,
            })
            .await
            .map_err(|e| upstream(&e))?;
        Ok(QuestionOpened { ask_id: r.ask_id })
    }

    async fn question_get(&self, _ctx: Ctx, ask_id: String, q: QuestionWait) -> Answer<QuestionAnswer> {
        let key = format!("question:{ask_id}");
        self.agent.waiting_on_the_person(&key, true);
        let calls = self.agent.magpie_calls().ok_or_else(no_magpie)?;
        let r = calls
            .question_get(&ask_id, &magpie_api::sandbox::QuestionQuery { wait_ms: q.wait_ms })
            .await
            .map_err(|e| match e.api().map(|r| r.status) {
                Some(404) => {
                    self.agent.waiting_on_the_person(&key, false);
                    aviary_core::http::ApiError::not_found("no such question")
                }
                _ => upstream(&e),
            })?;
        if r.answer.is_some() {
            // answered: the command no longer waits on the person
            self.agent.waiting_on_the_person(&key, false);
        }
        Ok(QuestionAnswer {
            ask_id: r.ask_id,
            answer: r.answer.and_then(|a| serde_json::to_value(a).ok()),
        })
    }

    async fn person_wait(
        &self,
        _ctx: Ctx,
        key: String,
        waiting: bool,
        q: QuestionWait,
        card: Option<PersonWaitCard>,
        ended: PersonWaitEnded,
    ) -> Answer<()> {
        if !supervisor_api::local::valid_wait_key(&key) {
            return Err(aviary_core::http::ApiError::bad_request(
                "a wait key is 1 to 100 of A-Za-z0-9._:-",
            ));
        }
        self.agent.waiting_on_the_person(&format!("wait:{key}"), waiting);
        if let Some(card) = card.filter(|_| waiting) {
            self.open_card(&key, card).await;
        }
        if !waiting {
            self.carded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&key);
            if let Some(outcome) = ended.outcome {
                // before the answer: the command prints the outcome only after it, so magpie has the card settled
                // before any result can carry the outcome's marker (rule 11 reads only a settled card)
                self.end_card(&key, outcome).await;
            }
        }
        if waiting {
            let max = supervisor_api::local::QUESTION_WAIT_MAX;
            let hold = q
                .wait_ms
                .map_or(max, |ms| std::time::Duration::from_millis(u64::from(ms)).min(max));
            self.agent.clock().sleep(hold).await;
        }
        Ok(())
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

impl Local {
    /// Passes a wait's card to magpie once per key; a failure is retried at the command's next call.
    async fn open_card(&self, key: &str, card: PersonWaitCard) {
        if self
            .carded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(key)
        {
            return;
        }
        let Some(calls) = self.agent.magpie_calls() else { return };
        let open = magpie_api::sandbox::PersonWaitOpen {
            label: card.label,
            title: card.title,
            why: card.why,
            marker: card.marker,
        };
        match calls.person_wait_open(key, &open).await {
            Ok(_) => {
                self.carded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(key.to_owned());
            }
            Err(e) => tracing::warn!(key, error = %e, "a person wait's card was not opened; the next call tries again"),
        }
    }

    /// Passes a wait's outcome to magpie, a few times over 15 s when it fails.
    async fn end_card(&self, key: &str, outcome: String) {
        let Some(calls) = self.agent.magpie_calls() else { return };
        let end = magpie_api::sandbox::PersonWaitEnd { outcome };
        for attempt in 0..5u32 {
            match calls.person_wait_ended(key, &end).await {
                Ok(()) => return,
                // no card was opened for it (a command that gave none): nothing to end
                Err(e) if e.api().map(|r| r.status) == Some(404) => return,
                Err(e) => {
                    tracing::warn!(key, error = %e, attempt, "a person wait's outcome did not reach magpie");
                    if attempt < 4 {
                        self.agent
                            .clock()
                            .sleep(std::time::Duration::from_secs(1u64 << attempt))
                            .await;
                    }
                }
            }
        }
    }
}

fn no_magpie() -> aviary_core::http::ApiError {
    aviary_core::http::ApiError::unavailable("no agent driver", 1)
}

fn upstream(e: &magpie_api::client::ClientError) -> aviary_core::http::ApiError {
    aviary_core::http::ApiError::new(axum::http::StatusCode::BAD_GATEWAY, "upstream_failed", e.to_string())
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
