//! The ops reads (16.4), callers hermit and admin.

use magpie_api::ops::{OpsStats, PeopleQuery, PeopleResponse, ViewAsLog, ViewAsLogQuery};
use magpie_api::server::Ctx;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::stats;

/// `GET /v1/ops/people`.
pub async fn people(app: &Magpie, _ctx: Ctx, q: PeopleQuery) -> Answer<PeopleResponse> {
    answer(stats::people(&app.pool, q.tz.as_deref()).await)
}

/// `GET /v1/ops/view-as`.
pub async fn view_as(app: &Magpie, _ctx: Ctx, q: ViewAsLogQuery) -> Answer<ViewAsLog> {
    answer(stats::view_as(&app.pool, q.limit, q.before).await)
}

/// `GET /v1/ops/stats`.
pub async fn stats(app: &Magpie, _ctx: Ctx) -> Answer<OpsStats> {
    answer(stats::stats(&app.pool).await)
}
