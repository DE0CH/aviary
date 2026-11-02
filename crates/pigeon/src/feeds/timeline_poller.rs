//! `TimelinePoller(person, conversation)` (spec 8.2): one long-poll of a conversation's timeline (M9 `after`) while
//! a socket of that person on this replica watches it (and 30 s after the last one leaves). Each new or updated row
//! is projected once and pushed as `agent_stream` to every watching socket (7.6.1), in `seqEnd` order.
//!
//! Turn boundaries from the change feed come through here (8.4): a turn event whose `timelineSeq` is past the rows
//! sent so far waits until the poller has sent that row (at most 2 s), so `turn_completed` follows the reply's last
//! row. A history rewrite (`timeline_replaced`, or a poll answering in another epoch) sends one
//! `agent.timeline.replacement` per new epoch and restarts the poller from the tail of the new epoch (7.6.3); rows
//! of the new epoch are never sent under the old one.

use std::collections::VecDeque;
use std::time::Instant;

use magpie_api::changes::TurnChange;
use magpie_api::client::ClientError;
use magpie_api::rows::{TimelinePage, TimelineQuery};
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
            if let Some(projected) = self.app.0.projector.project(&row, &ctx) {
                let rest = payload_rest(&pushes::timeline_row(&self.c, &projected, &epoch));
                for conn in &subscribers {
                    conn.send_sub("agent_stream", &conn.timeline_sub(), &rest);
                }
            }
            self.cursor = row.seq_end;
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
