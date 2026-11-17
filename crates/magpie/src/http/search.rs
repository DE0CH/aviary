//! The conversation search (Deyao, 2026-11-17): pigeon's search for a person, and the supervisor's todo and put of its
//! conversation's message vectors.

use magpie_api::search::{
    ConversationSearchRequest, ConversationSearchResponse, EmbeddingsPut, EmbeddingsTodo, EmbeddingsTodoQuery,
};
use magpie_api::server::Ctx;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::search;
use crate::turns::caller::Sandbox;

/// `POST /v1/search`.
pub async fn conversation_search(
    app: &Magpie,
    ctx: Ctx,
    body: ConversationSearchRequest,
) -> Answer<ConversationSearchResponse> {
    answer(async { search::conversation_search(&app.pool, &Person::from_ctx(&ctx)?.email, body).await }.await)
}

/// `GET /v1/sandbox/embeddings/todo`.
pub async fn todo(app: &Magpie, ctx: Ctx, q: EmbeddingsTodoQuery) -> Answer<EmbeddingsTodo> {
    answer(async { search::todo(&app.pool, &Sandbox::from_ctx(&ctx)?, q).await }.await)
}

/// `POST /v1/sandbox/embeddings`.
pub async fn put(app: &Magpie, ctx: Ctx, body: EmbeddingsPut) -> Answer<()> {
    answer(async { search::put(&app.pool, &Sandbox::from_ctx(&ctx)?, body).await }.await)
}
