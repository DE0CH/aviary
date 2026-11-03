//! The timeline projection (magpie 7.3): events into protocol-neutral rows, in the same transaction as the event,
//! under the conversation row lock.
//!
//! [`project`] runs for every appended event ([`crate::events::append`]): it finds the rows the event touches, asks
//! [`derive`] what the event makes of them, and keeps the bookkeeping:
//! - every new row and every row update takes the next row seq (`conversations.row_head + 1`), recorded in
//!   `row_seqs` and in the row's `seqs`, so every integer from 1 to `row_head` is issued to exactly one row version;
//! - a row keeps its identity (`row_id` = its first row seq) through updates; `source_seqs` lists the events it was
//!   built from, the identifying one first, then the updates in the order they landed;
//! - a `retract` (rule 9) deletes the rows whose identifying event left the transcript, rebuilds a tool row that lost
//!   only a later event (only its sources change when the rebuilt item is the one it shows), and when it deleted or
//!   changed a row, bumps the epoch and writes `timeline_replaced` to the person's changes;
//! - a rewritten line (`meta.replaces`) of an assistant message or a prompt takes over the rows of the line it
//!   replaces before that line's retract lands ([`assistant`], [`prompt_takeover`]);
//! - every transaction that wrote a row or bumped the epoch sends [`crate::fanout::ROWS`] `<conversation>:<row_head>`.
//!
//! The reads (pages, prompts, search) are [`read`].

pub mod derive;
pub mod read;

use std::collections::HashSet;

use aviary_pg::Tx;
use magpie_api::changes::ChangeBody;
use magpie_api::events::{EventBody, EventMeta};
use magpie_api::rows::{RowItem, ToolItem};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::events::Stored;
use crate::{changes, fanout};

/// Reads an event's kind and data as the contract's [`EventBody`] (unknown kinds: [`EventBody::Unknown`]).
///
/// # Errors
/// When the data of a known kind does not decode.
pub fn body_of(kind: &str, data: &Value) -> Result<EventBody> {
    Ok(serde_json::from_value(json!({"kind": kind, "data": data}))?)
}

/// Reads an event's `meta` (absent or undecodable: empty).
#[must_use]
pub fn meta_of(meta: Option<&Value>) -> EventMeta {
    meta.and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default()
}

/// The version of the derivation (rules 1-10) a row was made or last updated by, stored in `rows.derivation`. A
/// release that changes what an event makes of a row raises it and ships a job that rebuilds the rows below it
/// from their events (the projection never re-derives stored rows by itself; magpie 7.3 changes them only on an
/// event).
pub const DERIVATION: i16 = 1;

/// A new row.
struct NewRow {
    item: RowItem,
    at: OffsetDateTime,
    turn_id: Option<Uuid>,
    sources: Vec<i64>,
    tool_use_id: Option<String>,
    message_id: Option<String>,
    block_index: Option<i32>,
    is_prompt: bool,
}

/// A tool row as stored.
struct ToolRow {
    row_id: i64,
    item: ToolItem,
    sources: Vec<i64>,
}

/// The projection of one transaction's event: what it wrote.
struct Writer {
    conversation: Uuid,
    wrote: bool,
}

impl Writer {
    async fn next_seq(&self, tx: &Tx<'_>) -> Result<i64> {
        let row = tx
            .query_one(
                "update conversations set row_head = row_head + 1 where id = $1 returning row_head",
                &[&self.conversation],
            )
            .await?;
        Ok(row.get(0))
    }

