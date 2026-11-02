//! M7, M8, M11.

use magpie_api::Admitted;
use magpie_api::server::Ctx;
use magpie_api::turns::{AgentPermissionResponse, CancelResponse, SendRequest, SendResponse};
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::turns::driver::{self, Driver};
use crate::turns::{admission, cancel};

/// M7: admits the turn and starts its driver on this replica.
pub async fn send(app: &Magpie, ctx: Ctx, id: Uuid, body: SendRequest) -> Answer<Admitted<SendResponse>> {
    answer(
        async {
            let sent = admission::send(&app.pool, &Person::from_ctx(&ctx)?, id, body).await?;
            if let Some(turn) = sent.new_turn() {
                driver::spawn(Driver::of(app), turn);
            }
            Ok(sent.answer)
        }
        .await,
    )
}

/// M8.
pub async fn cancel(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<CancelResponse> {
    answer(async { cancel::cancel(&app.pool, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// M11.
pub async fn answer_ask(app: &Magpie, ctx: Ctx, id: Uuid, ask_id: String, body: AgentPermissionResponse) -> Answer<()> {
    answer(async { cancel::answer(&app.pool, &Person::from_ctx(&ctx)?, id, &ask_id, body).await }.await)
}
