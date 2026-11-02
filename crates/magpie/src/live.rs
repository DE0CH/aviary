//! Live frames (magpie 7.5): the partial assistant output of a running turn, received from the supervisor, kept in
//! the unlogged `live_frames` table, served as [`LiveState`] in M9 and `GET …/live`, superseded by the durable row
//! (rule 4 deletes a block's frames in the transaction that writes its row) and swept 60 s after the turn's end.
//!
//! Live frames never take a row seq and are never events. In this release the supervisor sends none and pigeon
//! renders none (platform R36); this is the receiving side.

use std::collections::BTreeMap;

use aviary_pg::{Pool, Tx};
use magpie_api::errors::ErrorCode;
use magpie_api::live::{BlockType, FrameOp, LiveBlock, LiveFrame, LiveState, LiveStatus};
use magpie_api::server::Ctx;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ccfs;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::fanout;

/// Frames of a turn are kept this long after `turn.ended` (magpie 17).
pub const KEPT_AFTER_END: std::time::Duration = std::time::Duration::from_secs(60);
/// The live cleanup runs this often on every replica (magpie 15).
pub const CLEANUP_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

fn op_str(op: FrameOp) -> &'static str {
    match op {
        FrameOp::Start => "start",
        FrameOp::Delta => "delta",
        FrameOp::Stop => "stop",
        FrameOp::Status => "status",
        FrameOp::Gap => "gap",
    }
}

fn block_str(b: BlockType) -> &'static str {
    match b {
        BlockType::Text => "text",
        BlockType::Thinking => "thinking",
        BlockType::ToolUse => "tool_use",
    }
}

fn block_of(s: Option<&str>) -> BlockType {
    match s {
        Some("thinking") => BlockType::Thinking,
        Some("tool_use") => BlockType::ToolUse,
        _ => BlockType::Text,
    }
}

/// The sandbox's conversation, locked, checked against the gateway's identity and the lease with the ccfs layer's
/// checks (magpie 3.3, 6.9), so a durable row cannot land between the checks and the insert. Answers
/// `(conversation, row_head, active_turn)`.
async fn sandbox_conversation(tx: &Tx<'_>, ctx: &Ctx) -> Result<(Uuid, i64, Option<Uuid>)> {
    let caller = ccfs::Caller::from_ctx(ctx)?;
    let conv = ccfs::lock(tx, &caller).await?;
    conv.check_lease(&caller)?;
    let row_head: i64 = tx
        .query_one("select row_head from conversations where id = $1", &[&conv.id])
        .await?
        .get(0);
    Ok((conv.id, row_head, conv.active_turn))
}

