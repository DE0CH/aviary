//! The files panel and M14.

use axum::response::Response;
use magpie_api::files::{FilesPanel, RawFileQuery};
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::files;

/// `GET /v1/conversations/{id}/files`.
pub async fn panel(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<FilesPanel> {
    answer(async { files::panel(&app.pool.get().await?, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// M14 `GET /v1/conversations/{id}/files/raw`.
pub async fn raw(app: &Magpie, ctx: Ctx, id: Uuid, q: RawFileQuery) -> Answer<Response> {
    answer(async { files::files_raw(app, &ctx, &Person::from_ctx(&ctx)?, id, &q).await }.await)
}
