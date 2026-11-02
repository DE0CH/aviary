//! The `model` endpoint (supervisor 14, 17.3): the model ids Claude Code runs with, and the title call.

use futures_util::StreamExt as _;
use gateway_api::models::Models;
use magpie_api::sandbox::{AckResult, TitleCommand};
use serde_json::{Value, json};

use crate::limits::{TITLE_MAX_BYTES, TITLE_TIMEOUT};

/// The fixed name.
pub const BASE: &str = "http://model";

/// The client of `model` over the fixed-names client.
#[derive(Clone, Debug)]
pub struct Model {
    http: reqwest::Client,
    base: String,
}

impl Model {
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_base(http, BASE)
    }

    /// At another base (tests).
    #[must_use]
    pub fn with_base(http: reqwest::Client, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// `GET /v1/aviary/models`.
    ///
    /// # Errors
    /// The route's or the transport's error, as text.
    pub async fn models(&self) -> Result<Models, String> {
        gateway_api::models::fetch(&self.http, &self.base, None)
            .await
            .map_err(|e| e.to_string())
    }

    /// The title call: `POST /v1/messages` with the title model, the deployment's title instructions and magpie's
    /// prompt; the ack carries the response JSON as is, or `<status> <first 500 bytes>`.
    pub async fn title(&self, system: &str, cmd: &TitleCommand) -> AckResult {
        let error = |e: String| AckResult {
            error: Some(e),
            ..AckResult::default()
        };
        let models = match self.models().await {
            Ok(m) => m,
            Err(e) => return error(format!("models: {e}")),
        };
        let body = json!({
            "model": models.title,
            "max_tokens": cmd.max_tokens,
            "system": system,
            "messages": [{"role": "user", "content": cmd.user_prompt}],
        });
        let sent = self
            .http
            .post(format!("{}{}", self.base, gateway_api::models::MESSAGES))
            .header("anthropic-version", "2023-06-01")
            .timeout(TITLE_TIMEOUT)
            .json(&body)
            .send()
            .await;
        let res = match sent {
            Ok(r) => r,
            Err(e) => return error(format!("unreachable: {e}")),
        };
        let status = res.status();
        let mut bytes = Vec::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => {
                    if bytes.len() + c.len() > TITLE_MAX_BYTES {
                        return error(format!("{} response over {TITLE_MAX_BYTES} bytes", status.as_u16()));
                    }
                    bytes.extend_from_slice(&c);
                }
                Err(e) => return error(format!("{} {e}", status.as_u16())),
            }
        }
        if !status.is_success() {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(500)]).into_owned();
            return error(format!("{} {head}", status.as_u16()));
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => AckResult {
                response: Some(v),
                ..AckResult::default()
            },
            Err(e) => error(format!("{} not JSON: {e}", status.as_u16())),
        }
    }
}