/// `POST /v1/sandbox/turns/{turnId}/live`: stores a batch. A re-sent batch is idempotent (`(turn, n)`), a delta
/// repeating bytes already stored at its `(messageId, index, offset)` is ignored, and so is any frame of a block
/// whose durable row exists. A turn that is not the conversation's active turn answers 409 `turn_not_active` (the
/// supervisor drops the frames and never retries).
///
/// # Errors
/// 409 `turn_not_active` / `lease_lost`, 404, 400; a statement error.
pub async fn receive(pool: &Pool, ctx: &Ctx, turn: Uuid, frames: &[LiveFrame]) -> Result<()> {
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let (conversation, row_head, active) = sandbox_conversation(t, ctx).await?;
        if active != Some(turn) {
            return Err(Error::code(ErrorCode::TurnNotActive, "the turn is not the conversation's active turn"));
        }
        let mut top: Option<i64> = None;
        for f in frames {
            let n = i64::try_from(f.n).map_err(|_| Error::bad_request("n is too large"))?;
            let index = f.index.map(i32::try_from).transpose().map_err(|_| Error::bad_request("index is too large"))?;
            let offset = f
                .offset
                .map(i64::try_from)
                .transpose()
                .map_err(|_| Error::bad_request("offset is too large"))?;
            let status: Option<Value> = (f.op == FrameOp::Status).then(|| {
                json!({"status": f.status.clone().unwrap_or_default(), "attempt": f.attempt, "retryInMs": f.retry_in_ms})
            });
            let after_row_seq = (f.op == FrameOp::Start).then_some(row_head);
            let at = f.at.map_or_else(time::OffsetDateTime::now_utc, |a| a.0);
            let op = op_str(f.op);
            let block = f.block.map(block_str);
            let inserted = t
                .execute(
                    "insert into live_frames (conversation_id, turn_id, n, message_id, block_index, op, block, name,
                                              byte_offset, text, status, after_row_seq, at)
                     select $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
                     where not ($4::text is not null and exists (
                             select 1 from rows r where r.conversation_id = $1 and r.message_id = $4
                                                    and r.block_index = $5))
                       and not ($6 = 'delta' and exists (
                             select 1 from live_frames l where l.turn_id = $2 and l.message_id = $4
                                                           and l.block_index = $5 and l.op = 'delta'
                                                           and l.byte_offset = $9))
                     on conflict (turn_id, n) do nothing",
                    &[
                        &conversation,
                        &turn,
                        &n,
                        &f.message_id,
                        &index,
                        &op,
                        &block,
                        &f.name,
                        &offset,
                        &f.text,
                        &status,
                        &after_row_seq,
                        &at,
                    ],
                )
                .await?;
            if inserted > 0 {
                top = Some(top.map_or(n, |m| m.max(n)));
            }
        }
        if let Some(n) = top {
            fanout::notify(t, fanout::LIVE, &conversation.to_string(), n).await?;
        }
        Ok(())
    })
    .await
}

/// One block while the state is built.
struct Building {
    block: BlockType,
    name: Option<String>,
    /// Where `text` starts within the block (0 unless frames before it were lost).
    start: u64,
    text: String,
    done: bool,
    after_row_seq: u64,
    /// The block's byte length after the last frame `<= liveAfter`.
    seen_len: Option<u64>,
    /// A frame after `liveAfter` touched it.
    changed: bool,
}

