//! The admin reads (magpie 13, 16.4).

use magpie_api::events::{EventsPage, EventsQuery};
use magpie_api::ops::{AdminConversation, AdminConversationList, AdminListQuery};
use magpie_api::rows::{TimelinePage, TimelineQuery};
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::admin;
use crate::app::Magpie;

/// `GET /v1/admin/conversations`.
pub async fn conversations(app: &Magpie, ctx: Ctx, q: AdminListQuery) -> Answer<AdminConversationList> {
    answer(async { admin::list(&app.pool, &ctx, &q).await }.await)
}

/// `GET /v1/admin/conversations/{id}`.
pub async fn conversation(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<AdminConversation> {
    answer(admin::get(&app.pool, &ctx, id).await)
}

/// `GET /v1/admin/conversations/{id}/events`.
pub async fn events(app: &Magpie, ctx: Ctx, id: Uuid, q: EventsQuery) -> Answer<EventsPage> {
    answer(admin::events(&app.pool, &ctx, id, &q).await)
}

/// `GET /v1/admin/conversations/{id}/timeline`.
pub async fn timeline(app: &Magpie, ctx: Ctx, id: Uuid, q: TimelineQuery) -> Answer<TimelinePage> {
    answer(admin::timeline(&app.pool, &ctx, id, &q).await)
}