    async fn insert(&mut self, tx: &Tx<'_>, r: NewRow) -> Result<i64> {
        let seq = self.next_seq(tx).await?;
        let kind = r
            .item
            .kind()
            .ok_or_else(|| Error::Internal("a row of an unknown kind".into()))?;
        let item = serde_json::to_value(&r.item)?;
        tx.execute(
            "insert into rows (conversation_id, row_id, seq_end, kind, at, turn_id, item, source_seqs, tool_use_id,
                               message_id, block_index, is_prompt, seqs, derivation)
             values ($1, $2, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, array[$2::bigint], $12)",
            &[
                &self.conversation,
                &seq,
                &kind,
                &r.at,
                &r.turn_id,
                &item,
                &r.sources,
                &r.tool_use_id,
                &r.message_id,
                &r.block_index,
                &r.is_prompt,
                &DERIVATION,
            ],
        )
        .await?;
        self.issue(tx, seq, seq).await?;
        Ok(seq)
    }

    async fn issue(&mut self, tx: &Tx<'_>, seq: i64, row_id: i64) -> Result<()> {
        tx.execute(
            "insert into row_seqs (conversation_id, seq, row_id) values ($1, $2, $3)",
            &[&self.conversation, &seq, &row_id],
        )
        .await?;
        self.wrote = true;
        Ok(())
    }

    /// A new version of tool row `row_id` with `item` and the given sources.
    async fn update(&mut self, tx: &Tx<'_>, row_id: i64, item: &ToolItem, sources: &[i64]) -> Result<()> {
        self.update_row(tx, row_id, &RowItem::Tool(item.clone()), sources).await
    }

    /// A new version of row `row_id` (of the same kind) with `item` and the given sources.
    async fn update_row(&mut self, tx: &Tx<'_>, row_id: i64, item: &RowItem, sources: &[i64]) -> Result<()> {
        let seq = self.next_seq(tx).await?;
        let item = serde_json::to_value(item)?;
        tx.execute(
            "update rows set item = $3, seq_end = $4, seqs = seqs || $4::bigint, source_seqs = $5, derivation = $6
             where conversation_id = $1 and row_id = $2",
            &[&self.conversation, &row_id, &item, &seq, &sources, &DERIVATION],
        )
        .await?;
        self.issue(tx, seq, row_id).await
    }

    /// The sources of row `row_id` change and its item does not: no new version, no row seq (an event that
    /// repeats what the row already shows is still a source a rebuild reads).
    async fn set_sources(&self, tx: &Tx<'_>, row_id: i64, sources: &[i64]) -> Result<()> {
        tx.execute(
            "update rows set source_seqs = $3 where conversation_id = $1 and row_id = $2",
            &[&self.conversation, &row_id, &sources],
        )
        .await?;
        Ok(())
    }

    /// Deletes row `row_id` (its seqs leave the timeline: the caller bumps the epoch).
    async fn delete(&self, tx: &Tx<'_>, row_id: i64) -> Result<()> {
        tx.execute(
            "delete from rows where conversation_id = $1 and row_id = $2",
            &[&self.conversation, &row_id],
        )
        .await?;
        Ok(())
    }

    /// `epoch + 1` and a `timeline_replaced` change (rule 9: a visible row was deleted or rebuilt).
    async fn replace_epoch(&mut self, tx: &Tx<'_>) -> Result<()> {
        let row = tx
            .query_one(
                "update conversations set epoch = epoch + 1 where id = $1 returning epoch, person",
                &[&self.conversation],
            )
            .await?;
        let epoch: i64 = row.get(0);
        let person: String = row.get(1);
        changes::emit(
            tx,
            &person,
            self.conversation,
            &ChangeBody::TimelineReplaced {
                epoch: epoch.to_string(),
            },
        )
        .await?;
        self.wrote = true;
        Ok(())
    }
}

fn tool_of(item: Value) -> Result<ToolItem> {
    match serde_json::from_value(item)? {
        RowItem::Tool(t) => Ok(t),
        _ => Err(Error::Internal("a tool_use_id row that is not a tool row".into())),
    }
}

/// The current tool row of `tool_use_id`, if any.
async fn tool_row(tx: &Tx<'_>, conversation: Uuid, tool_use_id: &str) -> Result<Option<ToolRow>> {
    let Some(r) = tx
        .query_opt(
            "select row_id, item, source_seqs from rows
             where conversation_id = $1 and tool_use_id = $2 and kind = 'tool'
             order by row_id desc limit 1",
            &[&conversation, &tool_use_id],
        )
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(ToolRow {
        row_id: r.get(0),
        item: tool_of(r.get(1))?,
        sources: r.get(2),
    }))
}

