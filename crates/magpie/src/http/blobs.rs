//! M15 and the sandbox's blob routes (magpie 5.4, 11.2).

use axum::body::Body;
use axum::response::{IntoResponse as _, Response};
use magpie_api::server::Ctx;
use magpie_api::uploads::{BlobQuery, SandboxBlobQuery};
use magpie_api::{Admitted, Empty};

use super::{Answer, answer};
use crate::app::Magpie;
use crate::files;

/// M15 `GET /v1/blobs/{blobId}`.
pub async fn blob(app: &Magpie, ctx: Ctx, blob_id: String, q: BlobQuery) -> Answer<Response> {
    answer(async { files::serve_blob(app, &blob_id, q.variant.unwrap_or_default(), &ctx.headers).await }.await)
}

/// `GET /v1/sandbox/blobs/{sha}`.
pub async fn sandbox_get(app: &Magpie, ctx: Ctx, sha: String, q: SandboxBlobQuery) -> Answer<Response> {
    answer(async { files::serve_sandbox_blob(app, &ctx, &sha, q.as_.unwrap_or_default()).await }.await)
}

/// `GET /v1/sandbox/earlier` (lease): the conversation's rows between two event seqs as text, for a harness that
/// has not seen them (`start_turn.carryOver`).
pub async fn earlier(app: &Magpie, ctx: Ctx, q: magpie_api::sandbox::EarlierQuery) -> Answer<Response> {
    answer(
        async {
            let db = app.pool.get().await?;
            let conv = crate::sandbox::conversation(&db, &ctx, true).await?;
            let text = crate::turns::carry::text(
                &db,
                conv.id,
                i64::try_from(q.after).unwrap_or(i64::MAX),
                i64::try_from(q.before).unwrap_or(i64::MAX),
            )
            .await?;
            Ok::<_, crate::error::Error>(
                (
                    [(axum::http::header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
                    text,
                )
                    .into_response(),
            )
        }
        .await,
    )
}

/// `PUT /v1/sandbox/blobs/{sha}`.
pub async fn sandbox_put(app: &Magpie, ctx: Ctx, sha: String, body: Body) -> Answer<Admitted<Empty>> {
    answer(files::put_sandbox_blob(app, &ctx, &sha, body).await)
}
