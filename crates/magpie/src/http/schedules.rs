//! The schedules routes ([`crate::schedules`]): the AI's (through the gateway), the app's panel, the admin's.

use magpie_api::schedules::{AdminSchedules, CreateSchedule, PauseAll, Schedule, SchedulesList, UpdateSchedule};
use magpie_api::server::Ctx;
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::schedules;

/// `GET /v1/sandbox/schedules`.
pub async fn list(app: &Magpie, ctx: Ctx) -> Answer<SchedulesList> {
    answer(schedules::list(&app.pool, &ctx).await)
}

/// `POST /v1/sandbox/schedules`.
pub async fn create(app: &Magpie, ctx: Ctx, body: &CreateSchedule) -> Answer<Schedule> {
    answer(schedules::create(&app.pool, &ctx, body).await)
}

/// `GET /v1/sandbox/schedules/{scheduleId}`.
pub async fn get(app: &Magpie, ctx: Ctx, id: &str) -> Answer<Schedule> {
    answer(schedules::get(&app.pool, &ctx, id).await)
}

/// `PATCH /v1/sandbox/schedules/{scheduleId}`.
pub async fn update(app: &Magpie, ctx: Ctx, id: &str, body: &UpdateSchedule) -> Answer<Schedule> {
    answer(schedules::update(&app.pool, &ctx, id, body).await)
}

/// `DELETE /v1/sandbox/schedules/{scheduleId}`.
pub async fn delete(app: &Magpie, ctx: Ctx, id: &str) -> Answer<()> {
    answer(schedules::delete(&app.pool, &ctx, id).await)
}

/// `GET /v1/conversations/{id}/schedules`.
pub async fn of_conversation(app: &Magpie, ctx: Ctx, id: Uuid) -> Answer<SchedulesList> {
    answer(async { schedules::of_conversation(&app.pool, &Person::from_ctx(&ctx)?, id).await }.await)
}

/// `GET /v1/admin/schedules`.
pub async fn admin(app: &Magpie, ctx: Ctx) -> Answer<AdminSchedules> {
    answer(schedules::admin_list(&app.pool, &ctx).await)
}

/// `PUT /v1/admin/schedules/pause`.
pub async fn admin_pause(app: &Magpie, ctx: Ctx, body: &PauseAll) -> Answer<AdminSchedules> {
    answer(schedules::admin_pause(&app.pool, &ctx, body).await)
}
