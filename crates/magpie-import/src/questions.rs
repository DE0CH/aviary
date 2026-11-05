//! Parked questions of the old stack (DECISIONS 17; claudecodeui fd1b3ed5): a conversation whose turn waited only on
//! an `AskUserQuestion` / `ExitPlanMode` ended that turn, stopped Claude Code and kept the question in its schema's
//! `jarvis_open_questions` (request id, `tool_use_id`, input, the answer once given). Its transcript ends with that
//! `tool_use` dangling. Such a conversation meets the hand-over conditions once its old sandbox idles out, and its
//! question goes with it: the release carries every unanswered one over as a native open question of a parked turn
//! ([`carry`], magpie 8.1a), so the person still sees it, and their answer resumes the conversation on the new stack
//! (supervisor 7.6: the answer is delivered as the next message, in the tool's own words). An answered question whose
//! turn has not run yet keeps the conversation legacy: `holds.js` lists it as running, and [`read`] refuses the release
//! by idle (it is not imported until its turn ran); a forced release carries it with its answer, which resumes the
//! turn at once.

use aviary_pg::{Pool, Tx};
use magpie_api::Timestamp;
use magpie_api::changes::ChangeBody;
use magpie_api::conversations::Ask;
use magpie_api::events::{AskPending, EventBody, TurnStartedData};
use magpie_api::turns::{AgentPermissionResponse, TurnOrigin};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use magpie::events::{self, NewEvent};

use crate::source::parse_time;
use crate::{NS, Result};

/// One row of `jarvis_open_questions`.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenQuestion {
    /// The id the old client answers by: the ask's id in magpie.
    pub request_id: String,
    /// `AskUserQuestion` or `ExitPlanMode`.
    pub tool_name: String,
    /// The dangling `tool_use` of the transcript.
    pub tool_use_id: Option<String>,
    /// The tool input as asked.
    pub input: Map<String, Value>,
    /// When it was asked.
    pub asked_at: OffsetDateTime,
    /// The person's answer, once given (its turn has not run yet).
    pub answer: Option<AgentPermissionResponse>,
}

/// `jarvis_open_questions.answer` (claudecodeui `OpenQuestionAnswer`): `{allow, updatedInput?, message?}`.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OldAnswer {
    allow: bool,
    #[serde(default)]
    updated_input: Option<Map<String, Value>>,
    #[serde(default)]
    message: Option<String>,
}

impl OldAnswer {
    fn response(self) -> AgentPermissionResponse {
        AgentPermissionResponse {
            behavior: if self.allow {
                magpie_api::turns::Behavior::Allow
            } else {
                magpie_api::turns::Behavior::Deny
            },
            selected_action_id: None,
            updated_input: self.updated_input.filter(|_| self.allow),
            updated_permissions: None,
            message: if self.allow { None } else { self.message },
            interrupt: None,
        }
    }
}

/// The parked questions of conversation `id` in `schema` (none for a schema older than the table, version 4).
///
/// # Errors
/// A statement error; [`crate::Error::Source`] for a row whose input is not a JSON object.
pub async fn read(pool: &Pool, schema: &str, id: Uuid) -> Result<Vec<OpenQuestion>> {
    let c = pool.get().await?;
    let table = format!("{schema}.jarvis_open_questions");
    let exists: bool = c
        .query_one("select to_regclass($1) is not null", &[&table])
        .await?
        .get(0);
    if !exists {
        return Ok(Vec::new());
    }
    let rows = c
        .query(
            &format!(
                "select request_id::text, session_id::text, tool_name::text, tool_use_id::text, input::text,
                        asked_at::text, answer::text
                 from {table} order by asked_at, request_id"
            ),
            &[],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let session: String = r.get(1);
        if Uuid::parse_str(&session).ok() != Some(id) {
            continue;
        }
        let request_id: String = r.get(0);
        let input: String = r.get(4);
        let input: Map<String, Value> = serde_json::from_str(&input).map_err(|e| {
            crate::Error::Source(format!(
                "open question {request_id}: its input is not a JSON object: {e}"
            ))
        })?;
        let asked: String = r.get(5);
        let answer: Option<String> = r.get(6);
        out.push(OpenQuestion {
            request_id,
            tool_name: r.get(2),
            tool_use_id: r.get::<_, Option<String>>(3).filter(|s| !s.is_empty()),
            input,
            asked_at: parse_time(&asked).unwrap_or_else(OffsetDateTime::now_utc),
            // an answer of another shape is no answer magpie can deliver: the question stays open
            answer: answer
                .and_then(|a| serde_json::from_str::<OldAnswer>(&a).ok())
                .map(OldAnswer::response),
        });
    }
    Ok(out)
}

