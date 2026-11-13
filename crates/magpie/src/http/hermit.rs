//! hermit's routes (magpie 16.3).

use magpie_api::Admitted;
use magpie_api::conversations::{Conversation, CreateRequest, CreateResponse};
use magpie_api::ops::{
    HermitCloseRequest, HermitCreateRequest, HermitSendRequest, LegacyImportStatus, PersonPurgeRequest,
    PersonPurgeResponse, WorkspacesExistsRequest, WorkspacesExistsResponse,
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
                model: None,
                thinking: None,
                idempotency_key: body.idempotency_key,
                message: body.message,
            };
            let r = create::create_with_model(&app.pool, &person, req, body.model)
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

/// `GET /v1/hermit/conversations/{id}`: the conversation whoever its person is (404 when there is none).
pub async fn conversation(app: &Magpie, _ctx: Ctx, id: Uuid) -> Answer<Conversation> {
    answer(
        async {
            let conn = app.pool.get().await?;
            match crate::conversations::load(&conn, id).await? {
                Some((_, c)) => Ok(c),
                None => Err(Error::not_found()),
            }
        }
        .await,
    )
}

/// `PUT /v1/hermit/conversations/{id}/closed`.
pub async fn close_conversation(app: &Magpie, _ctx: Ctx, id: Uuid, body: HermitCloseRequest) -> Answer<Conversation> {
    answer(crate::conversations::delete::close(&app.pool, id, body.reason.as_deref()).await)
}

/// `POST /v1/hermit/conversations/{id}/delete`.
pub async fn delete_conversation(app: &Magpie, _ctx: Ctx, id: Uuid) -> Answer<Conversation> {
    answer(crate::conversations::delete::delete_as_deployment(&app.pool, id).await)
}

/// `POST /v1/hermit/conversations/{id}/turns`: M7 as the person hermit names (404 for another's conversation). A
/// turn that runs refuses it (409 `turn_running`) unless the request says `joinRunning`: then the message joins the
/// running turn as a person's does.
pub async fn send_turn(app: &Magpie, _ctx: Ctx, id: Uuid, body: HermitSendRequest) -> Answer<Admitted<SendResponse>> {
    answer(
        async {
            let person = person_of(&body.person)?;
            admission::validate(&body.message)?;
            let running = if body.join_running {
                admission::WhileRunning::JoinAsDeployment
            } else {
                admission::WhileRunning::Refuse
            };
            let sent = admission::send(&app.pool, &person, id, body.message, running)
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
