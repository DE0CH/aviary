//! The conversation search's vectors (Deyao, 2026-11-17), made here in the conversation's own sandbox: after each turn's
//! end has reached magpie, and once at a start, the embedder asks magpie for the message pieces that have no vector
//! of the deployment's embedding model yet (the models route's `embedding`; none: nothing to do), has the model
//! route turn them into vectors through the gateway, and stores them. One catch-up runs at a time; a poke during one
//! makes it look again when it ends. A failure (the model route refuses, magpie is away) ends the catch-up and is
//! logged: the next turn's end tries again, so nothing is lost but time.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use magpie_api::search::{EmbeddingVector, EmbeddingsPut, EmbeddingsTodoQuery};

use crate::cc::report::Reporter;
use crate::limits::{EMBED_BATCH, EMBED_ROUNDS};
use crate::model::Model;

/// One catch-up at a time.
#[derive(Debug, Default)]
pub struct Embedder {
    running: AtomicBool,
    again: AtomicBool,
}

impl Embedder {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Starts a catch-up after `delay` unless one runs (that one looks again when it ends).
    pub fn poke(self: &Arc<Self>, reporter: Arc<Reporter>, model: Model, delay: Duration) {
        if self.running.swap(true, Ordering::AcqRel) {
            self.again.store(true, Ordering::Release);
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            loop {
                me.again.store(false, Ordering::Release);
                match catch_up(&reporter, &model).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(pieces = n, "conversation search: vectors stored"),
                    Err(e) => tracing::warn!(error = %e, "conversation search: embedding stopped"),
                }
                if me.again.load(Ordering::Acquire) {
                    continue;
                }
                me.running.store(false, Ordering::Release);
                // a poke between the look and the store found it running and started nothing: this one goes on
                if me.again.load(Ordering::Acquire) && !me.running.swap(true, Ordering::AcqRel) {
                    continue;
                }
                break;
            }
        });
    }
}

/// Embeds what magpie says is missing, up to [`EMBED_ROUNDS`] calls; the pieces stored.
///
/// # Errors
/// The models route's, the embeddings call's or magpie's failure, as text.
pub async fn catch_up(reporter: &Reporter, model: &Model) -> Result<usize, String> {
    let Some(embedding) = model.models().await?.embedding else {
        return Ok(0);
    };
    let mut stored = 0;
    for _ in 0..EMBED_ROUNDS {
        let todo = reporter
            .calls()
            .embeddings_todo(&EmbeddingsTodoQuery {
                model: embedding.id.clone(),
                limit: Some(EMBED_BATCH),
            })
            .await
            .map_err(|e| format!("todo: {e}"))?;
        if todo.items.is_empty() {
            break;
        }
        let texts: Vec<String> = todo.items.iter().map(|i| i.text.clone()).collect();
        let vectors = model.embed(&embedding.id, &texts).await?;
        let items: Vec<EmbeddingVector> = todo
            .items
            .iter()
            .zip(vectors)
            .map(|(i, vector)| EmbeddingVector {
                seq: i.seq,
                chunk: i.chunk,
                vector,
            })
            .collect();
        let n = items.len();
        reporter
            .calls()
            .embeddings_put(&EmbeddingsPut {
                model: embedding.id.clone(),
                items,
            })
            .await
            .map_err(|e| format!("put: {e}"))?;
        stored += n;
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    /// The supervisor asks the model for as many numbers as magpie's column has.
    #[test]
    fn the_gateway_and_magpie_agree_on_the_dimensions() {
        assert_eq!(
            gateway_api::models::EMBEDDING_DIMENSIONS,
            magpie_api::search::DIMENSIONS
        );
    }
}
