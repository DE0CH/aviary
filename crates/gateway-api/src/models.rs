//! The model route (platform R18) and the other fixed-name paths.
//!
//! `GET http://model/v1/aviary/models` serves both pigeon (in-cluster with its pair token) and the supervisor
//! (through the gateway); `POST /v1/messages` and `/v1/messages/count_tokens` are the Messages API (Claude Code's
//! calls), `POST /v1/chat/completions` the `OpenAI` chat completions API (Qwen Code's) and `POST /v1/embeddings` the
//! `OpenAI` embeddings API (the supervisor's embeddings of its conversation's messages, pigeon's of a search), each
//! relayed with the implementation's credential.

use std::time::Duration;

pub use aviary_core::harness::Harness;
use serde::{Deserialize, Serialize};

pub const MESSAGES: &str = "/v1/messages";
pub const COUNT_TOKENS: &str = "/v1/messages/count_tokens";
pub const CHAT_COMPLETIONS: &str = "/v1/chat/completions";
pub const MODELS: &str = "/v1/aviary/models";
/// The `OpenAI` embeddings API: `{model, input: [text…], dimensions, encoding_format: "float"}` →
/// `{data: [{index, embedding: [f32…]}]}`. The model is the models route's [`Models::embedding`]; a sandbox calls it
/// through the proxy, pigeon in-cluster with its pair token.
pub const EMBEDDINGS: &str = "/v1/embeddings";
/// The length of every embedding (the request's `dimensions`): magpie's vector column has this many.
pub const EMBEDDING_DIMENSIONS: usize = 1024;
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
    /// The embedding model of the conversation search (its messages' vectors and a query's). Absent: the deployment
    /// has none, and the search matches words only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Embedding>,
}

/// The models route's embedding model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Embedding {
    /// The id sent as the embeddings call's `model` (`qwen/qwen3-embedding-8b`). It must give
    /// [`EMBEDDING_DIMENSIONS`] numbers when asked for them.
    pub id: String,
    /// Put before a search query (not before a message) when the model wants one (Qwen3's
    /// `Instruct: …\nQuery:`). Absent: the query as typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_instruction: Option<String>,
}

/// One embeddings answer item.
#[derive(Clone, Debug, Deserialize)]
struct EmbeddingItem {
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Clone, Debug, Deserialize)]
struct EmbeddingsAnswer {
    data: Vec<EmbeddingItem>,
}

/// The embeddings call's body for `texts`: the fastest host first (`provider.sort: latency`, the model provider's
/// routing; a search waits for its query's vector, and an embedding model's hosts answer the same vector), which a
/// gateway may add to (zero data retention beside it, say).
#[must_use]
pub fn embeddings_request(model: &str, texts: &[String]) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "input": texts,
        "dimensions": EMBEDDING_DIMENSIONS,
        "encoding_format": "float",
        "provider": {"sort": "latency"},
    })
}

/// The vectors of an embeddings answer, in the order of the request's `input` (by `index`). `None` when the answer
/// does not parse, has another count than `n`, or a vector is not [`EMBEDDING_DIMENSIONS`] long or not finite.
#[must_use]
pub fn embeddings_from(body: &[u8], n: usize) -> Option<Vec<Vec<f32>>> {
    let a: EmbeddingsAnswer = serde_json::from_slice(body).ok()?;
    if a.data.len() != n {
        return None;
    }
    let mut out: Vec<Option<Vec<f32>>> = vec![None; n];
    for item in a.data {
        if item.embedding.len() != EMBEDDING_DIMENSIONS || !item.embedding.iter().all(|x| x.is_finite()) {
            return None;
        }
        let slot = out.get_mut(item.index)?;
        if slot.is_some() {
            return None;
        }
        *slot = Some(item.embedding);
    }
    out.into_iter().collect()
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
    /// The harness a conversation on this model runs in. Absent: Claude Code.
    #[serde(default, skip_serializing_if = "Harness::is_default")]
    pub harness: Harness,
    /// The people (emails) who may pick this model. Empty: everyone. A model being tried is listed for the people
    /// trying it and is no one else's to see or pick.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_to: Vec<String>,
}

