//! The supervisor's reports (magpie 8.5), each in one transaction under the conversation row lock, each carrying the
//! current lease: the turn's state, a turn Claude Code started by itself, background task events, asks, and the
//! heartbeat (with its reconciliation, the backstop for a report lost against all odds).
//!
//! The supervisor may report `running` before hermit's ready reached the driver (it takes `start_turn` as soon as it
//! boots): that report is the turn's ready too. For the same reason an end report is accepted from `admitted`,
//! `preparing` and `starting` once `start_turn` was delivered (a supervisor that fails a start before running, such
//! as its "Claude Code would not start" answer, supervisor 7.9); before the delivery no supervisor can know the turn
//! and the end is 409 `bad_transition`. A replay of an end already recorded is answered 204; any other transition
//! magpie 8.1 does not allow is 409 `bad_transition` with the current state.
//!
//! The heartbeat reconciles a `starting` turn whose `start_turn` was acknowledged as it does a running one (its
//! `running` report may be the one lost): `turnId == T` is the running report, `lastTurn.id == T` its end, `idle`
//! without the turn `run.error`.

use aviary_pg::{Pool, Tx};
use magpie_api::changes::{AskResolvedChange, ChangeBody};
use magpie_api::conversations::{Ask, BackgroundTask, SandboxPhase};
use magpie_api::errors::{DataCode, ErrorCode, ErrorInfo, Refusal};
use magpie_api::events::{AskPending, AskResolved, EventBody, TaskState, TaskStatus, TurnStartedData};
use magpie_api::sandbox::{
    AgentTurnRequest, AskOutcome, AskReport, AskResolvedReport, Heartbeat, HeartbeatState, ReportedState, TasksReport,
    TurnStateReport,
};
use magpie_api::turns::{AgentPermissionResponse, TurnOrigin, TurnOutcome, TurnPhase};
use magpie_api::{Admitted, Empty, Timestamp};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use super::caller::{self, Locked, Sandbox};
use super::end::{self, Ending};
use super::{State, Turn, active, load, push_running_deadline, start_turn_status, turn_change};
use crate::changes;
use crate::error::{Error, Result};
use crate::events::{self, NewEvent};

fn bad_transition(state: State) -> Error {
    Refusal::bad_transition(state.as_str()).into()
}

/// The turn is now running: its ready (when hermit's had not reached the driver), `started_at`, Claude Code's
/// session and main transcript path, the running deadline.
async fn running(tx: &Tx<'_>, c: &Locked, caller: &Sandbox, turn: &Turn, r: &TurnStateReport) -> Result<()> {
    let row = tx
        .query_one("select sandbox_phase from conversations where id = $1", &[&c.id])
        .await?;
    let preparing = row.get::<_, Option<String>>(0).as_deref() == Some("preparing");
    if preparing {
        events::append(
            tx,
            c.id,
            NewEvent::of(&EventBody::SandboxReady(Empty {}))?.turn(Some(turn.id)),
        )
        .await?;
        changes::emit(
            tx,
            &c.person,
            c.id,
            &ChangeBody::Sandbox {
                state: SandboxPhase::Ready,
            },
        )
        .await?;
    }
    tx.execute(
        "update turns set state = 'running', started_at = coalesce(started_at, $2), ready_at = coalesce(ready_at, now()),
                sandbox_id = coalesce(sandbox_id, $3), driver = null, driver_until = null
         where id = $1",
        &[&turn.id, &r.at.0, &caller.sandbox],
    )
    .await?;
    push_running_deadline(tx, turn.id).await?;
    tx.execute(
        "update conversations set run_state = 'running',
                sandbox_phase = case when sandbox_phase = 'preparing' then 'ready' else sandbox_phase end
         where id = $1",
        &[&c.id],
    )
    .await?;
    session_facts(tx, c.id, r).await?;
    changes::upserted(tx, &c.person, c.id).await?;
    Ok(())
}

