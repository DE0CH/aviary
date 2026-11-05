//! M7, M8, M11.

use std::sync::atomic::{AtomicBool, Ordering};

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
            // the switch window's old asset paths (until A7): resolved to fresh copies before each attempt's
            // transaction, but not for a repeat, which admits nothing, nor while the old stack owns the conversation
            // unfenced (its 409 `route: "legacy"` comes back at once, with no ingest and no copies). The owner is
            // read again on the attempt after a fence, so a send that found the conversation legacy and was then
            // fenced is resolved before it becomes native; an attempt that resolved keeps its copies for the next.
            let has_old = legacy::paths::has_legacy_path(&person, &body);
            let resolved: tokio::sync::Mutex<Option<SendRequest>> = tokio::sync::Mutex::new(None);
            let old_refused = AtomicBool::new(false);
            let (person, body, resolved, old_refused) = (&person, &body, &resolved, &old_refused);
            let sent = legacy::fence::native(app, id, || async move {
                let mut done = resolved.lock().await;
                let req = match &*done {
                    Some(b) => b.clone(),
                    None if has_old
                        && legacy::fence::may_go_native(&app.pool.get().await?, &person.email, id).await?
                        && !admission::is_repeat(&app.pool, id, &body.client_message_id).await? =>
                    {
                        let mut b = body.clone();
                        legacy::paths::resolve(app, person, &mut b)
                            .await
                            .inspect_err(|_| old_refused.store(true, Ordering::Relaxed))?;
                        *done = Some(b.clone());
                        b
                    }
                    None => body.clone(),
                };
                drop(done);
                admission::send(&app.pool, person, id, req).await
            })
            .await
            .inspect_err(|e| {
                if !old_refused.load(Ordering::Relaxed) {
                    unknown_upload(app, e);
                }
            })?;
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
            legacy::fence::native(app, id, || cancel::cancel(&app.pool, &person, id)).await
        }
        .await,
    )
}

/// M11.
pub async fn answer_ask(app: &Magpie, ctx: Ctx, id: Uuid, ask_id: String, body: AgentPermissionResponse) -> Answer<()> {
    answer(
        async {
            let person = Person::from_ctx(&ctx)?;
            let (ask_id, body) = (&ask_id, &body);
            let resumed =
                legacy::fence::native(app, id, || cancel::answer(&app.pool, &person, id, ask_id, body.clone())).await?;
            // a parked turn's last answer admitted it again (DECISIONS 17): its driver gets it a sandbox
            if let Some(turn) = resumed {
                driver::spawn(Driver::of(app), turn);
            }
            Ok(())
        }
        .await,
    )
}
