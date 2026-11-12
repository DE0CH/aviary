//! `TimelinePoller(person, conversation)` (spec 8.2): one long-poll of a conversation's timeline (M9 `after`) while
//! a socket of that person on this replica watches it (and 30 s after the last one leaves). Each new or updated row
//! is projected once and pushed as `agent_stream` to every watching socket (7.6.1), in `seqEnd` order.
//!
//! The running turn's live text (magpie 7.5: the model's answer as it comes) rides the same long-poll: each poll
//! says the last live frame it has (`liveAfter`), magpie answers at once when there is more, and the text added to an
//! open text or thinking block goes to every watching socket as an `agent_stream` timeline event without a `seq` (a
//! chunk the app appends to the message it is showing). When the block's durable row lands, a socket that got its
//! beginning as chunks gets the row with only the rest of the text, so the message is whole and the row's `seq`
//! moves its cursor; a socket that joined meanwhile gets the open block from its start first.
//!
//! Turn boundaries from the change feed come through here (8.4): a turn event whose `timelineSeq` is past the rows
//! sent so far waits until the poller has sent that row (at most 2 s), so `turn_completed` follows the reply's last
//! row. A history rewrite (`timeline_replaced`, or a poll answering in another epoch) sends one
//! `agent.timeline.replacement` per new epoch and restarts the poller from the tail of the new epoch (7.6.3); rows
//! of the new epoch are never sent under the old one.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use magpie_api::changes::TurnChange;
use magpie_api::client::ClientError;
use magpie_api::live::{BlockType, LiveState};
use magpie_api::rows::{RowItem, TimelinePage, TimelineQuery};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{Backoff, Hub};
use crate::App;
use crate::paseo::ProjectCtx;
use crate::paseo::messages::payload_rest;
use crate::paseo::pushes;
use crate::paseo::strings::Locale;

/// The poller's control messages.
#[derive(Debug)]
pub enum Ctl {
    /// A turn started or ended (from the change feed).
    Turn {
        /// The change.
        turn: TurnChange,
        /// When the change feed handed it over.
        received: Instant,
    },
    /// The conversation's rows were rebuilt in `epoch`.
    Replaced {
        /// The new epoch.
        epoch: String,
    },
    /// The conversation came back from a delegate (WP-50's release): start again from the tail; `announced` is the
    /// epoch whose replacement the watchers already got.
    Restart {
        /// That epoch.
        announced: String,
    },
}

const PAGE: u32 = 200;

type PollFut = std::pin::Pin<Box<dyn std::future::Future<Output = Poll> + Send>>;

enum Poll {
    Rows(TimelinePage),
    Gone,
    Failed(ClientError),
}

async fn poll(app: App, person: String, c: Uuid, q: TimelineQuery, wait: bool) -> Poll {
    let client = if wait { &app.0.magpie_poll } else { &app.0.magpie };
    let started = Instant::now();
    let r = client
        .person(&person)
        .with_request_id(Uuid::now_v7().to_string())
        .timeline(c, &q)
        .await;
    app.0.metrics.poll("timeline", started.elapsed().as_secs_f64());
    match r {
        Ok(p) => Poll::Rows(p),
        Err(e) if e.api().is_some_and(|r| r.status == 404) => Poll::Gone,
        Err(e) => Poll::Failed(e),
    }
}

fn tail() -> TimelineQuery {
    TimelineQuery {
        tail: true,
        limit: Some(1),
        ..TimelineQuery::default()
    }
}

struct Poller {
    hub: Hub,
    app: App,
    person: String,
    c: Uuid,
    epoch: Option<String>,
    /// The last epoch announced with a replacement (one per new epoch, whichever of the change feed and the poll
    /// sees it first).
    announced: Option<String>,
    cursor: u64,
    pending: VecDeque<(TurnChange, Instant)>,
    live: Live,
}

/// A block of the running turn's answer that is still being written: `(messageId, blockIndex)`.
type BlockKey = (String, u32);

/// The live text sent so far (the module's doc).
#[derive(Default)]
struct Live {
    /// The turn the frames are numbered in, and the last frame seen.
    turn: Option<Uuid>,
    n: u64,
    /// The open blocks, in the order they started.
    order: Vec<BlockKey>,
    open: HashMap<BlockKey, OpenBlock>,
    /// Per socket: how far into each block (a byte offset in the block) it has been sent.
    sent: HashMap<Uuid, HashMap<BlockKey, usize>>,
}

struct OpenBlock {
    thinking: bool,
    /// Where `text` starts within the block (0 unless its first frames were lost).
    start: usize,
    text: String,
}