/// Applies `body` to the tool row of `tool_use_id`, when that row exists and the event concerns it.
async fn update_tool(
    tx: &Tx<'_>,
    w: &mut Writer,
    ev: &Stored,
    tool_use_id: &str,
    body: &EventBody,
    meta: &EventMeta,
) -> Result<()> {
    let Some(mut row) = tool_row(tx, w.conversation, tool_use_id).await? else {
        return Ok(());
    };
    let before = row.item.clone();
    if derive::apply(&mut row.item, body, meta) {
        let new_source = !row.sources.contains(&ev.seq);
        if new_source {
            row.sources.push(ev.seq);
        }
        if row.item != before {
            w.update(tx, row.row_id, &row.item, &row.sources).await?;
        } else if new_source {
            // nothing visible changed (a repeated progress report, result or snapshot): no row seq
            w.set_sources(tx, row.row_id, &row.sources).await?;
        }
    }
    Ok(())
}

/// Projects one stored event into the conversation's rows (rules 1-10 of magpie 7.3).
///
/// # Errors
/// When a statement fails, or the data of a known kind does not decode.
#[allow(clippy::too_many_lines)] // one arm per rule, in the spec's order
pub async fn project(tx: &Tx<'_>, ev: &Stored) -> Result<()> {
    let body = body_of(&ev.event.kind, &ev.event.data)?;
    let meta = meta_of(ev.event.meta.as_ref());
    let mut w = Writer {
        conversation: ev.conversation_id,
        wrote: false,
    };
    let at = ev.event.at;
    let turn_id = ev.event.turn_id;
    match &body {
        // rule 1
        EventBody::UserSent(u) => {
            w.insert(
                tx,
                NewRow {
                    item: derive::user_sent(u, ev.event.viewer.as_deref()),
                    at,
                    turn_id,
                    sources: vec![ev.seq],
                    tool_use_id: None,
                    message_id: Some(u.client_message_id.clone()),
                    block_index: None,
                    is_prompt: true,
                },
            )
            .await?;
        }
        EventBody::CcUser(line) if !derive::sidechain(line, &meta) => {
            if let Some(n) = &meta.task_notification {
                // rule 3
                update_tool(tx, &mut w, ev, &n.tool_use_id, &body, &meta).await?;
            } else {
                let results = derive::tool_results(line);
                if results.is_empty() {
                    if meta.prompt == Some(true) {
                        // rule 2
                        let item = derive::prompt(line, &meta, ev.event.text.as_deref());
                        let message_id = match &item {
                            RowItem::User(u) => Some(u.message_id.clone()),
                            _ => None,
                        };
                        if prompt_takeover(tx, &mut w, ev, &meta, &item).await? {
                            // the rewritten prompt line kept the row of the line it replaces
                        } else {
                            w.insert(
                                tx,
                                NewRow {
                                    item,
                                    at,
                                    turn_id,
                                    sources: vec![ev.seq],
                                    tool_use_id: None,
                                    message_id,
                                    block_index: None,
                                    is_prompt: true,
                                },
                            )
                            .await?;
                        }
                    }
                } else {
                    // rule 5
                    let mut seen = Vec::new();
                    for (id, _) in results {
                        if !seen.contains(&id) {
                            update_tool(tx, &mut w, ev, &id, &body, &meta).await?;
                            seen.push(id);
                        }
                    }
                }
            }
        }
        // rule 4
        EventBody::CcAssistant(line) if !derive::sidechain(line, &meta) => {
            assistant(tx, &mut w, ev, line, &meta).await?;
        }
        // rule 6
        EventBody::FilesSent(f) => update_tool(tx, &mut w, ev, &f.tool_use_id, &body, &meta).await?,
        // rule 7
        EventBody::TaskStatus(s) => {
            if let Some(id) = &s.tool_use_id {
                update_tool(tx, &mut w, ev, id, &body, &meta).await?;
            }
        }
        // rule 8
        EventBody::TurnEnded(e) => {
            let running = tx
                .query(
                    "select row_id, item, source_seqs from rows
                     where conversation_id = $1 and turn_id = $2 and kind = 'tool' and (item->>'status') = 'running'
                     order by row_id",
                    &[&w.conversation, &e.turn_id],
                )
                .await?;
            for r in running {
                let mut item = tool_of(r.get(1))?;
                let mut sources: Vec<i64> = r.get(2);
                if derive::apply_turn_end(&mut item, e) {
                    sources.push(ev.seq);
                    w.update(tx, r.get(0), &item, &sources).await?;
                }
            }
            if let Some(item) = derive::turn_error_row(e) {
                w.insert(
                    tx,
                    NewRow {
                        item,
                        at,
                        turn_id: Some(e.turn_id),
                        sources: vec![ev.seq],
                        tool_use_id: None,
                        message_id: None,
                        block_index: None,
                        is_prompt: false,
                    },
                )
                .await?;
            }
        }
        // rule 9
        EventBody::Retract(r) => {
            let seqs = r.seqs.iter().filter_map(|s| i64::try_from(*s).ok()).collect();
            retract(tx, &mut w, seqs, ev.seq).await?;
        }
        // rule 10, sidechain lines and unknown kinds: no row
        _ => {}
    }
    if w.wrote {
        let head: i64 = tx
            .query_one("select row_head from conversations where id = $1", &[&w.conversation])
            .await?
            .get(0);
        fanout::notify(tx, fanout::ROWS, &w.conversation.to_string(), head).await?;
    }
    Ok(())
}

