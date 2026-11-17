//! The conversation search (Deyao, 2026-11-17): a person's words found across their conversations, by the words themselves
//! and by meaning.
//!
//! The meaning half is vectors of the conversations' messages (each user and assistant message's text, cut into
//! [`CHUNK_CHARS`]-character pieces). They are made inside each conversation's sandbox: after a turn ends the
//! supervisor asks for the pieces that have no vector yet ([`EmbeddingsTodoQuery`] → [`EmbeddingsTodo`]), has the
//! models route's embedding model turn them into vectors through the gateway (`POST http://model/v1/embeddings`),
//! and stores them ([`EmbeddingsPut`]). pigeon embeds the query itself (in-cluster, with its pair token) and sends
//! the vector with the words ([`ConversationSearchRequest`]).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Timestamp;

/// The length of every vector (magpie's `vector(1024)` column; the supervisor and pigeon ask the embedding model for
/// this many values, `gateway_api::models::EMBEDDING_DIMENSIONS`).
pub const DIMENSIONS: usize = 1024;
/// A message's text is cut into pieces of at most this many characters, each its own vector.
pub const CHUNK_CHARS: usize = 1200;
/// At most this many pieces of one message get a vector (the rest of a very long message is found by its words).
pub const CHUNKS_MAX: usize = 24;
/// A todo page holds at most this many pieces (the default is all of them up to the limit).
pub const TODO_MAX: u32 = 64;
/// A search answers at most this many hits.
pub const SEARCH_MAX: u32 = 50;
/// A snippet's length in characters.
pub const SNIPPET_CHARS: usize = 160;

/// `GET /v1/sandbox/embeddings/todo` query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingsTodoQuery {
    /// The embedding model (the models route's `embedding.id`): the pieces with no vector of this model.
    pub model: String,
    /// At most this many pieces (1-[`TODO_MAX`], default [`TODO_MAX`]); a message's pieces are never split across
    /// pages, so a page may hold more than `limit` when one message has more pieces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// The pieces of the caller's conversation still without a vector, oldest message first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingsTodo {
    pub items: Vec<EmbeddingPiece>,
}

/// One piece of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingPiece {
    /// The message's event seq.
    pub seq: u64,
    /// The piece's number within the message, from 0.
    pub chunk: u32,
    /// Its text.
    pub text: String,
}

/// `POST /v1/sandbox/embeddings` → 204: vectors of pieces the todo named. A piece whose message is not the
/// caller's conversation's, or with another number of values than the contract's dimensions, is a 400; a piece
/// stored before is kept as it was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingsPut {
    /// The embedding model the vectors are of.
    pub model: String,
    pub items: Vec<EmbeddingVector>,
}

/// One piece's vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingVector {
    pub seq: u64,
    pub chunk: u32,
    pub vector: Vec<f32>,
}

/// `POST /v1/search` (pigeon, for a person): the person's messages that hold `q`'s words, or mean what it means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSearchRequest {
    /// The query as typed (the words half: every message containing it, case-insensitive).
    pub q: String,
    /// The query's vector; absent: the words half only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<QueryEmbedding>,
    /// At most this many hits (1-[`SEARCH_MAX`], default 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// A query's vector and the model it is of (only vectors of the same model are compared).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryEmbedding {
    pub model: String,
    pub vector: Vec<f32>,
}

/// The hits, best first; deleted conversations are left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSearchResponse {
    pub hits: Vec<ConversationSearchHit>,
    /// The meaning half ran (a vector came, and the person has vectors of its model).
    pub semantic: bool,
}

/// One message that matched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSearchHit {
    pub conversation_id: Uuid,
    /// The conversation's title, `None` when untitled.
    pub title: Option<String>,
    /// The timeline's epoch and the message's row as the timeline last wrote it (its `seqEnd`, the cursor the app's
    /// timeline items carry), to open the conversation at it.
    pub epoch: String,
    pub seq: u64,
    /// `user` or `assistant`.
    pub kind: String,
    /// When the message was written.
    pub at: Timestamp,
    /// About [`SNIPPET_CHARS`] characters of the message around what matched.
    pub snippet: String,
    /// `words`, `meaning`, or `both`.
    pub matched: String,
    /// The hit's rank score (reciprocal rank fusion of the two halves; only the order means anything).
    pub score: f64,
}