impl Live {
    /// Takes a poll's live state in: each open block's text grows by what the state adds.
    fn take(&mut self, state: &LiveState) {
        if self.turn != Some(state.turn_id) {
            *self = Self::default();
            self.turn = Some(state.turn_id);
        }
        self.n = state.n;
        for b in &state.blocks {
            let thinking = match b.block {
                BlockType::Text => false,
                BlockType::Thinking => true,
                BlockType::ToolUse => continue,
            };
            let key = (b.message_id.clone(), b.block_index);
            let offset = usize::try_from(b.offset).unwrap_or(usize::MAX);
            if !self.open.contains_key(&key) {
                self.order.push(key.clone());
            }
            let o = self.open.entry(key).or_insert_with(|| OpenBlock {
                thinking,
                start: offset,
                text: String::new(),
            });
            let end = o.start + o.text.len();
            if (o.start..=end).contains(&offset) {
                // the text from somewhere inside what is held (a reset sends blocks whole): the part past its end
                if let Some(rest) = b.text.get(end - offset..) {
                    o.text.push_str(rest);
                }
            } else {
                // a hole (frames were lost): the block goes on from here
                o.start = offset;
                o.text.clone_from(&b.text);
            }
        }
    }

    /// What socket `conn` has not been sent yet of each open block: `(key, thinking, text)`, and marks it sent.
    fn unsent(&mut self, conn: Uuid) -> Vec<(BlockKey, bool, String)> {
        let sent = self.sent.entry(conn).or_default();
        let mut out = Vec::new();
        for key in &self.order {
            let Some(o) = self.open.get(key) else { continue };
            let from = sent.get(key).copied().unwrap_or(o.start).max(o.start);
            if let Some(rest) = o.text.get(from - o.start..).filter(|r| !r.is_empty()) {
                out.push((key.clone(), o.thinking, rest.to_owned()));
            }
            sent.insert(key.clone(), o.start + o.text.len());
        }
        out
    }

    /// The durable row of `key` has landed: the block is open no more. How far socket `conn` was sent into it.
    fn landed(&mut self, key: &BlockKey, conn: Uuid) -> usize {
        self.sent.get_mut(&conn).and_then(|s| s.remove(key)).unwrap_or(0)
    }

    fn close(&mut self, key: &BlockKey) {
        self.open.remove(key);
        self.order.retain(|k| k != key);
    }
}