/// Rule 2 for a rewritten prompt line (`meta.replaces = R`, R a `cc.user` line with the same `uuid` that still
/// counts): the new line takes over R's `user` row, as rule 4 does for an assistant line. R counts as gone from now
/// on (`retracted`, with `by_seq` the new line, so a share frozen before the rewrite still has R), the row keeps its
/// `row_id` and is updated at a new row seq, or keeps its version when the text, images and files are the same. R's
/// retract then touches no row, so no commit shows two `user` rows with one `messageId`. `false` when there is
/// nothing to take over (the caller inserts a new row).
async fn prompt_takeover(tx: &Tx<'_>, w: &mut Writer, ev: &Stored, meta: &EventMeta, item: &RowItem) -> Result<bool> {
    let RowItem::User(u) = item else {
        return Ok(false);
    };
    let Some(r) = meta
        .replaces
        .and_then(|r| i64::try_from(r).ok())
        .filter(|r| *r < ev.seq)
    else {
        return Ok(false);
    };
    let counts = tx
        .query_opt(
            "select 1 from events e
             where e.conversation_id = $1 and e.seq = $2 and e.kind = 'cc.user' and (e.data->>'uuid') = $3
               and not exists (select 1 from retracted x where x.conversation_id = e.conversation_id and x.seq = e.seq)",
            &[&w.conversation, &r, &u.message_id],
        )
        .await?
        .is_some();
    if !counts {
        return Ok(false);
    }
    let Some(old) = rows_of(tx, w.conversation, r)
        .await?
        .into_iter()
        .find(|o| o.kind == "user")
    else {
        return Ok(false);
    };
    tx.execute(
        "insert into retracted (conversation_id, seq, by_seq) values ($1, $2, $3) on conflict do nothing",
        &[&w.conversation, &r, &ev.seq],
    )
    .await?;
    if *item == old.item {
        w.set_sources(tx, old.row_id, &[ev.seq]).await?;
    } else {
        w.update_row(tx, old.row_id, item, &[ev.seq]).await?;
    }
    Ok(true)
}

/// A row made from the line a rewritten line replaces, as rule 4 finds it to take it over.
struct OldRow {
    row_id: i64,
    kind: String,
    item: RowItem,
    sources: Vec<i64>,
    tool_use_id: Option<String>,
    block_index: Option<i32>,
}

/// The line a rewritten line of message `mid` replaces (`meta.replaces = seq`), when it is a line of the same
/// message: `(still counts, where it stands)`. `None` for any other event (the `replaces` is ignored).
async fn replaced_line(tx: &Tx<'_>, conversation: Uuid, seq: i64, mid: &str) -> Result<Option<(bool, i64)>> {
    Ok(tx
        .query_opt(
            "select not exists (select 1 from retracted r where r.conversation_id = e.conversation_id and r.seq = e.seq),
                    coalesce(p.pos, e.seq)
             from events e
             left join line_pos p on p.conversation_id = e.conversation_id and p.seq = e.seq
             where e.conversation_id = $1 and e.seq = $2 and e.kind = 'cc.assistant'
               and (e.data->'message'->>'id') = $3",
            &[&conversation, &seq, &mid],
        )
        .await?
        .map(|r| (r.get(0), r.get(1))))
}

