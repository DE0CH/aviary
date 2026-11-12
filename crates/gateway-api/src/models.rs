//! The model route (platform R18) and the other fixed-name paths.
//!
//! `GET http://model/v1/aviary/models` serves both pigeon (in-cluster with its pair token) and the supervisor
//! (through the gateway); `POST /v1/messages` and `/v1/messages/count_tokens` are the Messages API relayed with the
//! implementation's credential.

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const MESSAGES: &str = "/v1/messages";
pub const COUNT_TOKENS: &str = "/v1/messages/count_tokens";
pub const MODELS: &str = "/v1/aviary/models";
/// `tools` (decision 15: REST, no MCP): `GET /v1/tools` lists the tools, `POST /v1/tools/<name>` calls one with its
/// JSON arguments, `GET /v1/openapi.json` describes both.
pub const TOOLS_LIST: &str = "/v1/tools";
pub const TOOLS_CALL_PREFIX: &str = "/v1/tools/";
pub const TOOLS_OPENAPI: &str = "/v1/openapi.json";

/// `GET /v1/aviary/models`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Models {
    /// The model id Claude Code runs (`ANTHROPIC_MODEL`).
    pub default: String,
    /// The id for Claude Code's small fast calls.
    pub small: String,
    /// The id for titles.
    pub title: String,
    /// The models' context window (a model may name its own: [`Model::context_tokens`]).
    pub context_tokens: u64,
    pub models: Vec<Model>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub label: String,
    /// The reasoning level the gateway applies to this model, if any: with `thinking`, the level a conversation
    /// starts on.
    pub reasoning: Option<String>,
    /// The thinking levels a person may choose for this model, lowest first (`none` = thinking off, then the
    /// gateway's effort names). Empty: no choice, every turn runs at `reasoning`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking: Vec<String>,
    /// This model's context window, when it is not the route's `contextTokens` (a deployment whose models differ:
    /// a 1M model beside 200k ones). Absent: the route's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
}

impl Models {
    /// The context window of the model `id`: its own when it names one, else the route's `contextTokens` (0: none
    /// named, Claude Code keeps its own for the model).
    #[must_use]
    pub fn context_of(&self, id: &str) -> u64 {
        self.models
            .iter()
            .find(|m| m.id == id)
            .and_then(|m| m.context_tokens)
            .unwrap_or(self.context_tokens)
    }
}

pub const MODELS_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads the models route. `base` is `http://model` (through the proxy configured on `http`) or the in-cluster
/// Service; `bearer` is pigeon's pair token, `None` for a sandbox (its credential is the proxy's).
///
/// # Errors
/// Transport, non-2xx (as [`hermit_api::ClientError::Api`]) or decode failures.
pub async fn fetch(
    http: &reqwest::Client,
    base: &str,
    bearer: Option<&str>,
) -> Result<Models, hermit_api::ClientError> {
    let mut rb = http
        .get(format!("{}{MODELS}", base.trim_end_matches('/')))
        .timeout(MODELS_TIMEOUT)
        .header(
            hermit_api::headers::PROTOCOL,
            format!("{}={}", crate::GATEWAY.0, crate::GATEWAY.1),
        );
    if let Some(t) = bearer {
        rb = rb.bearer_auth(t);
    }
    let res = rb.send().await?;
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.bytes().await?;
    if !status.is_success() {
        return Err(hermit_api::ClientError::Api(hermit_api::client::refusal(
            status, &headers, &bytes,
        )));
    }
    serde_json::from_slice(&bytes).map_err(|e| hermit_api::ClientError::Decode {
        status: status.as_u16(),
        message: e.to_string(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A deployment with a 1M model beside 200k ones (Jarvis 4: GLM 5.3 beside the Claude models, 2026-11-12): the
    /// model's own window wins, every other model and an unknown id take the route's.
    #[test]
    fn a_model_s_own_context_window_wins_over_the_route_s() {
        let m: Models = serde_json::from_str(
            r#"{"default":"a","small":"a","title":"a","contextTokens":200000,"models":[
                {"id":"a","label":"A","reasoning":null},
                {"id":"big","label":"Big","reasoning":null,"contextTokens":1000000}]}"#,
        )
        .unwrap();
        assert_eq!(m.context_of("a"), 200_000);
        assert_eq!(m.context_of("big"), 1_000_000);
        assert_eq!(m.context_of("unknown"), 200_000);
        // absent stays absent on the wire (older readers and fixtures see no new member)
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["models"][0].get("contextTokens"), None);
        assert_eq!(v["models"][1]["contextTokens"], 1_000_000);
    }
}
