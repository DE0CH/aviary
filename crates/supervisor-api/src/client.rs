//! hermit's client of `:7070` (hermit 3.10): the control token on every call, the timeouts hermit relies on, and a
//! 409 `sandbox_busy` read as [`Outcome::Busy`] rather than an error.

use std::time::Duration;

use hermit_api::client::refusal;
pub use hermit_api::{ClientError, Refusal};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::control::{
    Health, Outcome, PrepareAnswer, PrepareRequest, ReleaseAnswer, ReleaseRequest, Resumed, RunSummary, StateReport,
};
use crate::{CONTROL, ExecRequest, ExecResult, routes};

pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
pub const PREPARE_TIMEOUT: Duration = Duration::from_secs(15);
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(65);
/// Added to a release's `deadlineMs`.
pub const RELEASE_SLACK: Duration = Duration::from_secs(5);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

type R<T> = Result<T, ClientError>;

/// One sandbox's `:7070`.
#[derive(Clone, Debug)]
pub struct ControlClient {
    http: reqwest::Client,
    base: String,
    token: String,
    request_id: Option<String>,
}

impl ControlClient {
    /// `base` is `http://<sandbox address>:7070`; `token` is the sandbox's control token.
    #[must_use]
    pub fn new(http: reqwest::Client, base: impl Into<String>, token: impl Into<String>) -> Self {
        let mut base = base.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self {
            http,
            base,
            token: token.into(),
            request_id: None,
        }
    }

    /// `Aviary-Request` on every call of the returned client.
    #[must_use]
    pub fn request_id(mut self, id: &str) -> Self {
        self.request_id = Some(id.to_owned());
        self
    }

    fn build(&self, method: Method, path: &str, timeout: Duration) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .request(method, format!("{}{path}", self.base))
            .timeout(timeout)
            .bearer_auth(&self.token)
            .header(hermit_api::headers::PROTOCOL, format!("{}={}", CONTROL.0, CONTROL.1));
        if let Some(id) = &self.request_id {
            rb = rb.header(hermit_api::headers::REQUEST, id);
        }
        rb
    }

    async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> R<(StatusCode, Option<T>)> {
        let mut rb = self.build(method, path, timeout);
        if let Some(b) = body {
            rb = rb.json(b);
        }
        let res = rb.send().await?;
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.bytes().await?;
        if !status.is_success() {
            return Err(ClientError::Api(refusal(status, &headers, &bytes)));
        }
        if status == StatusCode::NO_CONTENT {
            return Ok((status, None));
        }
        let v = serde_json::from_slice(&bytes).map_err(|e| ClientError::Decode {
            status: status.as_u16(),
            message: e.to_string(),
        })?;
        Ok((status, Some(v)))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, timeout: Duration) -> R<T> {
        let (status, v) = self.call::<(), T>(Method::GET, path, None, timeout).await?;
        v.ok_or(ClientError::Decode {
            status: status.as_u16(),
            message: "empty body".into(),
        })
    }

    async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B, timeout: Duration) -> R<T> {
        let (status, v) = self.call(Method::POST, path, Some(body), timeout).await?;
        v.ok_or(ClientError::Decode {
            status: status.as_u16(),
            message: "empty body".into(),
        })
    }

    /// `GET /v1/health` (2 s).
    ///
    /// # Errors
    /// No answer, 401, 503 before the descriptor, or a body that does not decode.
    pub async fn health(&self) -> R<Health> {
        self.get(routes::HEALTH, HEALTH_TIMEOUT).await
    }

    /// `GET /v1/state`.
    ///
    /// # Errors
    /// As [`ControlClient::health`].
    pub async fn state(&self) -> R<StateReport> {
        self.get(routes::STATE, DEFAULT_TIMEOUT).await
    }

    /// `POST /v1/lifecycle/prepare` (15 s, or the deadline plus 5 s if longer).
    ///
    /// # Errors
    /// As [`ControlClient::health`]; a 409 `sandbox_busy` is `Ok(Outcome::Busy)`.
    pub async fn prepare(&self, body: &PrepareRequest) -> R<Outcome> {
        let timeout = PREPARE_TIMEOUT.max(Duration::from_millis(body.deadline_ms.into()) + RELEASE_SLACK);
        busy_or(
            self.post::<_, PrepareAnswer>(routes::PREPARE, body, timeout)
                .await
                .map(|_| ()),
        )
    }

    /// `POST /v1/lifecycle/resumed` (204).
    ///
    /// # Errors
    /// As [`ControlClient::health`].
    pub async fn resumed(&self, body: &Resumed) -> R<()> {
        self.call::<_, serde_json::Value>(Method::POST, routes::RESUMED, Some(body), DEFAULT_TIMEOUT)
            .await
            .map(|_| ())
    }

    /// `POST /v1/lifecycle/release` (`deadlineMs` + 5 s).
    ///
    /// # Errors
    /// As [`ControlClient::health`]; a 409 `sandbox_busy` is `Ok(Outcome::Busy)`.
    pub async fn release(&self, body: &ReleaseRequest) -> R<Outcome> {
        let timeout = Duration::from_millis(body.deadline_ms.into()) + RELEASE_SLACK;
        busy_or(
            self.post::<_, ReleaseAnswer>(routes::RELEASE, body, timeout)
                .await
                .map(|_| ()),
        )
    }

    /// `POST /v1/exec` (65 s).
    ///
    /// # Errors
    /// As [`ControlClient::health`].
    pub async fn exec(&self, body: &ExecRequest) -> R<ExecResult> {
        self.post(routes::EXEC, body, EXEC_TIMEOUT).await
    }
}

fn busy_or(r: R<()>) -> R<Outcome> {
    match r {
        Ok(()) => Ok(Outcome::Done),
        Err(ClientError::Api(e)) if e.status == 409 && e.is(hermit_api::error::codes::SANDBOX_BUSY) => {
            let run = e
                .body
                .fields
                .get("run")
                .cloned()
                .and_then(|v| serde_json::from_value::<RunSummary>(v).ok());
            match run {
                Some(run) => Ok(Outcome::Busy(run)),
                None => Err(ClientError::Api(e)),
            }
        }
        Err(e) => Err(e),
    }
}
