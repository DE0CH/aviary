//! The supervisor's work queue (magpie 8.4): `GET /v1/sandbox/commands?after=&waitMs=` long-polls for the commands
//! after `after` that are neither canceled nor acknowledged (a restarted supervisor polls with a smaller `after` and
//! gets its unacknowledged commands again), and marks them delivered. A `kill` queued for a turn of another sandbox
//! (one that went away after the kill was queued) is canceled instead of served: its process died with that sandbox,
//! and the caller's own generation must not be killed by it. `POST …/commands/{cmdSeq}/ack` records the
//! result (a `title` ack names the conversation, magpie 9.4).
//!
//! Commands are stored as `type` and `payload` and served through the contract's types, so what reaches the
//! supervisor is exactly [`magpie_api::sandbox::CommandBody`] (the `answer_ask` payload also keeps the person's whole
//! answer for `ask.resolved`, which the typed command drops).

use std::time::Duration;

use magpie_api::errors::ErrorCode;
use magpie_api::sandbox::{AckRequest, Command, CommandBody, CommandsQuery, CommandsResponse};
use serde_json::{Value, json};

use super::caller::{self, Sandbox};
use super::titles;
use crate::changes::Read;
use crate::error::{Error, Result};
use crate::fanout;

fn command_of(seq: i64, kind: &str, turn: Option<uuid::Uuid>, payload: &Value) -> Result<Command> {
    let body: CommandBody = serde_json::from_value(json!({"type": kind, "payload": payload}))?;
    Ok(Command {
        cmd_seq: u64::try_from(seq).unwrap_or(0),
        turn_id: turn,
        body,
    })
}

/// One look: the lease check, then the due commands, marked delivered.
async fn look(app: Read<'_>, caller: &Sandbox, after: i64) -> Result<(Vec<Command>, u64)> {
    let conn = app.pool.get().await?;
    let row = conn
        .query_opt(
            "select person, migrating, lease_id, lease_sandbox, cmd_head from conversations where id = $1",
            &[&caller.conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    caller::check(&row, caller)?;
    let head = u64::try_from(row.get::<_, i64>(4)).unwrap_or(0);
    conn.execute(
        "update commands set canceled_at = now()
         where conversation_id = $1 and type = 'kill' and acked_at is null and canceled_at is null
           and turn_id in (select id from turns where conversation_id = $1 and sandbox_id is distinct from $2)",
        &[&caller.conversation, &caller.sandbox],
    )
    .await?;
    let rows = conn
        .query(
            "update commands set delivered_at = coalesce(delivered_at, now())
             where conversation_id = $1 and cmd_seq > $2 and canceled_at is null and acked_at is null
             returning cmd_seq, type, turn_id, payload",
            &[&caller.conversation, &after],
        )
        .await?;
    let mut commands = rows
        .into_iter()
        .map(|r| command_of(r.get(0), r.get::<_, &str>(1), r.get(2), &r.get::<_, Value>(3)))
        .collect::<Result<Vec<_>>>()?;
    commands.sort_by_key(|c| c.cmd_seq);
    Ok((commands, head))
}

/// `GET /v1/sandbox/commands`.
///
/// # Errors
/// 400 for a `waitMs` over 25,000; 404, 409 `lease_lost` / `conversation_migrating`; a statement error.
pub async fn poll(app: Read<'_>, caller: &Sandbox, q: CommandsQuery) -> Result<CommandsResponse> {
    let wait = q.wait_ms.unwrap_or(0);
    if wait > 25_000 {
        return Err(Error::bad_request("waitMs must be 0-25000"));
    }
    let after = i64::try_from(q.after).map_err(|_| Error::bad_request("after is out of range"))?;
    let (commands, head) = look(app, caller, after).await?;
    if !commands.is_empty() || wait == 0 {
        return Ok(CommandsResponse { commands, head });
    }
    let key = caller.conversation.to_string();
    let last_head = std::sync::Mutex::new(head);
    let lh = &last_head;
    app.fanout
        .hold(
            fanout::COMMANDS,
            &key,
            Duration::from_millis(wait),
            app.draining,
            move || async move {
                let (commands, head) = look(app, caller, after).await?;
                if let Ok(mut h) = lh.lock() {
                    *h = head;
                }
                Ok((!commands.is_empty()).then_some(CommandsResponse { commands, head }))
            },
            || CommandsResponse {
                commands: Vec::new(),
                head: lh.lock().map_or(head, |h| *h),
            },
        )
        .await
}

/// `POST /v1/sandbox/commands/{cmdSeq}/ack`. A repeated ack is answered 204 and changes nothing.
///
/// # Errors
/// 404 for an unknown command; 409 `lease_lost` / `conversation_migrating`; a statement error.
pub async fn ack(pool: &aviary_pg::Pool, caller: &Sandbox, cmd_seq: u64, req: AckRequest) -> Result<()> {
    let seq = i64::try_from(cmd_seq).map_err(|_| Error::not_found())?;
    let result = serde_json::to_value(&req.result)?;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = caller::lock(t, caller).await?;
        let row = t
            .query_opt(
                "update commands set acked_at = now(), result = $3
                 where conversation_id = $1 and cmd_seq = $2 and acked_at is null
                 returning type",
                &[&c.id, &seq, &result],
            )
            .await?;
        let Some(row) = row else {
            let exists = t
                .query_opt(
                    "select 1 from commands where conversation_id = $1 and cmd_seq = $2",
                    &[&c.id, &seq],
                )
                .await?;
            return if exists.is_some() {
                Ok(())
            } else {
                Err(Error::code(ErrorCode::NotFound, "no such command"))
            };
        };
        if row.get::<_, &str>(0) == "title" {
            titles::on_ack(
                t,
                &c.person,
                c.id,
                req.result.response.as_ref(),
                req.result.error.as_deref(),
            )
            .await?;
        }
        Ok(())
    })
    .await
}