impl Models {
    /// The harness of the model `id` (an id the route does not list: the default harness).
    #[must_use]
    pub fn harness_of(&self, id: &str) -> Harness {
        self.models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.harness)
            .unwrap_or_default()
    }

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

    /// The embedding model is optional and says nothing on the wire when absent; an answer's vectors come back in the
    /// input's order whatever order the provider lists them in, and a short or extra vector refuses the answer.
    #[test]
    fn the_embedding_model_and_its_answers() {
        let none: Models =
            serde_json::from_str(r#"{"default":"a","small":"a","title":"a","contextTokens":1,"models":[]}"#).unwrap();
        assert_eq!(none.embedding, None);
        assert_eq!(serde_json::to_value(&none).unwrap().get("embedding"), None);
        let some: Models = serde_json::from_str(
            r#"{"default":"a","small":"a","title":"a","contextTokens":1,"models":[],
                "embedding":{"id":"qwen/qwen3-embedding-8b","queryInstruction":"Instruct: find\nQuery:"}}"#,
        )
        .unwrap();
        let e = some.embedding.unwrap();
        assert_eq!(e.id, "qwen/qwen3-embedding-8b");
        assert_eq!(e.query_instruction.as_deref(), Some("Instruct: find\nQuery:"));
        let req = embeddings_request("m", &["a".into(), "b".into()]);
        assert_eq!(req["dimensions"], EMBEDDING_DIMENSIONS);
        assert_eq!(req["provider"]["sort"], "latency");
        assert_eq!(req["input"][1], "b");
        let v = |x: f32| vec![x; EMBEDDING_DIMENSIONS];
        let body = serde_json::json!({"data": [{"index": 1, "embedding": v(2.0)}, {"index": 0, "embedding": v(1.0)}]});
        let got = embeddings_from(&serde_json::to_vec(&body).unwrap(), 2).unwrap();
        assert!((got[0][0] - 1.0).abs() < f32::EPSILON && (got[1][0] - 2.0).abs() < f32::EPSILON);
        assert!(embeddings_from(&serde_json::to_vec(&body).unwrap(), 3).is_none());
        let short = serde_json::json!({"data": [{"index": 0, "embedding": [1.0, 2.0]}]});
        assert!(embeddings_from(&serde_json::to_vec(&short).unwrap(), 1).is_none());
        let twice = serde_json::json!({"data": [{"index": 0, "embedding": v(1.0)}, {"index": 0, "embedding": v(1.0)}]});
        assert!(embeddings_from(&serde_json::to_vec(&twice).unwrap(), 2).is_none());
    }

    /// A model names the harness its conversations run in; one that names none runs in Claude Code, and says
    /// nothing on the wire.
    #[test]
    fn a_model_names_its_harness() {
        let m: Models = serde_json::from_str(
            r#"{"default":"a","small":"a","title":"a","contextTokens":200000,"models":[
                {"id":"a","label":"A","reasoning":null},
                {"id":"video","label":"Video","reasoning":null,"harness":"qwen"}]}"#,
        )
        .unwrap();
        assert_eq!(m.harness_of("a"), Harness::Claude);
        assert_eq!(m.harness_of("video"), Harness::Qwen);
        assert_eq!(m.harness_of("unknown"), Harness::Claude);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["models"][0].get("harness"), None);
        assert_eq!(v["models"][1]["harness"], "qwen");
        assert_eq!(
            v["models"][1].get("visibleTo"),
            None,
            "a model for everyone names no one"
        );
        let limited: Model =
            serde_json::from_str(r#"{"id":"v","label":"V","reasoning":null,"visibleTo":["a@example.com"]}"#).unwrap();
        assert_eq!(limited.visible_to, ["a@example.com"]);
        assert!(serde_json::from_str::<Models>(
            r#"{"default":"a","small":"a","title":"a","contextTokens":1,"models":[{"id":"a","label":"A","reasoning":null,"harness":"codex"}]}"#
        )
        .is_err());
    }
}
