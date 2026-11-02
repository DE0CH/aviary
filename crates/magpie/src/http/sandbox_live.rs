//! `POST /v1/sandbox/turns/{turnId}/live` (magpie 7.5), caller the gateway.

use magpie_api::live::LiveFramesRequest;
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::live;

/// Live frames of a running turn.
pub async fn frames(app: &Magpie, ctx: Ctx, turn: Uuid, body: LiveFramesRequest) -> Answer<()> {
    answer(live::receive(&app.pool, &ctx, turn, &body.frames).await)
}
