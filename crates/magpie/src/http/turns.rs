//! M7, M8, M11.

use magpie_api::Admitted;
use magpie_api::server::Ctx;
use magpie_api::turns::{AgentPermissionResponse, CancelResponse, SendRequest, SendResponse};
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::legacy;
use crate::turns::driver::{self, Driver};
use crate::turns::{admission, cancel};

/// M7: admits the turn and starts its driver on this replica.
pub async fn send(app: &Magpie, ctx: Ctx, id: Uuid, body: SendRequest) -> Answer<Admitted<SendResponse>> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            admission::validate(&body)?;
            // old asset paths (the legacy-path set, until A7): resolved to fresh copies before the transaction, but
            // not for a repeat, which admits nothing; their `unknown_upload` is counted where they resolve
            let mut req = body;
            if legacy::paths::has_legacy_path(&person, &req)
                && !admission::is_repeat(&app.pool, id, &req.client_message_id).await?
            {
                legacy::paths::resolve(app, &person, &mut req).await?;
            }
            let sent = admission::send(&app.pool, &person, id, req)
                .await
                .inspect_err(|e| unknown_upload(app, e))?;
            if let Some(turn) = sent.new_turn() {
                driver::spawn(Driver::of(app), turn);
            }
            Ok(sent.answer)
        }
        .await,
    )
}

/// Counts a refused send's `unknown_upload` that no old asset path caused (those are counted where they resolve).
pub(crate) fn unknown_upload(app: &Magpie, e: &crate::error::Error) {
    if e.is(&magpie_api::errors::ErrorCode::UnknownUpload) {
        app.legacy.unknown_upload(false);
    }
}

/// M8.
pub async fn cancel(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<CancelResponse> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            cancel::cancel(&app.pool, &person, id).await
        }
        .await,
    )
}

/// M11.
pub async fn answer_ask(app: &Magpie, ctx: Ctx, id: Uuid, ask_id: String, body: AgentPermissionResponse) -> Answer<()> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            let resumed = cancel::answer(&app.pool, &person, id, &ask_id, body).await?;
            // a parked turn's last answer admitted it again (DECISIONS 17): its driver gets it a sandbox
            if let Some(turn) = resumed {
                driver::spawn(Driver::of(app), turn);
            }
            Ok(())
        }
        .await,
    )
}