/// The rows whose identifying event is `seq`, by block index.
async fn rows_of(tx: &Tx<'_>, conversation: Uuid, seq: i64) -> Result<Vec<OldRow>> {
    tx.query(
        "select row_id, kind, item, source_seqs, tool_use_id, block_index from rows
         where conversation_id = $1 and source_seqs[1] = $2 order by row_id",
        &[&conversation, &seq],
    )
    .await?
    .into_iter()
    .map(|r| {
        Ok(OldRow {
            row_id: r.get(0),
            kind: r.get(1),
            item: serde_json::from_value(r.get(2))?,
            sources: r.get(3),
            tool_use_id: r.get(4),
            block_index: r.get(5),
        })
    })
    .collect()
}

/// Where a line of message `mid` stands, and the rows it takes over: `(position, rows of the replaced line)`. Its position is its own seq, or where the line it rewrites (`replaced`, a line of
/// the same message) stood, recorded in `line_pos`; when that line still counts it is recorded in `retracted` and
/// its rows are handed over.
async fn place(
    tx: &Tx<'_>,
    conversation: Uuid,
    seq: i64,
    replaced: Option<i64>,
    mid: &str,
) -> Result<(i64, Vec<OldRow>)> {
    let Some(r) = replaced else {
        return Ok((seq, Vec::new()));
    };
    let Some((counts, pos)) = replaced_line(tx, conversation, r, mid).await? else {
        return Ok((seq, Vec::new()));
    };
    tx.execute(
        "insert into line_pos (conversation_id, seq, pos) values ($1, $2, $3) on conflict do nothing",
        &[&conversation, &seq, &pos],
    )
    .await?;
    if !counts {
        return Ok((pos, Vec::new()));
    }
    tx.execute(
        "insert into retracted (conversation_id, seq, by_seq) values ($1, $2, $3) on conflict do nothing",
        &[&conversation, &r, &seq],
    )
    .await?;
    Ok((pos, rows_of(tx, conversation, r).await?))
}

