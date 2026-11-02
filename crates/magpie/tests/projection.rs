//! The projection's property test (magpie 20): random event sequences (tool calls with late results, task
//! notifications, sent files before and after their tool row, failed and canceled turns, retracts, noise kinds)
//! appended through magpie in random transaction groupings must give exactly the rows a reference projector gives;
//! `row_seqs` must cover `1..row_head` exactly once; `after` paging from every `S` must return every live row seq
//! once, in order; and the epoch changes only on a retract that removed or rebuilt a row.
//!
//! The reference keeps everything in memory and does its own lookups and bookkeeping; it shares only the pure
//! per-event derivations of `magpie::rows::derive` (each rule of which `timeline.rs` checks from events to rows).

#![allow(
    clippy::assert_is_empty,
    clippy::items_after_statements,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::assigning_clones,
    clippy::collapsible_if,
    clippy::many_single_char_names,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

mod common;

use std::collections::{BTreeMap, HashSet};

use common::ev::{self, ALICE};
use magpie::events::NewEvent;
use magpie::rows::{body_of, derive, meta_of};
use magpie_api::events::EventBody;
use magpie_api::rows::{RowItem, ToolItem};
use serde_json::{Value, json};
use uuid::Uuid;

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> Option<&'a T> {
        if v.is_empty() {
            None
        } else {
            Some(&v[self.below(v.len())])
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RefRow {
    row_id: i64,
    seq_end: i64,
    seqs: Vec<i64>,
    kind: String,
    turn_id: Option<Uuid>,
    item: RowItem,
    sources: Vec<i64>,
    tool_use_id: Option<String>,
    message_id: Option<String>,
    block_index: Option<i32>,
}

struct RefEvent {
    seq: i64,
    kind: String,
    data: Value,
    meta: Option<Value>,
    turn_id: Option<Uuid>,
    text: Option<String>,
    viewer: Option<String>,
}

/// The reference projector.
#[derive(Default)]
struct Reference {
    events: Vec<RefEvent>,
    rows: BTreeMap<i64, RefRow>,
    row_seqs: Vec<(i64, i64)>,
    row_head: i64,
    epoch: i64,
    retracted: HashSet<i64>,
    /// Where a rewritten line stands (the seq of the first line of its chain of rewrites).
    pos: std::collections::HashMap<i64, i64>,
    replaced: usize,
    taken_over: usize,
}

impl Reference {
    fn issue(&mut self, row_id: i64) -> i64 {
        self.row_head += 1;
        self.row_seqs.push((self.row_head, row_id));
        self.row_head
    }

    fn insert(&mut self, mut r: RefRow) {
        let s = self.row_head + 1;
        self.issue(s);
        r.row_id = s;
        r.seq_end = s;
        r.seqs = vec![s];
        r.kind = r.item.kind().unwrap().to_owned();
        self.rows.insert(s, r);
    }

    fn update(&mut self, row_id: i64, item: ToolItem, sources: Vec<i64>) {
        self.update_row(row_id, RowItem::Tool(item), sources);
    }

    fn update_row(&mut self, row_id: i64, item: RowItem, sources: Vec<i64>) {
        let s = self.issue(row_id);
        let r = self.rows.get_mut(&row_id).unwrap();
        r.item = item;
        r.seq_end = s;
        r.seqs.push(s);
        r.sources = sources;
    }

    fn tool(&self, id: &str) -> Option<(i64, ToolItem, Vec<i64>)> {
        self.rows
            .values()
            .rev()
            .find(|r| r.kind == "tool" && r.tool_use_id.as_deref() == Some(id))
            .map(|r| match &r.item {
                RowItem::Tool(t) => (r.row_id, t.clone(), r.sources.clone()),
                _ => unreachable!(),
            })
    }

    fn touch(&mut self, seq: i64, id: &str, body: &EventBody, meta: &magpie_api::events::EventMeta) {
        if let Some((row_id, mut t, mut sources)) = self.tool(id) {
            let before = t.clone();
            if derive::apply(&mut t, body, meta) {
                let new_source = !sources.contains(&seq);
                if new_source {
                    sources.push(seq);
                }
                if t != before {
                    self.update(row_id, t, sources);
                } else if new_source {
                    self.rows.get_mut(&row_id).unwrap().sources = sources;
                }
            }
        }
    }

    fn new_row(item: RowItem, turn_id: Option<Uuid>, sources: Vec<i64>) -> RefRow {
        RefRow {
            row_id: 0,
            seq_end: 0,
            seqs: Vec::new(),
            kind: String::new(),
            turn_id,
            item,
            sources,
            tool_use_id: None,
            message_id: None,
            block_index: None,
        }
    }

    fn apply(&mut self, e: RefEvent) {
        let body = body_of(&e.kind, &e.data).unwrap();
        let meta = meta_of(e.meta.as_ref());
        let seq = e.seq;
        match &body {
            EventBody::UserSent(u) => {
                let mut r = Self::new_row(derive::user_sent(u, e.viewer.as_deref()), e.turn_id, vec![seq]);
                r.message_id = Some(u.client_message_id.clone());
                self.insert(r);
            }
            EventBody::CcUser(line) if !derive::sidechain(line, &meta) => {
                if let Some(n) = &meta.task_notification {
                    self.touch(seq, &n.tool_use_id.clone(), &body, &meta);
                } else {
                    let results = derive::tool_results(line);
                    if results.is_empty() {
                        if meta.prompt == Some(true) {
                            let item = derive::prompt(line, &meta, e.text.as_deref());
                            let mut r = Self::new_row(item, e.turn_id, vec![seq]);
                            if let RowItem::User(u) = &r.item {
                                r.message_id = Some(u.message_id.clone());
                            }
                            self.insert(r);
                        }
                    } else {
                        let mut seen: Vec<String> = Vec::new();
                        for (id, _) in results {
                            if !seen.contains(&id) {
                                self.touch(seq, &id, &body, &meta);
                                seen.push(id);
                            }
                        }
                    }
                }
            }
            EventBody::CcAssistant(line) if !derive::sidechain(line, &meta) => {
                let mid = derive::message_id(line).unwrap().to_owned();
                let replaced = meta.replaces.map(|r| r as i64).filter(|r| *r < seq);
                // a rewrite of a line of the same message stands where that line stood, and takes over its rows
                // while that line still counts
                let mut old: Vec<RefRow> = Vec::new();
                let mut bound = seq;
                if let Some(r) = replaced
                    && self.events.iter().any(|x| {
                        x.seq == r && x.kind == "cc.assistant" && derive::message_id(&x.data) == Some(mid.as_str())
                    })
                {
                    bound = self.pos.get(&r).copied().unwrap_or(r);
                    self.pos.insert(seq, bound);
                    if !self.retracted.contains(&r) {
                        self.taken_over += 1;
                        self.retracted.insert(r);
                        old = self.rows.values().filter(|x| x.sources[0] == r).cloned().collect();
                    }
                }
                let first: u32 = self
                    .events
                    .iter()
                    .filter(|x| {
                        x.kind == "cc.assistant"
                            && self.pos.get(&x.seq).copied().unwrap_or(x.seq) < bound
                            && !self.retracted.contains(&x.seq)
                            && derive::message_id(&x.data) == Some(mid.as_str())
                    })
                    .map(|x| derive::block_count(&x.data))
                    .sum();
                // the rebuild reads this event too
                self.events.push(copy_ref(&e));
                let mut replaced_rows = false;
                for b in derive::assistant(line, &meta, first) {
                    let bi = b.block_index as i32;
                    let kind = b.item.kind().unwrap();
                    if let Some(i) = old
                        .iter()
                        .position(|o| o.block_index == Some(bi) && o.kind == kind && o.tool_use_id == b.tool_use_id)
                    {
                        let o = old.remove(i);
                        let (item, sources) = if o.kind == "tool" {
                            let mut cand = o.clone();
                            cand.sources = std::iter::once(seq).chain(o.sources.iter().skip(1).copied()).collect();
                            match self.rebuild(&cand) {
                                Some((t, kept)) => (RowItem::Tool(t), kept),
                                None => (b.item.clone(), vec![seq]),
                            }
                        } else {
                            (b.item.clone(), vec![seq])
                        };
                        if item == o.item {
                            self.rows.get_mut(&o.row_id).unwrap().sources = sources;
                        } else {
                            self.update_row(o.row_id, item, sources);
                        }
                        continue;
                    }
                    if let Some(i) = old.iter().position(|o| o.block_index == Some(bi)) {
                        let o = old.remove(i);
                        self.rows.remove(&o.row_id);
                        replaced_rows = true;
                    }
                    let mut item = b.item;
                    let mut sources = vec![seq];
                    if let (Some(id), RowItem::Tool(t)) = (&b.tool_use_id, &mut item)
                        && let Some(sent) = self
                            .events
                            .iter()
                            .rev()
                            .find(|x| x.kind == "files.sent" && x.data["toolUseId"].as_str() == Some(id))
                    {
                        if let EventBody::FilesSent(f) = body_of("files.sent", &sent.data).unwrap() {
                            derive::apply_sent(t, &f);
                            sources.push(sent.seq);
                        }
                    }
                    let mut r = Self::new_row(item, e.turn_id, sources);
                    r.tool_use_id = b.tool_use_id;
                    r.message_id = Some(mid.clone());
                    r.block_index = Some(bi);
                    self.insert(r);
                }
                for o in old {
                    self.rows.remove(&o.row_id);
                    replaced_rows = true;
                }
                if replaced_rows {
                    self.epoch += 1;
                    self.replaced += 1;
                }
                return;
            }
            EventBody::FilesSent(f) => self.touch(seq, &f.tool_use_id.clone(), &body, &meta),
            EventBody::TaskStatus(s) => {
                if let Some(id) = s.tool_use_id.clone() {
                    self.touch(seq, &id, &body, &meta);
                }
            }
            EventBody::TurnEnded(t) => {
                let running: Vec<i64> = self
                    .rows
                    .values()
                    .filter(|r| {
                        r.kind == "tool"
                            && r.turn_id == Some(t.turn_id)
                            && matches!(&r.item, RowItem::Tool(x) if x.status == magpie_api::rows::ToolStatus::Running)
                    })
                    .map(|r| r.row_id)
                    .collect();
                for row_id in running {
                    let r = &self.rows[&row_id];
                    let (RowItem::Tool(mut item), mut sources) = (r.item.clone(), r.sources.clone()) else {
                        unreachable!()
                    };
                    if derive::apply_turn_end(&mut item, t) {
                        sources.push(seq);
                        self.update(row_id, item, sources);
                    }
                }
                if let Some(item) = derive::turn_error_row(t) {
                    self.insert(Self::new_row(item, Some(t.turn_id), vec![seq]));
                }
            }
            EventBody::Retract(r) => {
                let seqs: Vec<i64> = r.seqs.iter().map(|s| *s as i64).collect();
                self.retracted.extend(seqs.iter().copied());
                let touched: Vec<i64> = self
                    .rows
                    .values()
                    .filter(|x| x.sources.iter().any(|s| seqs.contains(s)))
                    .map(|x| x.row_id)
                    .collect();
                for row_id in &touched {
                    let row = self.rows[row_id].clone();
                    let gone = self.retracted.contains(&row.sources[0]);
                    let rebuilt = if gone || row.kind != "tool" {
                        None
                    } else {
                        self.rebuild(&row)
                    };
                    match rebuilt {
                        Some((item, kept)) => self.update(*row_id, item, kept),
                        None => {
                            self.rows.remove(row_id);
                        }
                    }
                }
                if !touched.is_empty() {
                    self.epoch += 1;
                    self.replaced += 1;
                }
            }
            _ => {}
        }
        self.events.push(e);
    }

    fn rebuild(&self, row: &RefRow) -> Option<(ToolItem, Vec<i64>)> {
        let mut item: Option<ToolItem> = None;
        let mut kept = Vec::new();
        for s in row.sources.iter().filter(|s| !self.retracted.contains(s)) {
            let e = self.events.iter().find(|x| x.seq == *s)?;
            let body = body_of(&e.kind, &e.data).unwrap();
            let meta = meta_of(e.meta.as_ref());
            match &mut item {
                None => {
                    let EventBody::CcAssistant(line) = &body else {
                        return None;
                    };
                    item = Some(derive::tool_from_line(
                        line,
                        row.tool_use_id.as_deref().unwrap(),
                        row.block_index.unwrap() as u32,
                    )?);
                    kept.push(*s);
                }
                Some(t) => {
                    if derive::apply(t, &body, &meta) {
                        kept.push(*s);
                    }
                }
            }
        }
        item.map(|t| (t, kept))
    }
}

/// The generator's state: what exists so far, so later events can refer to it.
struct Gen {
    rng: Rng,
    turn: Uuid,
    message: String,
    messages: usize,
    tools: Vec<String>,
    tool_n: usize,
    cc_seqs: Vec<i64>,
    /// `(message, seq)` of the assistant lines so far.
    lines: Vec<(String, i64)>,
    next_seq: i64,
}

impl Gen {
    fn new(seed: u64, first_seq: i64) -> Self {
        Self {
            rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
            turn: Uuid::new_v4(),
            message: "m0".into(),
            messages: 0,
            tools: Vec::new(),
            tool_n: 0,
            cc_seqs: Vec::new(),
            lines: Vec::new(),
            next_seq: first_seq,
        }
    }

    fn tool_id(&mut self) -> String {
        // sometimes a call whose snapshot or result came first
        if self.rng.chance(15) {
            return format!("t{}", self.tool_n + 1);
        }
        self.rng.pick(&self.tools).cloned().unwrap_or_else(|| "nope".into())
    }

    fn event(&mut self) -> NewEvent {
        let e = match self.rng.below(100) {
            0..=7 => {
                self.turn = Uuid::new_v4();
                ev::turn_started(self.turn)
            }
            8..=15 => ev::user_sent(&format!("c{}", self.rng.next() % 1000), "hello"),
            16..=40 => {
                if self.rng.chance(40) {
                    self.messages += 1;
                    self.message = format!("m{}", self.messages);
                }
                let mut blocks = Vec::new();
                for _ in 0..=self.rng.below(2) {
                    blocks.push(match self.rng.below(5) {
                        0 => ev::text(""),
                        1 => ev::thinking("thinking"),
                        2 => ev::text("words"),
                        _ => {
                            self.tool_n += 1;
                            let id = format!("t{}", self.tool_n);
                            self.tools.push(id.clone());
                            let name = if self.rng.chance(30) { "SendUserFile" } else { "Bash" };
                            ev::tool_use(&id, name)
                        }
                    });
                }
                let mut e = ev::assistant(&self.message, Value::Array(blocks));
                if self.rng.chance(5) {
                    e.meta = Some(json!({"sidechain": true}));
                }
                if self.rng.chance(10)
                    && let Some(old) = self.rng.pick(&self.cc_seqs).copied()
                {
                    e.meta = Some(json!({"replaces": old}));
                } else if self.rng.chance(20) {
                    // Claude Code rewrote a line of this message (its retract may come later, or never)
                    let same: Vec<i64> = self
                        .lines
                        .iter()
                        .filter(|(m, _)| *m == self.message)
                        .map(|(_, s)| *s)
                        .collect();
                    if let Some(old) = self.rng.pick(&same).copied() {
                        e.meta = Some(json!({"replaces": old}));
                    }
                }
                if e.meta.is_none() || e.meta.as_ref().is_some_and(|m| m.get("replaces").is_some()) {
                    self.lines.push((self.message.clone(), self.next_seq));
                }
                e
            }
            41..=55 => {
                let a = self.tool_id();
                let b = self.tool_id();
                let err = self.rng.chance(20);
                if self.rng.chance(30) {
                    ev::results(&[(&a, "out", err), (&b, "more", false)])
                } else {
                    ev::results(&[(&a, "out", err)])
                }
            }
            56..=61 => {
                let id = self.tool_id();
                let status = *self.rng.pick(&["completed", "failed", "killed", "stopped"]).unwrap();
                ev::notification(&id, status, "summary")
            }
            62..=67 => {
                let id = self.tool_id();
                ev::files_sent(&id, "out/f.txt")
            }
            68..=72 => {
                let id = self.tool_id();
                let status = *self.rng.pick(&["running", "completed", "failed", "stopped"]).unwrap();
                ev::task_status(&id, status)
            }
            73..=80 => {
                let outcome = *self.rng.pick(&["completed", "failed", "canceled"]).unwrap();
                ev::turn_ended(self.turn, outcome)
            }
            81..=84 => ev::prompt(&format!("u{}", self.rng.next() % 1000), "a queued prompt"),
            85..=92 => {
                let mut seqs = Vec::new();
                for _ in 0..=self.rng.below(3) {
                    if let Some(s) = self.rng.pick(&self.cc_seqs) {
                        seqs.push(*s);
                    }
                }
                if self.rng.chance(10) {
                    seqs.push(self.next_seq + 1000);
                }
                seqs.dedup();
                ev::retract(&seqs)
            }
            93..=95 => ev::ev("title.changed", json!({"title": "t", "nameSource": null})),
            96..=97 => ev::ev("sandbox.ready", json!({})),
            _ => ev::ev(
                "ask.pending",
                json!({"askId": "a1", "toolName": "AskUserQuestion", "toolUseId": null, "input": {}}),
            ),
        };
        let e = if e.turn_id.is_none() && e.kind != "turn.ended" {
            ev::in_turn(e, self.turn)
        } else {
            e
        };
        if e.kind.starts_with("cc.") {
            self.cc_seqs.push(self.next_seq);
        }
        self.next_seq += 1;
        e
    }
}

fn copy_ref(e: &RefEvent) -> RefEvent {
    RefEvent {
        seq: e.seq,
        kind: e.kind.clone(),
        data: e.data.clone(),
        meta: e.meta.clone(),
        turn_id: e.turn_id,
        text: e.text.clone(),
        viewer: e.viewer.clone(),
    }
}

fn copy(e: &NewEvent, seq: i64) -> RefEvent {
    RefEvent {
        seq,
        kind: e.kind.clone(),
        data: e.data.clone(),
        meta: e.meta.clone(),
        turn_id: e.turn_id,
        text: e.text.clone(),
        viewer: e.viewer.clone(),
    }
}

#[tokio::test]
async fn incremental_projection_equals_the_reference_and_row_seqs_are_dense() {
    let db = common::fresh_db("prop").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let mut total_rows = 0;
    let mut total_replaced = 0;
    let mut total_taken_over = 0;
    for seed in 1..=30u64 {
        let id = ev::conversation(&pigeon, ALICE).await;
        // conversation.created is event 1
        let mut g = Gen::new(seed, 2);
        let mut reference = Reference {
            epoch: 1,
            ..Reference::default()
        };
        let feed_before = alice
            .changes(&magpie_api::changes::ChangesQuery {
                after: magpie_api::changes::After::Latest,
                limit: None,
                wait_ms: None,
            })
            .await
            .unwrap()
            .head_seq;
        let n = 30 + g.rng.below(40);
        let mut made = 0;
        while made < n {
            let k = 1 + g.rng.below(3);
            let batch: Vec<NewEvent> = (0..k).map(|_| g.event()).collect();
            let copies: Vec<NewEvent> = batch.clone();
            let seqs = ev::append(a.pool(), ALICE, id, batch).await;
            for (e, s) in copies.iter().zip(seqs) {
                reference.apply(copy(e, s));
            }
            made += k;
        }
        // the stored state equals the reference's
        let c = sql.get().await.unwrap();
        let rows = c
            .query(
                "select row_id, seq_end, seqs, kind, turn_id, item, source_seqs, tool_use_id, message_id, block_index
                 from rows where conversation_id = $1 order by row_id",
                &[&id],
            )
            .await
            .unwrap();
        let stored: Vec<RefRow> = rows
            .iter()
            .map(|r| RefRow {
                row_id: r.get(0),
                seq_end: r.get(1),
                seqs: r.get(2),
                kind: r.get(3),
                turn_id: r.get(4),
                item: serde_json::from_value(r.get(5)).unwrap(),
                sources: r.get(6),
                tool_use_id: r.get(7),
                message_id: r.get(8),
                block_index: r.get(9),
            })
            .collect();
        let expected: Vec<RefRow> = reference.rows.values().cloned().collect();
        assert_eq!(stored.len(), expected.len(), "seed {seed}");
        for (s, e) in stored.iter().zip(&expected) {
            assert_eq!(s, e, "seed {seed}");
        }
        let head = c
            .query_one("select row_head, epoch from conversations where id = $1", &[&id])
            .await
            .unwrap();
        let (row_head, epoch): (i64, i64) = (head.get(0), head.get(1));
        assert_eq!((row_head, epoch), (reference.row_head, reference.epoch), "seed {seed}");
        // row_seqs covers 1..row_head exactly once, each to the row the reference issued it to
        let issued: Vec<(i64, i64)> = c
            .query(
                "select seq, row_id from row_seqs where conversation_id = $1 order by seq",
                &[&id],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        assert_eq!(issued, reference.row_seqs, "seed {seed}");
        assert_eq!(
            issued.iter().map(|x| x.0).collect::<Vec<_>>(),
            (1..=row_head).collect::<Vec<_>>()
        );
        // every live row's seqs are the row_seqs issued to it
        for r in &stored {
            let mine: Vec<i64> = issued.iter().filter(|x| x.1 == r.row_id).map(|x| x.0).collect();
            assert_eq!(r.seqs, mine, "seed {seed}");
            assert_eq!(r.seqs.last(), Some(&r.seq_end));
        }
        // after paging from every S returns every seq of a live row once, in order
        let live: Vec<i64> = issued
            .iter()
            .filter(|x| reference.rows.contains_key(&x.1))
            .map(|x| x.0)
            .collect();
        for s in 0..=row_head {
            // a whole page from every S; walked in small pages from a few
            let limit = if s == 0 || s == row_head / 2 || s % 17 == 3 {
                1 + (s as u32 % 5)
            } else {
                500
            };
            let mut got = Vec::new();
            let mut cursor = s as u64;
            loop {
                let p = alice.timeline(id, &ev::after(cursor, limit)).await.unwrap();
                if p.rows.is_empty() {
                    break;
                }
                for r in &p.rows {
                    assert!(reference.rows.contains_key(&(r.seq_start as i64)));
                    got.push(r.seq_end as i64);
                }
                cursor = p.rows.last().unwrap().seq_end;
            }
            let want: Vec<i64> = live.iter().copied().filter(|x| *x > s).collect();
            assert_eq!(got, want, "seed {seed} after={s}");
        }
        // one timeline_replaced per epoch change
        let feed = alice
            .changes(&magpie_api::changes::ChangesQuery {
                after: magpie_api::changes::After::Seq(feed_before),
                limit: Some(500),
                wait_ms: None,
            })
            .await
            .unwrap();
        let replaced = feed
            .changes
            .iter()
            .filter(|c| matches!(c.body, magpie_api::changes::ChangeBody::TimelineReplaced { .. }))
            .count();
        assert_eq!(replaced, reference.replaced, "seed {seed}");
        total_rows += stored.len();
        total_replaced += reference.replaced;
        total_taken_over += reference.taken_over;
    }
    // the sequences exercised what they should
    assert!(total_rows > 300, "{total_rows} rows");
    assert!(total_replaced > 10, "{total_replaced} epoch changes");
    assert!(
        total_taken_over > 5,
        "{total_taken_over} rewritten lines took over rows"
    );
}