/// Runs until nobody here watches the conversation for the linger time, the conversation is purged, or the drain.
pub async fn run(hub: Hub, app: App, person: String, c: Uuid, mut ctl: mpsc::UnboundedReceiver<Ctl>) {
    let tuning = app.0.tuning.clone();
    let mut backoff = Backoff::new(tuning.poll_backoff_min, tuning.poll_backoff_max);
    let mut idle_since: Option<Instant> = None;
    let mut p = Poller {
        hub,
        app,
        person,
        c,
        epoch: None,
        announced: None,
        cursor: 0,
        pending: VecDeque::new(),
        live: Live::default(),
    };
    let wait_ms = u64::try_from(tuning.poll_wait.as_millis()).unwrap_or(25_000);
    let mut fut: PollFut = Box::pin(poll(p.app.clone(), p.person.clone(), c, tail(), false));
    let mut tick = tokio::time::interval(
        tuning
            .poller_linger
            .min(std::time::Duration::from_secs(1))
            .max(std::time::Duration::from_millis(20)),
    );
    loop {
        let next_deadline = p.pending.front().map(|(_, at)| *at + tuning.turn_hold);
        tokio::select! {
            m = ctl.recv() => match m {
                Some(Ctl::Turn { turn, received }) => {
                    p.pending.push_back((turn, received));
                    p.release(false);
                }
                Some(Ctl::Restart { announced }) => {
                    p.announced = Some(announced);
                    p.epoch = None;
                    p.pending.clear();
                    fut = Box::pin(poll(p.app.clone(), p.person.clone(), c, tail(), false));
                }
                Some(Ctl::Replaced { epoch }) => {
                    if p.epoch.as_deref() != Some(epoch.as_str()) && p.announced.as_deref() != Some(epoch.as_str()) {
                        p.replaced(&epoch);
                        p.epoch = None;
                        fut = Box::pin(poll(p.app.clone(), p.person.clone(), c, tail(), false));
                    }
                }
                None => return,
            },
            answer = &mut fut => {
                // after an error the backoff is a delay inside the next poll's future: turn events, restarts and
                // replacements are handled meanwhile (8.4's 2 s hold still applies)
                let (query, delay) = match answer {
                    Poll::Rows(page) => {
                        backoff.reset();
                        (p.page(page), None)
                    }
                    Poll::Gone => {
                        p.hub.timeline_poller_gone(&p.person, c);
                        return;
                    }
                    Poll::Failed(error) => {
                        tracing::warn!(%error, "timeline poll failed");
                        (p.next_query(), Some(backoff.wait()))
                    }
                };
                let wait = query.after.is_some();
                let query = if wait { TimelineQuery { wait_ms: Some(wait_ms), ..query } } else { query };
                let (app, person) = (p.app.clone(), p.person.clone());
                fut = Box::pin(async move {
                    if let Some(delay) = delay {
                        tokio::time::sleep(delay).await;
                    }
                    poll(app, person, c, query, wait).await
                });
            }
            () = sleep_until(next_deadline) => p.release(true),
            _ = tick.tick() => {
                if p.hub.timeline_poller_done(&p.person, c, &mut idle_since, tuning.poller_linger) {
                    return;
                }
            }
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending().await,
    }
}

impl Poller {
    /// The sockets to feed: none while a delegate serves the conversation (a poller lingering from before the
    /// delegation must not send beside it).
    fn subscribers(&self) -> Vec<std::sync::Arc<crate::paseo::transport::Conn>> {
        if self.hub.delegate(&self.person, &self.c).is_some() {
            return Vec::new();
        }
        self.hub.timeline_subscribers(&self.person, &self.c)
    }

    fn locale(&self) -> Locale {
        Locale::from_zh(self.app.identities().is_zh())
    }

    fn next_query(&self) -> TimelineQuery {
        if self.epoch.is_none() {
            return tail();
        }
        TimelineQuery {
            after: Some(self.cursor),
            limit: Some(PAGE),
            // the live text after the last frame seen (none seen: magpie sends the open blocks whole)
            live_after: self.live.turn.map(|_| self.live.n),
            live_turn: self.live.turn,
            ..TimelineQuery::default()
        }
    }

    /// One poll's answer; returns the next query.
    fn page(&mut self, page: TimelinePage) -> TimelineQuery {
        match &self.epoch {
            None => {
                // a (re)start from the tail: rows up to maxSeq are history
                self.epoch = Some(page.epoch.clone());
                self.cursor = page.max_seq;
                self.release(false);
                return self.next_query();
            }
            Some(e) if *e != page.epoch => {
                let epoch = page.epoch.clone();
                self.replaced(&epoch);
                self.epoch = None;
                return tail();
            }
            Some(_) => {}
        }
        let epoch = page.epoch.clone();
        let locale = self.locale();
        let signer = self.app.blob_signer();
        let ctx = ProjectCtx {
            zh: locale == Locale::Zh,
            signer: &signer,
        };
        let subscribers = self.subscribers();
        let mut rows = page.rows;
        rows.sort_by_key(|r| r.seq_end);
        for row in rows {
            if row.seq_end <= self.cursor {
                continue;
            }
            // the row of a block whose text went out live: each socket gets what it has not been sent of it
            let block = match &row.item {
                RowItem::Assistant(a) if a.error.is_none() => Some(((a.message_id.clone(), a.block_index), &a.text)),
                RowItem::Reasoning(r) => Some(((r.message_id.clone(), r.block_index), &r.text)),
                _ => None,
            }
            .filter(|(key, _)| self.live.open.contains_key(key));
            if let Some(mut projected) = self.app.0.projector.project(&row, &ctx) {
                let whole = payload_rest(&pushes::timeline_row(&self.c, &projected, &epoch));
                for conn in &subscribers {
                    let rest = block
                        .as_ref()
                        .map(|(key, text)| (self.live.landed(key, conn.id), text))
                        .and_then(|(sent, text)| text.get(sent..).filter(|_| sent > 0))
                        .map(|rest| {
                            if let Some(t) = projected.get_mut("item").and_then(|i| i.get_mut("text")) {
                                *t = serde_json::Value::String(rest.to_owned());
                            }
                            payload_rest(&pushes::timeline_row(&self.c, &projected, &epoch))
                        });
                    conn.send_sub("agent_stream", &conn.timeline_sub(), rest.as_ref().unwrap_or(&whole));
                }
            }
            if let Some((key, _)) = &block {
                self.live.close(key);
            }
            self.cursor = row.seq_end;
        }
        match &page.live {
            Some(state) => {
                self.live.take(state);
                let turn = state.turn_id;
                for conn in &subscribers {
                    for ((message_id, _), thinking, text) in self.live.unsent(conn.id) {
                        let rest = payload_rest(&pushes::live_text(&self.c, &turn, &message_id, thinking, &text));
                        conn.send_sub("agent_stream", &conn.timeline_sub(), &rest);
                    }
                }
                let here: Vec<Uuid> = subscribers.iter().map(|c| c.id).collect();
                self.live.sent.retain(|id, _| here.contains(id));
            }
            // no turn runs (or it has written nothing yet): nothing is open
            None => self.live = Live::default(),
        }
        self.release(false);
        self.next_query()
    }

    /// Sends the waiting turn events whose rows have been sent, and (with `expired`) those held 2 s.
    fn release(&mut self, expired: bool) {
        let hold = self.app.0.tuning.turn_hold;
        while let Some((t, at)) = self.pending.front() {
            let due = self.epoch.is_some() && t.timeline_seq <= self.cursor;
            if !(due || (expired && at.elapsed() >= hold)) {
                break;
            }
            let Some((t, _)) = self.pending.pop_front() else {
                break;
            };
            let payload = pushes::turn_event(&self.c, t.phase, &t.turn_id, t.error.as_ref(), self.locale());
            let rest = payload_rest(&payload);
            for conn in self.subscribers() {
                conn.send_sub("agent_stream", &conn.timeline_sub(), &rest);
            }
        }
    }

    fn replaced(&mut self, epoch: &str) {
        if self.announced.as_deref() == Some(epoch) {
            return;
        }
        self.announced = Some(epoch.to_owned());
        let rest = payload_rest(&pushes::replacement(&self.c, epoch));
        for conn in self.subscribers() {
            conn.send_sub("agent.timeline.replacement", &conn.timeline_sub(), &rest);
        }
    }
}
