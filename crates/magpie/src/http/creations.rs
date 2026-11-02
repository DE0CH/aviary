//! M4.

use magpie_api::conversations::CreateResponse;
use magpie_api::server::Ctx;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::{Person, create};

/// M4.
pub async fn get(app: &Magpie, ctx: Ctx, key: String) -> Answer<CreateResponse> {
    answer(async { create::creation(&app.pool, &Person::from_ctx(&ctx)?, &key).await }.await)
}
