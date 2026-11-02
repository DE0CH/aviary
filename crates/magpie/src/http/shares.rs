//! M16.

use axum::response::Response;
use magpie_api::files::RawFileQuery;
use magpie_api::server::Ctx;
use magpie_api::shares::{ContinueResponse, ShareRef, ShareView, SharesList};
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::shares;

/// `GET /v1/conversations/{id}/shares`.
pub async fn list(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<SharesList> {
    answer(async { shares::list(&app.pool.get().await?, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// `POST /v1/conversations/{id}/shares`.
pub async fn create(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<ShareRef> {
    answer(async { shares::create(app, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// `DELETE /v1/shares/{shareId}`.
pub async fn revoke(app: &Magpie, ctx: Ctx, share_id: String) -> Answer<()> {
    answer(async { shares::revoke(app, &Person::from_ctx(&ctx)?, &share_id).await }.await)
}

/// `GET /v1/shares/{shareId}`.
pub async fn view(app: &Magpie, ctx: Ctx, share_id: String) -> Answer<ShareView> {
    answer(async { shares::view(&app.pool.get().await?, &Person::from_ctx(&ctx)?, &share_id).await }.await)
}

/// `GET /v1/shares/{shareId}/files/raw`.
pub async fn files_raw(app: &Magpie, ctx: Ctx, share_id: String, q: RawFileQuery) -> Answer<Response> {
    answer(async { shares::files_raw(app, &ctx, &share_id, &q).await }.await)
}

/// `POST /v1/shares/{shareId}/continue`.
pub async fn continue_share(app: &Magpie, ctx: Ctx, share_id: String) -> Answer<ContinueResponse> {
    answer(async { shares::continue_share(app, &Person::from_ctx(&ctx)?, &share_id).await }.await)
}
