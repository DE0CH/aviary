//! The supervisor's turn routes (magpie 16.2): the command poll and acks, turn state, agent-started turns, task
//! events, asks, the heartbeat.

use magpie_api::sandbox::{
    AckRequest, AgentTurnRequest, AskReport, AskResolvedReport, CommandsQuery, CommandsResponse, Heartbeat,
    PersonWaitEnd, PersonWaitOpen, PersonWaitOpened, QuestionCreated, QuestionQuery, QuestionRequest, QuestionState,
    TasksReport, TurnStateReport,
};
use magpie_api::server::Ctx;
use magpie_api::{Admitted, Empty};
use uuid::Uuid;

use super::{Answer, answer};
use crate::app::Magpie;
use crate::turns::caller::Sandbox;
use crate::turns::{commands, reports};

/// `GET /v1/sandbox/commands`.
pub async fn commands(app: &Magpie, ctx: Ctx, q: CommandsQuery) -> Answer<CommandsResponse> {
    answer(async { commands::poll(app.read(), &Sandbox::from_ctx(&ctx)?, q).await }.await)
}

/// `POST /v1/sandbox/commands/{cmdSeq}/ack`.
pub async fn ack(app: &Magpie, ctx: Ctx, cmd_seq: u64, body: AckRequest) -> Answer<()> {
    answer(async { commands::ack(&app.pool, &Sandbox::from_ctx(&ctx)?, cmd_seq, body).await }.await)
}

/// `POST /v1/sandbox/turns`.
pub async fn agent_turn(app: &Magpie, ctx: Ctx, body: AgentTurnRequest) -> Answer<Admitted<Empty>> {
    answer(async { reports::agent_turn(&app.pool, &Sandbox::from_ctx(&ctx)?, body).await }.await)
}

/// `POST /v1/sandbox/turns/{turnId}/state`.
pub async fn turn_state(app: &Magpie, ctx: Ctx, turn_id: Uuid, mut body: TurnStateReport) -> Answer<()> {
    answer(
        async {
            if let Some(p) = &body.transcript_path {
                body.transcript_path = Some(crate::ccfs::homes::stored(&ctx, p)?);
            }
            reports::turn_state(&app.pool, &Sandbox::from_ctx(&ctx)?, turn_id, body).await
        }
        .await,
    )
}

/// `POST /v1/sandbox/tasks`.
pub async fn tasks(app: &Magpie, ctx: Ctx, body: TasksReport) -> Answer<()> {
    answer(async { reports::tasks(&app.pool, &Sandbox::from_ctx(&ctx)?, body).await }.await)
}

/// `POST /v1/sandbox/asks`.
pub async fn ask_open(app: &Magpie, ctx: Ctx, body: AskReport) -> Answer<()> {
    answer(async { reports::ask_open(&app.pool, &Sandbox::from_ctx(&ctx)?, body).await }.await)
}

/// `POST /v1/sandbox/questions`.
pub async fn question_open(app: &Magpie, ctx: Ctx, body: QuestionRequest) -> Answer<QuestionCreated> {
    answer(async { reports::question_open(&app.pool, &Sandbox::from_ctx(&ctx)?, body).await }.await)
}

/// `GET /v1/sandbox/questions/{askId}`.
pub async fn question_get(app: &Magpie, ctx: Ctx, ask_id: String, q: QuestionQuery) -> Answer<QuestionState> {
    answer(async { reports::question_get(&app.pool, &Sandbox::from_ctx(&ctx)?, &ask_id, q).await }.await)
}

/// `POST /v1/sandbox/person-waits/{key}`.
pub async fn person_wait_open(app: &Magpie, ctx: Ctx, key: String, body: PersonWaitOpen) -> Answer<PersonWaitOpened> {
    answer(async { reports::person_wait_open(&app.pool, &Sandbox::from_ctx(&ctx)?, &key, body).await }.await)
}

/// `POST /v1/sandbox/person-waits/{key}/ended`.
pub async fn person_wait_ended(app: &Magpie, ctx: Ctx, key: String, body: PersonWaitEnd) -> Answer<()> {
    answer(async { reports::person_wait_ended(&app.pool, &Sandbox::from_ctx(&ctx)?, &key, body).await }.await)
}

/// `POST /v1/sandbox/asks/{askId}/resolved`.
pub async fn ask_resolved(app: &Magpie, ctx: Ctx, ask_id: String, body: AskResolvedReport) -> Answer<()> {
    answer(async { reports::ask_resolved(&app.pool, &Sandbox::from_ctx(&ctx)?, &ask_id, body).await }.await)
}

/// `POST /v1/sandbox/heartbeat`.
pub async fn heartbeat(app: &Magpie, ctx: Ctx, body: Heartbeat) -> Answer<()> {
    answer(async { reports::heartbeat(&app.pool, &Sandbox::from_ctx(&ctx)?, body).await }.await)
}