/// A report's Claude Code session and main transcript path, when it names them (idempotent): the `running` report,
/// and also a late `running` after the heartbeat reconciled the turn running without them, and an end report.
/// Setting the path promotes the lines Claude Code wrote there before (magpie 6.2).
async fn session_facts(tx: &Tx<'_>, conversation: Uuid, r: &TurnStateReport) -> Result<()> {
    if r.provider_session_id.is_some() {
        tx.execute(
            "update conversations set provider_session_id = $2
             where id = $1 and provider_session_id is distinct from $2",
            &[&conversation, &r.provider_session_id],
        )
        .await?;
    }
    if let Some(path) = &r.transcript_path {
        crate::ccfs::lines::set_main_transcript(tx, conversation, path).await?;
    }
    Ok(())
}

/// Whether the transcript line with `uuid` is stored: as an event of the conversation, or as a line of its main
/// transcript (an internal line makes no event; the turn's last line is near the end of the file).
async fn line_stored(tx: &Tx<'_>, conversation: Uuid, path: Option<&str>, uuid: &str) -> Result<bool> {
    let as_event = tx
        .query_opt(
            "select 1 from events where conversation_id = $1 and kind like 'cc.%' and data->>'uuid' = $2 limit 1",
            &[&conversation, &uuid],
        )
        .await?;
    if as_event.is_some() {
        return Ok(true);
    }
    let needle = format!("\"uuid\":\"{uuid}\"");
    let as_line = tx
        .query_opt(
            "select 1 from (
               select l.stub from cc_lines l join cc_files f on f.id = l.file_id
               where f.scope = $1
                 and f.path = coalesce($2, (select main_transcript from conversations where id = $1))
               order by l.ord desc limit 2000) recent
             where position(convert_to($3, 'UTF8') in recent.stub) > 0
             limit 1",
            &[&conversation, &path, &needle],
        )
        .await?;
    Ok(as_line.is_some())
}

fn tasks_of(r: &TurnStateReport) -> Option<Vec<BackgroundTask>> {
    if r.held != Some(true) {
        return None;
    }
    Some(
        r.tasks
            .iter()
            .flatten()
            .map(|t| BackgroundTask {
                task_id: t.task_id.clone(),
                tool_use_id: t.tool_use_id.clone(),
                kind: t.kind.clone(),
                description: t.description.clone(),
                started_at: t.started_at,
            })
            .collect(),
    )
}

