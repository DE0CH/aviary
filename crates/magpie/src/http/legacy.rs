//! The switch window's routes ([`crate::legacy`]): tickets until A6, the legacy-path record until A7.

use axum::response::Response;
use magpie_api::legacy::{LegacyPathRequest, TicketDone, TicketRequest, TicketResponse, WindowState};
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::legacy::{paths, tickets, window};

/// `POST /v1/conversations/{id}/legacy/tickets`.
pub async fn ticket(app: &Magpie, ctx: Ctx, id: Uuid, body: TicketRequest) -> Answer<TicketResponse> {
    answer(async { tickets::take(app, &Person::from_ctx(&ctx)?, id, &body).await }.await)
}

/// `POST /v1/legacy/tickets/{ticket}/done`.
pub async fn ticket_done(app: &Magpie, ctx: Ctx, ticket: Uuid, body: TicketDone) -> Answer<()> {
    answer(async { tickets::done(app, &Person::from_ctx(&ctx)?, ticket, &body).await }.await)
}

/// `PUT /v1/uploads/{uploadId}/legacy-path`.
pub async fn upload_legacy_path(app: &Magpie, ctx: Ctx, upload: Uuid, body: LegacyPathRequest) -> Answer<()> {
    answer(async { paths::record(&app.pool, &Person::from_ctx(&ctx)?, upload, &body.legacy_path).await }.await)
}

/// `GET /v1/legacy/window`.
pub async fn window(app: &Magpie, ctx: Ctx) -> Answer<WindowState> {
    answer(async { window::state(app, &Person::from_ctx(&ctx)?).await }.await)
}

/// `GET /v1/uploads/{uploadId}/raw`.
pub async fn upload_raw(app: &Magpie, ctx: Ctx, upload: Uuid) -> Answer<Response> {
    answer(async { window::upload_raw(app, &Person::from_ctx(&ctx)?, upload).await }.await)
}