/// Rule 4: one row per non-empty block of an assistant line; a tool row picks up a sent-file snapshot that came
/// first; the block's live frames are deleted with the durable row's insert (magpie 7.5 supersede).
///
/// A line with `meta.replaces = R` (Claude Code rewrote line R: an overwrite, a rename-over, a truncate whose
/// limbo is not yet flushed) is R's successor: when R is a line of the same message, the new line stands where R
/// stood (`line_pos`; its blocks count there for the block index of every line). When R still counts, the new
/// line takes over R's rows in this transaction, before R's retract lands: R counts as gone from now on (block
/// indexes, rebuilds; it is recorded in `retracted`), a row of R at the same block index and of the same kind
/// (the same call, for a tool row) is updated in place at a new row seq and keeps its `row_id` (a tool row keeps
/// its later results, snapshots and task reports, rebuilt from them), or keeps its version when nothing visible
/// changed; a row of R the new line has no block for is deleted (a new epoch, as rule 9). R's retract then touches
/// no row. So no commit ever shows two rows with one `(messageId, blockIndex)` or two tool rows with one `callId`.
async fn assistant(tx: &Tx<'_>, w: &mut Writer, ev: &Stored, line: &Value, meta: &EventMeta) -> Result<()> {
    let Some(mid) = derive::message_id(line) else {
        return Ok(());
    };
    let replaced = meta
        .replaces
        .and_then(|r| i64::try_from(r).ok())
        .filter(|r| *r < ev.seq);
    let (bound, mut old) = place(tx, w.conversation, ev.seq, replaced, mid).await?;
    // the blocks of the message's lines that still count and stand before this one (Claude Code writes one line
    // per block)
    let first: i64 = tx
        .query_one(
            "select coalesce(sum(case when jsonb_typeof(e.data->'message'->'content') = 'array'
                                      then jsonb_array_length(e.data->'message'->'content') else 1 end), 0)::bigint
             from events e
             left join line_pos p on p.conversation_id = e.conversation_id and p.seq = e.seq
             where e.conversation_id = $1 and e.kind = 'cc.assistant' and (e.data->'message'->>'id') = $2
               and coalesce(p.pos, e.seq) < $3
               and not exists (select 1 from retracted r where r.conversation_id = e.conversation_id and r.seq = e.seq)",
            &[&w.conversation, &mid, &bound],
        )
        .await?
        .get(0);
    // and past every row of the message's earlier lines: a line that counted when a later row was numbered and was
    // retracted since leaves a gap, and counting alone would give a later line an index a row already has
    let past: i64 = tx
        .query_one(
            "select coalesce(max(r.block_index) + 1, 0)::bigint from rows r
             left join line_pos p on p.conversation_id = r.conversation_id and p.seq = r.source_seqs[1]
             where r.conversation_id = $1 and r.message_id = $2 and r.block_index is not null and not r.is_prompt
               and coalesce(p.pos, r.source_seqs[1]) < $3",
            &[&w.conversation, &mid, &bound],
        )
        .await?
        .get(0);
    let first = u32::try_from(first.max(past)).unwrap_or(u32::MAX);
    let mut replaced_rows = false;
    for b in derive::assistant(line, meta, first) {
        let block_index = i32::try_from(b.block_index).unwrap_or(i32::MAX);
        let kind = b.item.kind().unwrap_or_default();
        let same = old
            .iter()
            .position(|o| o.block_index == Some(block_index) && o.kind == kind && o.tool_use_id == b.tool_use_id);
        if let Some(i) = same {
            let o = old.remove(i);
            let (item, sources) = match (&b.item, &b.tool_use_id) {
                (RowItem::Tool(_), Some(id)) => {
                    let mut sources = vec![ev.seq];
                    sources.extend(o.sources.iter().skip(1).copied());
                    let gone = gone_among(tx, w.conversation, &sources).await?;
                    match rebuild_tool(tx, w.conversation, id, b.block_index, &sources, &gone).await? {
                        Some((t, kept)) => (RowItem::Tool(t), kept),
                        None => (b.item.clone(), vec![ev.seq]),
                    }
                }
                _ => (b.item.clone(), vec![ev.seq]),
            };
            if item == o.item {
                w.set_sources(tx, o.row_id, &sources).await?;
            } else {
                w.update_row(tx, o.row_id, &item, &sources).await?;
            }
        } else {
            if let Some(i) = old.iter().position(|o| o.block_index == Some(block_index)) {
                // R's row at this index is of another kind or another call: it goes (a new epoch below)
                let o = old.remove(i);
                w.delete(tx, o.row_id).await?;
                replaced_rows = true;
            }
            insert_block(tx, w, ev, mid, b).await?;
        }
        tx.execute(
            "delete from live_frames where conversation_id = $1 and message_id = $2 and block_index = $3",
            &[&w.conversation, &mid, &block_index],
        )
        .await?;
    }
    for o in old {
        // a block of R that the rewritten line no longer has
        w.delete(tx, o.row_id).await?;
        replaced_rows = true;
    }
    if replaced_rows {
        w.replace_epoch(tx).await?;
    }
    Ok(())
}

/// Rule 4's new row for one block.
async fn insert_block(tx: &Tx<'_>, w: &mut Writer, ev: &Stored, mid: &str, b: derive::BlockRow) -> Result<()> {
    let mut item = b.item;
    let mut sources = vec![ev.seq];
    if let (Some(id), RowItem::Tool(t)) = (&b.tool_use_id, &mut item)
        && let Some(sent) = tx
            .query_opt(
                "select e.seq, e.data from events e
                 where e.conversation_id = $1 and e.kind = 'files.sent' and (e.data->>'toolUseId') = $2
                 order by e.seq desc limit 1",
                &[&w.conversation, id],
            )
            .await?
    {
        // rule 6's snapshot came before its tool row
        let data: Value = sent.get(1);
        if let EventBody::FilesSent(f) = body_of("files.sent", &data)? {
            derive::apply_sent(t, &f);
            sources.push(sent.get(0));
        }
    }
    let block_index = i32::try_from(b.block_index).unwrap_or(i32::MAX);
    w.insert(
        tx,
        NewRow {
            item,
            at: ev.event.at,
            turn_id: ev.event.turn_id,
            sources,
            tool_use_id: b.tool_use_id,
            message_id: Some(mid.to_owned()),
            block_index: Some(block_index),
            is_prompt: false,
        },
    )
    .await?;
    Ok(())
}