/// `POST /v1/sandbox/turns/{turnId}/state`.
///
/// # Errors
/// 404, 409 `bad_transition` / `transcript_pending` / `lease_lost` / `conversation_migrating`; a statement error.
pub async fn turn_state(pool: &Pool, caller: &Sandbox, turn_id: Uuid, r: TurnStateReport) -> Result<()> {
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = caller::lock(t, caller).await?;
        let turn = load(t, c.id, turn_id).await?.ok_or_else(Error::not_found)?;
        if r.state == ReportedState::Parked {
            // the sandbox goes while the turn waits only on its questions (DECISIONS 17)
            return super::parked::park(t, &c.person, &turn, &r).await;
        }
        let outcome = match r.state {
            ReportedState::Running | ReportedState::Parked => None,
            ReportedState::Completed => Some(TurnOutcome::Completed),
            ReportedState::Failed => Some(TurnOutcome::Failed),
            ReportedState::Canceled => Some(TurnOutcome::Canceled),
        };
        if turn.state.is_ended() {
            let same = match outcome {
                Some(TurnOutcome::Completed) => turn.state == State::Completed,
                Some(TurnOutcome::Failed) => turn.state == State::Failed,
                Some(TurnOutcome::Canceled) => turn.state == State::Canceled,
                None => false,
            };
            return if same { Ok(()) } else { Err(bad_transition(turn.state)) };
        }
        let Some(outcome) = outcome else {
            return match turn.state {
                // a replay, or the real report after the heartbeat reconciled the turn running: its facts still count
                State::Running | State::Stopping => session_facts(t, c.id, &r).await,
                _ => running(t, &c, caller, &turn, &r).await,
            };
        };
        if turn.state.is_waiting() || turn.state == State::Starting {
            let (delivered, _) = start_turn_status(t, &turn).await?;
            if !delivered {
                return Err(bad_transition(turn.state));
            }
        }
        session_facts(t, c.id, &r).await?;
        if let Some(uuid) = r.last_uuid.as_deref()
            && !line_stored(t, c.id, r.transcript_path.as_deref(), uuid).await?
        {
            return Err(Error::code(
                ErrorCode::TranscriptPending,
                "the turn's last transcript line is not stored yet",
            ));
        }
        let error = match outcome {
            TurnOutcome::Failed => Some(r.error.as_ref().map_or_else(
                || ErrorInfo::new(DataCode::RunError, "the supervisor reported a failure without an error"),
                |e| ErrorInfo::new(e.code.clone(), e.message.clone()),
            )),
            _ => None,
        };
        if r.error.as_ref().is_some_and(|e| e.code == DataCode::CcWriteStuck) {
            crate::ccfs::set_resync(t, c.id).await?;
        }
        // the turn's truncated lines that were not written again are retracted before its end (magpie 6.7)
        crate::ccfs::limbo::flush_scope(t, c.id, Some(turn.id)).await?;
        let ending = Ending {
            outcome,
            error,
            held: tasks_of(&r),
            usage: r.usage.map(serde_json::to_value).transpose()?,
            duration_ms: r.duration_ms.and_then(|d| i64::try_from(d).ok()),
            kill: None,
        };
        end::end(t, &c.person, &turn, ending).await
    })
    .await
}

/// Opens a turn Claude Code started by itself (`origin: agent`), running from the start.
async fn open_agent(tx: &Tx<'_>, c: &Locked, caller: &Sandbox, id: Uuid, at: OffsetDateTime) -> Result<()> {
    let body = EventBody::TurnStarted(TurnStartedData {
        turn_id: id,
        origin: TurnOrigin::Agent,
    });
    let seq = events::append(tx, c.id, NewEvent::of(&body)?.turn(Some(id))).await?;
    tx.execute(
        "insert into turns (id, conversation_id, origin, state, start_seq, created_at, ready_at, started_at, sandbox_id)
         values ($1, $2, 'agent', 'running', $3, now(), now(), $4, $5)",
        &[&id, &c.id, &seq, &at, &caller.sandbox],
    )
    .await?;
    push_running_deadline(tx, id).await?;
    tx.execute(
        "update conversations set run_state = 'running', active_turn = $2, activity_at = now(), sandbox_phase = null
         where id = $1",
        &[&c.id, &id],
    )
    .await?;
    turn_change(tx, &c.person, c.id, id, TurnPhase::Started, TurnOrigin::Agent, None).await?;
    changes::upserted(tx, &c.person, c.id).await?;
    Ok(())
}

/// `POST /v1/sandbox/turns`: 201, or 200 for a repeat. A conversation with an active turn answers 409
/// `bad_transition` (Claude Code runs one turn at a time; the active turn's end comes first).
///
/// # Errors
/// 400 for an origin other than `agent`; 404, 409; a statement error.
pub async fn agent_turn(pool: &Pool, caller: &Sandbox, r: AgentTurnRequest) -> Result<Admitted<Empty>> {
    if r.origin != TurnOrigin::Agent {
        return Err(Error::bad_request("origin must be agent"));
    }
    aviary_pg::tx!(pool, |t| -> Result<Admitted<Empty>> {
        let c = caller::lock(t, caller).await?;
        if let Some(row) = t
            .query_opt("select conversation_id from turns where id = $1", &[&r.turn_id])
            .await?
        {
            return if row.get::<_, Uuid>(0) == c.id {
                Ok(Admitted::Repeat(Empty {}))
            } else {
                Err(Error::not_found())
            };
        }
        if let Some(a) = active(t, c.id).await?.filter(|a| !a.state.is_ended()) {
            return Err(bad_transition(a.state));
        }
        open_agent(t, &c, caller, r.turn_id, r.at.0).await?;
        Ok(Admitted::New(Empty {}))
    })
    .await
}

