//! M15 and the sandbox's blob routes (magpie 5.4, 11.2).

use axum::body::Body;
use axum::response::Response;
use magpie_api::server::Ctx;
use magpie_api::uploads::{BlobQuery, SandboxBlobQuery};
use magpie_api::{Admitted, Empty};

use super::{Answer, answer};
use crate::app::Magpie;
use crate::blobs::read::respond;
use crate::files;

/// M15 `GET /v1/blobs/{blobId}`.
pub async fn blob(app: &Magpie, ctx: Ctx, blob_id: String, q: BlobQuery) -> Answer<Response> {
    answer(
        async {
            let (sha, served) = files::serve_blob(app, &blob_id, q.variant.unwrap_or_default()).await?;
            Ok(respond(&sha, served, &ctx.headers))
        }
        .await,
    )
}

/// `GET /v1/sandbox/blobs/{sha}`.
pub async fn sandbox_get(app: &Magpie, ctx: Ctx, sha: String, q: SandboxBlobQuery) -> Answer<Response> {
    answer(
        async {
            let (sha, served) = files::serve_sandbox_blob(app, &ctx, &sha, q.as_.unwrap_or_default()).await?;
            Ok(respond(&sha, served, &ctx.headers))
        }
        .await,
    )
}

/// `PUT /v1/sandbox/blobs/{sha}`.
pub async fn sandbox_put(app: &Magpie, ctx: Ctx, sha: String, body: Body) -> Answer<Admitted<Empty>> {
    answer(files::put_sandbox_blob(app, &ctx, &sha, body).await)
}