/// Rule 9 (`by`: the retract event's seq).
async fn retract(tx: &Tx<'_>, w: &mut Writer, seqs: Vec<i64>, by: i64) -> Result<()> {
    if seqs.is_empty() {
        return Ok(());
    }
    tx.execute(
        "insert into retracted (conversation_id, seq, by_seq) select $1, unnest($2::bigint[]), $3
         on conflict do nothing",
        &[&w.conversation, &seqs, &by],
    )
    .await?;
    let touched = tx
        .query(
            "select row_id, kind, item, source_seqs, block_index from rows
             where conversation_id = $1 and source_seqs && $2::bigint[]
             order by row_id",
            &[&w.conversation, &seqs],
        )
        .await?;
    if touched.is_empty() {
        return Ok(());
    }
    let mut changed = false;
    for r in touched {
        let row_id: i64 = r.get(0);
        let kind: String = r.get(1);
        let sources: Vec<i64> = r.get(3);
        let gone = gone_among(tx, w.conversation, &sources).await?;
        let identifying_gone = sources.first().is_none_or(|s| gone.contains(s));
        let rebuilt = if identifying_gone || kind != "tool" {
            None
        } else {
            let old = tool_of(r.get(2))?;
            let block_index = u32::try_from(r.get::<_, Option<i32>>(4).unwrap_or(0)).unwrap_or(0);
            rebuild_tool(tx, w.conversation, &old.call_id, block_index, &sources, &gone)
                .await?
                .map(|x| (old, x))
        };
        match rebuilt {
            Some((old, (item, surviving))) if item == old => {
                // the surviving events make the row it already shows (a rewritten result line that took the
                // retracted one's place): only its sources change, no row seq, no new epoch
                w.set_sources(tx, row_id, &surviving).await?;
            }
            Some((_, (item, surviving))) => {
                w.update(tx, row_id, &item, &surviving).await?;
                changed = true;
            }
            None => {
                w.delete(tx, row_id).await?;
                changed = true;
            }
        }
    }
    if changed {
        w.replace_epoch(tx).await?;
    }
    Ok(())
}

/// Which of `seqs` count as gone from the transcript (`retracted`: a retract, or a rewrite that took the line's
/// place). Only those seqs are read, so the cost does not grow with the conversation's retracts.
async fn gone_among(tx: &Tx<'_>, conversation: Uuid, seqs: &[i64]) -> Result<HashSet<i64>> {
    Ok(tx
        .query(
            "select seq from retracted where conversation_id = $1 and seq = any($2)",
            &[&conversation, &seqs],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

/// A tool row made again from its surviving events, in the order they first landed on it; `None` when its
/// identifying line is not among them.
async fn rebuild_tool(
    tx: &Tx<'_>,
    conversation: Uuid,
    call_id: &str,
    block_index: u32,
    sources: &[i64],
    gone: &HashSet<i64>,
) -> Result<Option<(ToolItem, Vec<i64>)>> {
    let surviving: Vec<i64> = sources.iter().copied().filter(|s| !gone.contains(s)).collect();
    let rows = tx
        .query(
            "select seq, kind, data, meta from events where conversation_id = $1 and seq = any($2)",
            &[&conversation, &surviving],
        )
        .await?;
    let mut by_seq = std::collections::HashMap::new();
    for r in &rows {
        by_seq.insert(r.get::<_, i64>(0), r);
    }
    let mut item: Option<ToolItem> = None;
    let mut kept = Vec::new();
    for s in &surviving {
        let Some(r) = by_seq.get(s) else { continue };
        let kind: String = r.get(1);
        let data: Value = r.get(2);
        let meta = meta_of(r.get::<_, Option<Value>>(3).as_ref());
        let body = body_of(&kind, &data)?;
        match &mut item {
            None => {
                let EventBody::CcAssistant(line) = &body else {
                    return Ok(None);
                };
                item = derive::tool_from_line(line, call_id, block_index);
                if item.is_none() {
                    return Ok(None);
                }
                kept.push(*s);
            }
            Some(t) => {
                if derive::apply(t, &body, &meta) {
                    kept.push(*s);
                }
            }
        }
    }
    Ok(item.map(|t| (t, kept)))
}