fn terminal(s: TaskState) -> bool {
    !matches!(s, TaskState::Running)
}

/// `POST /v1/sandbox/tasks`: `task.status` events (a repeated `id` is skipped) and the conversation's `background`.
///
/// # Errors
/// 404, 409 `lease_lost` / `conversation_migrating`; a statement error.
pub async fn tasks(pool: &Pool, caller: &Sandbox, r: TasksReport) -> Result<()> {
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = caller::lock(t, caller).await?;
        let row = t
            .query_one("select background from conversations where id = $1", &[&c.id])
            .await?;
        let mut background: Vec<BackgroundTask> = serde_json::from_value(row.get::<_, Value>(0)).unwrap_or_default();
        let mut changed = false;
        for e in &r.events {
            let fresh = t
                .execute(
                    "insert into report_ids (conversation_id, id) values ($1, $2) on conflict do nothing",
                    &[&c.id, &e.id],
                )
                .await?;
            if fresh == 0 {
                continue;
            }
            let body = EventBody::TaskStatus(TaskStatus {
                task_id: e.task_id.clone(),
                tool_use_id: e.tool_use_id.clone(),
                event: e.event,
                status: e.status,
                description: e.description.clone(),
                summary: e.summary.clone(),
                output_file: e.output_file.clone(),
            });
            let mut ev = NewEvent::of(&body)?.turn(c.active_turn);
            ev.at = e.at.0;
            events::append(t, c.id, ev).await?;
            let pos = background.iter().position(|b| b.task_id == e.task_id);
            if terminal(e.status) {
                if let Some(i) = pos {
                    background.remove(i);
                    changed = true;
                }
            } else if pos.is_none() {
                background.push(BackgroundTask {
                    task_id: e.task_id.clone(),
                    tool_use_id: e.tool_use_id.clone(),
                    kind: "task".to_owned(),
                    description: e.description.clone(),
                    started_at: Some(e.at),
                });
                changed = true;
            }
        }
        if changed {
            let idle = background.is_empty() && c.run_state == "held";
            t.execute(
                "update conversations set background = $2,
                        run_state = case when $3 then 'idle' else run_state end
                 where id = $1",
                &[&c.id, &serde_json::to_value(&background)?, &idle],
            )
            .await?;
            changes::upserted(t, &c.person, c.id).await?;
        }
        Ok(())
    })
    .await
}

async fn ask_event(tx: &Tx<'_>, conversation: Uuid, kind: &str, ask_id: &str) -> Result<bool> {
    Ok(tx
        .query_opt(
            "select 1 from events where conversation_id = $1 and kind = $2 and data->>'askId' = $3 limit 1",
            &[&conversation, &kind, &ask_id],
        )
        .await?
        .is_some())
}

/// `POST /v1/sandbox/asks`: `ask.pending`, change `ask`. Idempotent per `askId`.
///
/// # Errors
/// 404, 409 `lease_lost` / `conversation_migrating`; a statement error.
pub async fn ask_open(pool: &Pool, caller: &Sandbox, r: AskReport) -> Result<()> {
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = caller::lock(t, caller).await?;
        if ask_event(t, c.id, "ask.pending", &r.ask_id).await? {
            return Ok(());
        }
        let now = OffsetDateTime::now_utc();
        let body = EventBody::AskPending(AskPending {
            ask_id: r.ask_id.clone(),
            tool_name: r.tool_name.clone(),
            tool_use_id: r.tool_use_id.clone(),
            input: r.input.clone(),
        });
        let mut ev = NewEvent::of(&body)?.turn(c.active_turn);
        ev.at = now;
        events::append(t, c.id, ev).await?;
        let ask = Ask {
            id: r.ask_id.clone(),
            tool_name: r.tool_name.clone(),
            tool_use_id: r.tool_use_id.clone(),
            input: r.input.clone(),
            title: None,
            description: None,
            created_at: Timestamp(now),
        };
        changes::emit(t, &c.person, c.id, &ChangeBody::Ask { ask }).await?;
        changes::upserted(t, &c.person, c.id).await?;
        Ok(())
    })
    .await
}

