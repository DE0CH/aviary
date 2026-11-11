//! hermit's routes (magpie 16.3).

use magpie_api::Admitted;
use magpie_api::conversations::{CreateRequest, CreateResponse};
use magpie_api::ops::{
    HermitCreateRequest, HermitSendRequest, LegacyImportStatus, PersonPurgeRequest, PersonPurgeResponse,
    WorkspacesExistsRequest, WorkspacesExistsResponse,
};
use magpie_api::server::Ctx;
use magpie_api::turns::SendResponse;
use uuid::Uuid;

use super::{Answer, answer};
use crate::admin;
use crate::app::Magpie;
use crate::conversations::{Person, create};
use crate::error::{Error, Result};
use crate::turns::admission;
use crate::turns::driver::{self, Driver};

/// The person hermit names, `lower(btrim())` as everywhere (no viewer: the deployment acts, not an admin).
fn person_of(email: &str) -> Result<Person> {
    let email = email.trim().to_lowercase();
    if email.find('@').is_none_or(|i| i < 1) {
        return Err(Error::bad_request("person is not an email"));
    }
    Ok(Person { email, viewer: None })
}

/// `POST /v1/hermit/conversations`: M3 for the person hermit names; the first turn's driver starts on this replica.
pub async fn create_conversation(
    app: &Magpie,
    _ctx: Ctx,
    body: HermitCreateRequest,
) -> Answer<Admitted<CreateResponse>> {
    answer(
        async {
            let person = person_of(&body.person)?;
            let req = CreateRequest {
                idempotency_key: body.idempotency_key,
                message: body.message,
            };
            let r = create::create(&app.pool, &person, req)
                .await
                .inspect_err(|e| super::turns::unknown_upload(app, e))?;
            if let Admitted::New(c) = &r
                && let Some(turn) = &c.turn
            {
                driver::spawn(Driver::of(app), turn.id);
            }
            Ok(r)
        }
        .await,
    )
}

/// `POST /v1/hermit/conversations/{id}/turns`: M7 as the person hermit names (404 for another's conversation).
pub async fn send_turn(app: &Magpie, _ctx: Ctx, id: Uuid, body: HermitSendRequest) -> Answer<Admitted<SendResponse>> {
    answer(
        async {
            let person = person_of(&body.person)?;
            admission::validate(&body.message)?;
            let sent = admission::send(&app.pool, &person, id, body.message)
                .await
                .inspect_err(|e| super::turns::unknown_upload(app, e))?;
            if let Some(turn) = sent.new_turn() {
                driver::spawn(Driver::of(app), turn);
            }
            Ok(sent.answer)
        }
        .await,
    )
}

/// `POST /v1/hermit/persons/{person}/purge`.
pub async fn person_purge(
    app: &Magpie,
    _ctx: Ctx,
    person: String,
    _body: PersonPurgeRequest,
) -> Answer<PersonPurgeResponse> {
    answer(admin::purge_person(app, &person).await)
}

/// `POST /v1/hermit/workspaces/exists`.
pub async fn workspaces_exists(
    app: &Magpie,
    _ctx: Ctx,
    body: WorkspacesExistsRequest,
) -> Answer<WorkspacesExistsResponse> {
    answer(async { admin::workspaces_exist(&app.pool.get().await?, &body).await }.await)
}

/// `GET /v1/hermit/legacy-import`.
pub async fn legacy_import(app: &Magpie, _ctx: Ctx) -> Answer<LegacyImportStatus> {
    answer(async { admin::legacy_import(&app.pool.get().await?).await }.await)
}
