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
//!   only a later event, and when it did either, bumps the epoch and writes `timeline_replaced` to the person's
//!   changes;
//! - every transaction that wrote a row or bumped the epoch sends [`crate::fanout::ROWS`] `<conversation>:<row_head>`.
//!
//! The reads (pages, prompts, search) are [`read`].

pub mod derive;
pub mod read;

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
                               message_id, block_index, is_prompt, seqs)
             values ($1, $2, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, array[$2::bigint])",
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
        let seq = self.next_seq(tx).await?;
        let item = serde_json::to_value(RowItem::Tool(item.clone()))?;
        tx.execute(
            "update rows set item = $3, seq_end = $4, seqs = seqs || $4::bigint, source_seqs = $5
             where conversation_id = $1 and row_id = $2",
            &[&self.conversation, &row_id, &item, &seq, &sources],
        )
        .await?;
        self.issue(tx, seq, row_id).await
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
    if derive::apply(&mut row.item, body, meta) {
        if !row.sources.contains(&ev.seq) {
            row.sources.push(ev.seq);
        }
        w.update(tx, row.row_id, &row.item, &row.sources).await?;
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
            retract(tx, &mut w, seqs).await?;
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

/// Rule 4: one row per non-empty block of an assistant line; a tool row picks up a sent-file snapshot that came
/// first; the block's live frames are deleted with the durable row's insert (magpie 7.5 supersede).
async fn assistant(tx: &Tx<'_>, w: &mut Writer, ev: &Stored, line: &Value, meta: &EventMeta) -> Result<()> {
    let Some(mid) = derive::message_id(line) else {
        return Ok(());
    };
    // the blocks of earlier lines of the same message that still count (Claude Code writes one line per block); a
    // line that replaces a retracted one (a rewrite put it back) counts from where that one stood
    let bound = meta
        .replaces
        .and_then(|r| i64::try_from(r).ok())
        .filter(|r| *r < ev.seq)
        .unwrap_or(ev.seq);
    let first: i64 = tx
        .query_one(
            "select coalesce(sum(case when jsonb_typeof(e.data->'message'->'content') = 'array'
                                      then jsonb_array_length(e.data->'message'->'content') else 1 end), 0)::bigint
             from events e
             where e.conversation_id = $1 and e.kind = 'cc.assistant' and (e.data->'message'->>'id') = $2
               and e.seq < $3
               and not exists (select 1 from retracted r where r.conversation_id = e.conversation_id and r.seq = e.seq)",
            &[&w.conversation, &mid, &bound],
        )
        .await?
        .get(0);
    let first = u32::try_from(first).unwrap_or(u32::MAX);
    for b in derive::assistant(line, meta, first) {
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
        tx.execute(
            "delete from live_frames where conversation_id = $1 and message_id = $2 and block_index = $3",
            &[&w.conversation, &mid, &block_index],
        )
        .await?;
    }
    Ok(())
}

/// Rule 9.
async fn retract(tx: &Tx<'_>, w: &mut Writer, seqs: Vec<i64>) -> Result<()> {
    if seqs.is_empty() {
        return Ok(());
    }
    tx.execute(
        "insert into retracted (conversation_id, seq) select $1, unnest($2::bigint[]) on conflict do nothing",
        &[&w.conversation, &seqs],
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
    let gone: Vec<i64> = tx
        .query(
            "select seq from retracted where conversation_id = $1",
            &[&w.conversation],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    let mut changed = false;
    for r in touched {
        let row_id: i64 = r.get(0);
        let kind: String = r.get(1);
        let sources: Vec<i64> = r.get(3);
        let identifying_gone = sources.first().is_none_or(|s| gone.contains(s));
        let rebuilt = if identifying_gone || kind != "tool" {
            None
        } else {
            let old = tool_of(r.get(2))?;
            let block_index = u32::try_from(r.get::<_, Option<i32>>(4).unwrap_or(0)).unwrap_or(0);
            rebuild_tool(tx, w.conversation, &old.call_id, block_index, &sources, &gone).await?
        };
        match rebuilt {
            Some((item, surviving)) => w.update(tx, row_id, &item, &surviving).await?,
            None => {
                tx.execute(
                    "delete from rows where conversation_id = $1 and row_id = $2",
                    &[&w.conversation, &row_id],
                )
                .await?;
            }
        }
        changed = true;
    }
    if changed {
        let row = tx
            .query_one(
                "update conversations set epoch = epoch + 1 where id = $1 returning epoch, person",
                &[&w.conversation],
            )
            .await?;
        let epoch: i64 = row.get(0);
        let person: String = row.get(1);
        changes::emit(
            tx,
            &person,
            w.conversation,
            &ChangeBody::TimelineReplaced {
                epoch: epoch.to_string(),
            },
        )
        .await?;
        w.wrote = true;
    }
    Ok(())
}

/// A tool row made again from its surviving events, in the order they first landed on it; `None` when its
/// identifying line is not among them.
async fn rebuild_tool(
    tx: &Tx<'_>,
    conversation: Uuid,
    call_id: &str,
    block_index: u32,
    sources: &[i64],
    gone: &[i64],
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
