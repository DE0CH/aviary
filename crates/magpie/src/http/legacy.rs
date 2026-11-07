//! The legacy-path record of a teed upload ([`crate::legacy::paths`]), until A7.

use magpie_api::legacy::LegacyPathRequest;
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::legacy::paths;

/// `PUT /v1/uploads/{uploadId}/legacy-path`.
pub async fn upload_legacy_path(app: &Magpie, ctx: Ctx, upload: Uuid, body: LegacyPathRequest) -> Answer<()> {
    answer(async { paths::record(&app.pool, &Person::from_ctx(&ctx)?, upload, &body.legacy_path).await }.await)
}