/// `POST /v1/sandbox/asks/{askId}/resolved`: `ask.resolved` with the person's answer (or the cancel resolution),
/// change `ask_resolved`. Idempotent per `askId`.
///
/// # Errors
/// 404 for an ask never opened; 409 `lease_lost` / `conversation_migrating`; a statement error.
pub async fn ask_resolved(pool: &Pool, caller: &Sandbox, ask_id: &str, r: AskResolvedReport) -> Result<()> {
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = caller::lock(t, caller).await?;
        if !ask_event(t, c.id, "ask.pending", ask_id).await? {
            return Err(Error::not_found());
        }
        if ask_event(t, c.id, "ask.resolved", ask_id).await? {
            return Ok(());
        }
        let resolution = match r.outcome {
            AskOutcome::Canceled => AgentPermissionResponse::canceled(),
            AskOutcome::Answered => {
                if let Some(a) = super::cancel::stored_answer(t, c.id, ask_id).await? {
                    a
                } else {
                    tracing::warn!(conversation = %c.id, ask = %ask_id, "an ask reported answered has no stored answer");
                    AgentPermissionResponse::canceled()
                }
            }
        };
        if r.outcome == AskOutcome::Canceled {
            t.execute(
                "update commands set canceled_at = now()
                 where conversation_id = $1 and type = 'answer_ask' and payload->>'askId' = $2
                   and delivered_at is null and canceled_at is null",
                &[&c.id, &ask_id],
            )
            .await?;
        }
        let body = EventBody::AskResolved(AskResolved {
            ask_id: ask_id.to_owned(),
            resolution: resolution.clone(),
        });
        events::append(t, c.id, NewEvent::of(&body)?.turn(c.active_turn)).await?;
        changes::emit(
            t,
            &c.person,
            c.id,
            &ChangeBody::AskResolved(AskResolvedChange {
                ask_id: ask_id.to_owned(),
                resolution,
            }),
        )
        .await?;
        changes::upserted(t, &c.person, c.id).await?;
        Ok(())
    })
    .await
}

