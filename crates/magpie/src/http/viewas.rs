//! M17.

use magpie_api::ops::ViewAsRequest;
use magpie_api::server::Ctx;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::viewas;

/// M17 `POST /v1/view-as`.
pub async fn start(app: &Magpie, _ctx: Ctx, body: ViewAsRequest) -> Answer<()> {
    answer(async { viewas::start(&app.pool.get().await?, &body).await }.await)
}
