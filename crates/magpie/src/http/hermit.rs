//! hermit's routes (magpie 16.3).

use magpie_api::ops::{
    LegacyImportStatus, PersonPurgeRequest, PersonPurgeResponse, WorkspacesExistsRequest, WorkspacesExistsResponse,
};
use magpie_api::server::Ctx;

use super::{Answer, answer};
use crate::admin;
use crate::app::Magpie;

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