/// Rules 1, 2 and the lost `running` report of a starting turn (magpie 8.5) for the active turn `turn`.
async fn reconcile_active(t: &Tx<'_>, c: &Locked, caller: &Sandbox, hb: &Heartbeat, turn: &Turn) -> Result<()> {
    let (_, acked) = start_turn_status(t, turn).await?;
    let starting = turn.state == State::Starting && acked;
    if !starting && !matches!(turn.state, State::Running | State::Stopping) {
        return Ok(());
    }
    let last = hb.last_turn.as_ref();
    // 1: the supervisor ended T and its end report never landed
    if let Some(l) = last.filter(|l| l.id == turn.id && hb.turn_id != Some(turn.id)) {
        let ending = Ending {
            outcome: l.outcome,
            error: match l.outcome {
                TurnOutcome::Failed => Some(l.error.as_ref().map_or_else(
                    || ErrorInfo::new(DataCode::RunError, "the supervisor reported a failure without an error"),
                    |e| ErrorInfo::new(e.code.clone(), e.message.clone()),
                )),
                _ => None,
            },
            held: None,
            usage: None,
            duration_ms: None,
            kill: None,
        };
        end::end(t, &c.person, turn, ending).await?;
        if hb.state == HeartbeatState::Held {
            t.execute(
                "update conversations set run_state = 'held' where id = $1 and active_turn is null",
                &[&c.id],
            )
            .await?;
        }
        return Ok(());
    }
    // 0: a starting turn the supervisor runs, whose running report never landed
    if starting && hb.turn_id == Some(turn.id) {
        let r = TurnStateReport {
            state: ReportedState::Running,
            at: Timestamp::now(),
            provider_session_id: None,
            transcript_path: None,
            exit_code: None,
            error: None,
            held: None,
            tasks: None,
            last_uuid: None,
            usage: None,
            duration_ms: None,
        };
        tracing::info!(turn = %turn.id, "heartbeat reconciled: running");
        return running(t, c, caller, turn, &r).await;
    }
    // 2: the supervisor no longer knows T
    if acked && hb.state == HeartbeatState::Idle && hb.turn_id.is_none() && last.is_none_or(|l| l.id != turn.id) {
        let e = ErrorInfo::new(DataCode::RunError, "the sandbox no longer knows this turn");
        return end::end(t, &c.person, turn, Ending::failed(e)).await;
    }
    if turn.state == State::Running {
        push_running_deadline(t, turn.id).await?;
    }
    Ok(())
}
/// `POST /v1/sandbox/heartbeat`: `heartbeat_at`, a running turn's deadline, and the reconciliation of magpie 8.5.
///
/// # Errors
/// 404, 409 `lease_lost` / `conversation_migrating`; a statement error.
pub async fn heartbeat(pool: &Pool, caller: &Sandbox, hb: Heartbeat) -> Result<()> {
    tracing::debug!(
        conversation = %caller.conversation,
        sandbox = %caller.sandbox,
        generation = hb.generation,
        supervisor = %hb.supervisor.version,
        boot = %hb.supervisor.boot_id,
        restarts = hb.supervisor.restarts,
        quota_exceeded = hb.cc.quota_exceeded,
        blocked_writes = hb.cc.blocked_writes,
        oldest_blocked_ms = hb.cc.oldest_blocked_ms,
        "heartbeat"
    );
    if hb.cc.blocked_writes > 0 || hb.cc.quota_exceeded {
        tracing::warn!(
            conversation = %caller.conversation,
            blocked_writes = hb.cc.blocked_writes,
            oldest_blocked_ms = hb.cc.oldest_blocked_ms,
            quota_exceeded = hb.cc.quota_exceeded,
            "supervisor reports blocked cc writes"
        );
    }
    // what the admin reads show (magpie 13)
    let facts = &serde_json::json!({"gen": hb.generation, "supervisor": hb.supervisor, "cc": hb.cc});
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = caller::lock(t, caller).await?;
        t.execute(
            "update conversations set heartbeat_at = now(), last_heartbeat = $2 where id = $1",
            &[&c.id, facts],
        )
        .await?;
        if let Some(turn) = active(t, c.id).await?.filter(|a| !a.state.is_ended()) {
            return reconcile_active(t, &c, caller, &hb, &turn).await;
        }
        // 3: a turn Claude Code started whose report never landed
        if let Some(id) = hb.turn_id {
            let known = t.query_opt("select 1 from turns where id = $1", &[&id]).await?;
            if known.is_none() {
                return open_agent(t, &c, caller, id, OffsetDateTime::now_utc()).await;
            }
        }
        // 4: background tasks after the turn
        let to = match (hb.state, c.run_state.as_str()) {
            (HeartbeatState::Held, s) if s != "held" => Some("held"),
            (HeartbeatState::Idle, "held") => Some("idle"),
            _ => None,
        };
        if let Some(to) = to {
            if to == "idle" {
                let before = t
                    .query_one("select background from conversations where id = $1", &[&c.id])
                    .await?;
                super::untracked(t, c.id, None, &before.get(0), &[]).await?;
            }
            t.execute(
                "update conversations set run_state = $2,
                        background = case when $2 = 'idle' then '[]'::jsonb else background end
                 where id = $1",
                &[&c.id, &to],
            )
            .await?;
            changes::upserted(t, &c.person, c.id).await?;
        }
        Ok(())
    })
    .await
}