/// The parked turn the questions of conversation `id` wait in (one per conversation: the old turn they were asked
/// in), the same id on every run.
#[must_use]
pub fn turn_id(first_request: &str) -> Uuid {
    Uuid::new_v5(&NS, format!("open-question:{first_request}").as_bytes())
}

/// Carries `questions` of conversation `id` (just released, in the release's transaction) over: a parked turn
/// (origin person, asked at the first question's time) whose open asks are the questions, keyed by their
/// `tool_use_id`; the conversation runs it. Answered ones (a forced release) are answered at once, and the last
/// answer admits the turn again (magpie 8.1a).
///
/// # Errors
/// A statement error.
pub async fn carry(t: &Tx<'_>, person: &str, id: Uuid, questions: &[OpenQuestion]) -> Result<()> {
    let Some(first) = questions.first() else {
        return Ok(());
    };
    let turn = turn_id(&first.request_id);
    if t.query_opt("select 1 from turns where id = $1", &[&turn])
        .await?
        .is_some()
    {
        return Ok(());
    }
    let at = questions.iter().map(|q| q.asked_at).min().unwrap_or(first.asked_at);
    let asks: Vec<String> = questions.iter().map(|q| q.request_id.clone()).collect();
    t.execute(
        "insert into turns (id, conversation_id, origin, state, echo_matched, created_at, ready_at, started_at,
                            parked_asks, parked_ms)
         values ($1, $2, 'person', 'parked', true, $3, $3, $3, $4, 0)",
        &[&turn, &id, &at, &asks],
    )
    .await?;
    let body = EventBody::TurnStarted(TurnStartedData {
        turn_id: turn,
        origin: TurnOrigin::Person,
    });
    let mut ev = NewEvent::of(&body)?.turn(Some(turn));
    ev.at = at;
    let start = events::append(t, id, ev).await?;
    t.execute("update turns set start_seq = $2 where id = $1", &[&turn, &start])
        .await?;
    for q in questions {
        let body = EventBody::AskPending(AskPending {
            ask_id: q.request_id.clone(),
            tool_name: q.tool_name.clone(),
            tool_use_id: q.tool_use_id.clone(),
            input: q.input.clone(),
        });
        let mut ev = NewEvent::of(&body)?.turn(Some(turn));
        ev.at = q.asked_at;
        events::append(t, id, ev).await?;
        let ask = Ask {
            id: q.request_id.clone(),
            tool_name: q.tool_name.clone(),
            tool_use_id: q.tool_use_id.clone(),
            input: q.input.clone(),
            title: None,
            description: None,
            created_at: Timestamp(q.asked_at),
        };
        magpie::changes::emit(t, person, id, &ChangeBody::Ask { ask }).await?;
    }
    t.execute(
        "update conversations set active_turn = $2, run_state = 'running' where id = $1",
        &[&id, &turn],
    )
    .await?;
    tracing::info!(conversation = %id, turn = %turn, questions = asks.len(), "parked questions carried over");
    for q in questions {
        if let Some(a) = &q.answer {
            let row = magpie::turns::load(t, id, turn)
                .await?
                .ok_or_else(magpie::error::Error::not_found)?;
            if row.state != magpie::turns::State::Parked {
                break;
            }
            magpie::turns::parked::answer(t, person, &row, &q.request_id, a.clone()).await?;
        }
    }
    magpie::changes::upserted(t, person, id).await?;
    Ok(())
}
