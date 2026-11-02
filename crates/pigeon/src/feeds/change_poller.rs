//! `ChangePoller(person)` (spec 8.2): one long-poll of magpie's person change feed (M12) per person with a socket on
//! this replica (and 30 s after the last one closes), fanning each change out to that person's sockets: list
//! upserts and removes to list subscribers (7.6.4), turn boundaries and history rewrites through the conversation's
//! timeline poller (8.4, 7.6.3), the sandbox chip to every socket (7.6.5), asks (7.5.9).
//!
//! Joins (8.3) are handled in the poller's own task, between polls, so a socket's catch-up and the live changes
//! after it reach the socket in order: a snapshot taken at head `H` while the poller is at `P >= H` gets the
//! changes `H < seq <= P` fetched once for it; one taken at `H > P` ignores live changes up to `H`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use magpie_api::changes::{After, Change, ChangeBody, ChangesQuery, ChangesResponse};
use magpie_api::client::ClientError;
use magpie_api::errors::ErrorCode;
use magpie_api::turns::TurnPhase;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{Backoff, Hub, timeline_poller};
use crate::App;
use crate::paseo::dispatch::view_parts;
use crate::paseo::mapping::View;
use crate::paseo::messages::{payload_rest, session};
use crate::paseo::pushes;
use crate::paseo::transport::{Conn, ListKind};

/// The poller's control messages.
#[derive(Debug)]
pub enum Ctl {
    /// A list subscription took its snapshot at `head`.
    Join {
        /// The socket.
        conn: Arc<Conn>,
        /// Which list.
        kind: ListKind,
        /// The snapshot's `headSeq`.
        head: u64,
    },
}

const PAGE: u32 = 500;

struct Poller {
    hub: Hub,
    app: App,
    person: String,
    pos: u64,
    generation: String,
    /// Conversations this poller pushed `preparing` for (a turn end then sends `ready`, 7.6.5).
    preparing: HashSet<Uuid>,
}

async fn fetch(app: &App, person: &str, after: After, wait: bool) -> Result<ChangesResponse, ClientError> {
    let q = ChangesQuery {
        after,
        limit: Some(PAGE),
        wait_ms: wait.then(|| u64::try_from(app.0.tuning.poll_wait.as_millis()).unwrap_or(25_000)),
    };
    let client = if wait { &app.0.magpie_poll } else { &app.0.magpie };
    client
        .person(person)
        .with_request_id(Uuid::now_v7().to_string())
        .changes(&q)
        .await
}

/// Runs until the person has had no socket here for the linger time, or the drain.
pub async fn run(hub: Hub, app: App, person: String, mut ctl: mpsc::UnboundedReceiver<Ctl>) {
    let tuning = app.0.tuning.clone();
    let mut backoff = Backoff::new(tuning.poll_backoff_min, tuning.poll_backoff_max);
    let mut idle_since: Option<Instant> = None;
    // the start position: the current head
    let start = loop {
        match fetch(&app, &person, After::Latest, false).await {
            Ok(r) => break r,
            Err(e) => {
                tracing::warn!(error = %e, "change feed: head not read");
                if hub.change_poller_done(&person, &mut idle_since, tuning.poller_linger) {
                    return;
                }
                tokio::time::sleep(backoff.wait()).await;
            }
        }
    };
    backoff.reset();
    let mut p = Poller {
        hub,
        app,
        person,
        pos: start.head_seq,
        generation: start.generation,
        preparing: HashSet::new(),
    };
    let mut poll = Box::pin(fetch_owned(p.app.clone(), p.person.clone(), p.pos));
    let mut tick = tokio::time::interval(
        tuning
            .poller_linger
            .min(std::time::Duration::from_secs(1))
            .max(std::time::Duration::from_millis(20)),
    );
    loop {
        tokio::select! {
            c = ctl.recv() => match c {
                Some(Ctl::Join { conn, kind, head }) => p.join(&conn, kind, head).await,
                None => return,
            },
            r = &mut poll => {
                match r {
                    Ok(page) => {
                        backoff.reset();
                        p.deliver(&page);
                    }
                    Err(e) if e.code() == Some(&ErrorCode::CursorExpired) => {
                        // after a long outage: restart at the head, and make every socket re-snapshot
                        tracing::warn!("change feed: cursor expired; sockets re-snapshot");
                        if let Ok(r) = fetch(&p.app, &p.person, After::Latest, false).await {
                            p.pos = r.head_seq;
                        }
                        for conn in p.hub.conns_of(&p.person) {
                            conn.close(1012, "resync");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "change feed poll failed");
                        tokio::time::sleep(backoff.wait()).await;
                    }
                }
                poll = Box::pin(fetch_owned(p.app.clone(), p.person.clone(), p.pos));
            }
            _ = tick.tick() => {
                if p.hub.change_poller_done(&p.person, &mut idle_since, tuning.poller_linger) {
                    return;
                }
            }
        }
    }
}

async fn fetch_owned(app: App, person: String, pos: u64) -> Result<ChangesResponse, ClientError> {
    fetch(&app, &person, After::Seq(pos), true).await
}

impl Poller {
    fn view_parts(&self) -> (String, crate::paseo::strings::Locale, Option<String>) {
        view_parts(&self.app)
    }

