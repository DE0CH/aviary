//! M13.

use axum::body::Body;
use magpie_api::server::Ctx;
use magpie_api::uploads::{UploadMeta, UploadResponse};

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::uploads;

/// M13 `POST /v1/uploads`.
pub async fn upload(app: &Magpie, ctx: Ctx, meta: UploadMeta, body: Body) -> Answer<UploadResponse> {
    answer(async { uploads::upload(app, &Person::from_ctx(&ctx)?, meta, body).await }.await)
}