/// The live state of `turn` in `conversation` (`None` without stored frames). With `live_after`, blocks carry only
/// text added after that frame; without it, from another turn (a `live_after` beyond the turn's last frame), after a
/// `gap` frame past it, or when frames were lost (a truncated table), `reset: true` and every open block is sent
/// whole from where its stored text starts.
///
/// # Errors
/// When a statement fails.
#[allow(clippy::too_many_lines)] // one pass over the frames, in frame order
pub async fn state(db: &impl Db, conversation: Uuid, turn: Uuid, live_after: Option<u64>) -> Result<Option<LiveState>> {
    let rows = db
        .query(
            "select n, message_id, block_index, op, block, name, byte_offset, text, status, after_row_seq
             from live_frames where conversation_id = $1 and turn_id = $2 order by n",
            &[&conversation, &turn],
        )
        .await?;
    let Some(last) = rows.last() else {
        return Ok(None);
    };
    let top = u64::try_from(last.get::<_, i64>(0)).unwrap_or(0);
    let mut reset = live_after.is_none_or(|a| a > top);
    let after = if reset { 0 } else { live_after.unwrap_or(0) };
    let mut blocks: BTreeMap<(String, i32), Building> = BTreeMap::new();
    let mut order: Vec<(String, i32)> = Vec::new();
    let mut status: Option<LiveStatus> = None;
    for r in &rows {
        let n = u64::try_from(r.get::<_, i64>(0)).unwrap_or(0);
        let op: String = r.get(3);
        let later = n > after;
        if op == "gap" {
            if later {
                reset = true;
            }
            continue;
        }
        if op == "status" {
            let s: Option<Value> = r.get(8);
            status = s.and_then(|v| {
                Some(LiveStatus {
                    status: v.get("status")?.as_str()?.to_owned(),
                    attempt: v
                        .get("attempt")
                        .and_then(Value::as_u64)
                        .and_then(|a| u32::try_from(a).ok()),
                    retry_in_ms: v.get("retryInMs").and_then(Value::as_u64),
                })
            });
            continue;
        }
        let (Some(mid), Some(idx)) = (r.get::<_, Option<String>>(1), r.get::<_, Option<i32>>(2)) else {
            continue;
        };
        // a frame of a block clears an earlier retry status: the output resumed
        status = None;
        let key = (mid, idx);
        let offset = r
            .get::<_, Option<i64>>(6)
            .and_then(|o| u64::try_from(o).ok())
            .unwrap_or(0);
        if !blocks.contains_key(&key) {
            let after_row_seq = r
                .get::<_, Option<i64>>(9)
                .and_then(|s| u64::try_from(s).ok())
                .unwrap_or(0);
            if op != "start" {
                // the block's first frames are gone (a crash truncated the unlogged table)
                reset = true;
            }
            blocks.insert(
                key.clone(),
                Building {
                    block: block_of(r.get::<_, Option<String>>(4).as_deref()),
                    name: r.get(5),
                    start: if op == "delta" { offset } else { 0 },
                    text: String::new(),
                    done: false,
                    after_row_seq,
                    seen_len: None,
                    changed: false,
                },
            );
            order.push(key.clone());
        }
        let Some(b) = blocks.get_mut(&key) else { continue };
        match op.as_str() {
            "delta" => {
                let text: String = r.get::<_, Option<String>>(7).unwrap_or_default();
                let end = b.start + b.text.len() as u64;
                if offset > end {
                    // bytes in between were lost: restart the block here
                    reset = true;
                    b.start = offset;
                    b.text = text;
                } else {
                    let skip = usize::try_from(end - offset).unwrap_or(usize::MAX);
                    if skip < text.len() && text.is_char_boundary(skip) {
                        b.text.push_str(&text[skip..]);
                    }
                }
            }
            "stop" => b.done = true,
            _ => {}
        }
        if later {
            b.changed = true;
        } else {
            b.seen_len = Some(b.start + b.text.len() as u64);
        }
    }
    let mut out = Vec::new();
    for key in order {
        let Some(b) = blocks.remove(&key) else { continue };
        let (offset, text) = if reset {
            (b.start, b.text)
        } else {
            if !b.changed {
                continue;
            }
            let seen = b.seen_len.unwrap_or(b.start);
            let from = usize::try_from(seen.saturating_sub(b.start)).unwrap_or(usize::MAX);
            match b.text.get(from..) {
                Some(added) => (seen, added.to_owned()),
                None => (seen, String::new()),
            }
        };
        out.push(LiveBlock {
            message_id: key.0,
            block_index: u32::try_from(key.1).unwrap_or(0),
            block: b.block,
            name: b.name,
            offset,
            text,
            done: b.done,
            after_row_seq: b.after_row_seq,
        });
    }
    Ok(Some(LiveState {
        turn_id: turn,
        n: top,
        reset,
        blocks: out,
        status,
    }))
}

/// `GET /v1/conversations/{id}/live`: the running turn's live state with `reset: true`, `None` (204) when no turn is
/// active or it has no frames.
///
/// # Errors
/// 404 when the conversation is not `person`'s; a statement error.
pub async fn get(pool: &Pool, conversation: Uuid, person: &str) -> Result<Option<LiveState>> {
    let conn = pool.get().await?;
    let row = conn
        .query_opt(
            "select person, active_turn from conversations where id = $1",
            &[&conversation],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let owner: String = row.get(0);
    if owner != person {
        return Err(Error::not_found());
    }
    let Some(turn) = row.get::<_, Option<Uuid>>(1) else {
        return Ok(None);
    };
    state(&conn, conversation, turn, None).await
}

/// The live cleanup (every replica): frames of turns that ended more than 60 s ago; the count deleted.
///
/// # Errors
/// When the statement fails.
pub async fn cleanup(pool: &Pool) -> Result<u64> {
    let conn = pool.get().await?;
    let secs = i64::try_from(KEPT_AFTER_END.as_secs()).unwrap_or(60);
    Ok(conn
        .execute(
            "delete from live_frames l using turns t
             where t.id = l.turn_id and t.ended_at < now() - make_interval(secs => $1::bigint::double precision)",
            &[&secs],
        )
        .await?)
}