    async fn join(&mut self, conn: &Arc<Conn>, kind: ListKind, head: u64) {
        if head >= self.pos {
            conn.set_floor(kind, head);
            return;
        }
        // catch up (head, pos] for this socket only
        conn.set_floor(kind, head);
        let mut after = head;
        while after < self.pos {
            match fetch(&self.app, &self.person, After::Seq(after), false).await {
                Ok(page) => {
                    let (product, locale, model) = self.view_parts();
                    let view = View {
                        product: &product,
                        locale,
                        model: model.as_deref(),
                    };
                    let mut last = after;
                    for ch in page.changes.iter().filter(|c| c.seq > after && c.seq <= self.pos) {
                        self.list_change(std::slice::from_ref(conn), ch, &view, Some(kind));
                        last = ch.seq;
                    }
                    if page.changes.is_empty() || last == after {
                        break;
                    }
                    after = last;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "change feed catch-up failed; the socket re-snapshots");
                    conn.close(1012, "resync");
                    return;
                }
            }
        }
        conn.set_floor(kind, self.pos);
    }

    fn deliver(&mut self, page: &ChangesResponse) {
        if page.generation != self.generation {
            self.generation.clone_from(&page.generation);
        }
        let conns = self.hub.conns_of(&self.person);
        let (product, locale, model) = self.view_parts();
        let view = View {
            product: &product,
            locale,
            model: model.as_deref(),
        };
        for ch in &page.changes {
            if ch.seq <= self.pos {
                continue;
            }
            self.change(&conns, ch, &view);
            self.pos = ch.seq;
        }
        if page.changes.is_empty() && page.head_seq > self.pos {
            self.pos = page.head_seq;
        }
    }

    /// A list change (upsert or remove) to the sockets whose subscription takes it; `only` limits it to one list
    /// (a join's catch-up).
    fn list_change(&self, conns: &[Arc<Conn>], ch: &Change, view: &View<'_>, only: Option<ListKind>) {
        let g = &self.generation;
        let (agent, workspace) = match &ch.body {
            ChangeBody::ConversationUpserted { conversation } if conversation.deleted_at.is_none() => (
                pushes::agent_upsert(conversation, view, g, ch.seq),
                pushes::workspace_upsert(conversation, view, g, ch.seq),
            ),
            ChangeBody::ConversationUpserted { .. } | ChangeBody::ConversationRemoved { .. } => (
                pushes::agent_remove(&ch.conversation_id, g, ch.seq),
                pushes::workspace_remove(&ch.conversation_id, g, ch.seq),
            ),
            _ => return,
        };
        let (agent, workspace) = (payload_rest(&agent), payload_rest(&workspace));
        for conn in conns {
            for kind in [ListKind::Agents, ListKind::Workspaces] {
                if only.is_some_and(|o| o != kind) {
                    continue;
                }
                let Some(sub) = conn.list_sub(kind) else {
                    continue;
                };
                if conn.floor(kind).is_none_or(|f| ch.seq <= f) {
                    continue;
                }
                match kind {
                    ListKind::Agents => conn.send_sub("agent_update", &sub, &agent),
                    ListKind::Workspaces => conn.send_sub("workspace_update", &sub, &workspace),
                };
            }
        }
    }

    fn change(&mut self, conns: &[Arc<Conn>], ch: &Change, view: &View<'_>) {
        let c = ch.conversation_id;
        match &ch.body {
            ChangeBody::ConversationUpserted { .. } | ChangeBody::ConversationRemoved { .. } => {
                self.list_change(conns, ch, view, None);
            }
            ChangeBody::Turn(t) => {
                let ended = t.phase != TurnPhase::Started;
                self.hub.to_timeline(
                    &self.person,
                    c,
                    timeline_poller::Ctl::Turn {
                        turn: t.clone(),
                        received: Instant::now(),
                    },
                );
                if ended && self.preparing.remove(&c) {
                    let f = session(
                        crate::paseo::messages::sandbox_status_type(),
                        &pushes::sandbox_status(&c, "ready"),
                    );
                    for conn in conns {
                        conn.send(f.clone());
                    }
                }
            }
            ChangeBody::Sandbox { state } => {
                let state = match state {
                    magpie_api::conversations::SandboxPhase::Preparing => {
                        self.preparing.insert(c);
                        "preparing"
                    }
                    magpie_api::conversations::SandboxPhase::Ready => {
                        self.preparing.remove(&c);
                        "ready"
                    }
                };
                let f = session(
                    crate::paseo::messages::sandbox_status_type(),
                    &pushes::sandbox_status(&c, state),
                );
                for conn in conns {
                    conn.send(f.clone());
                }
            }
            ChangeBody::Ask { ask } => {
                let f = session("agent_permission_request", &pushes::permission(&c, ask));
                for conn in conns.iter().filter(|k| k.watches(&c) || k.lists()) {
                    conn.send(f.clone());
                }
            }
            ChangeBody::AskResolved(r) => {
                let f = session(
                    "agent_permission_resolved",
                    &pushes::permission_resolved(&c, &r.ask_id, &r.resolution),
                );
                for conn in conns.iter().filter(|k| k.watches(&c) || k.lists()) {
                    conn.send(f.clone());
                }
            }
            ChangeBody::TimelineReplaced { epoch } => {
                self.hub
                    .to_timeline(&self.person, c, timeline_poller::Ctl::Replaced { epoch: epoch.clone() });
            }
            ChangeBody::Unknown => {}
        }
    }
}
