//! A legacy conversation's socket requests (DESIGN 9.2-9.5): reads through the LU as they are; writes under a
//! magpie ticket that closes only on the old stack's outcome (WP-51's contract, `magpie_api::legacy`):
//!
//! - every write takes a ticket first; `native` (magpie owns the conversation now, the fence won) sends it natively,
//!   `legacy` sends it through the LU, and the old child's answer is the ticket's outcome (a send: `started` when it
//!   accepted, since the old hub starts the turn before it answers; `refused` otherwise; anything else `applied` or
//!   `refused`), reported with the answer's `take`;
//! - a send's ticket call carries its `clientMessageId` (a retry gets the open ticket, possibly while another request
//!   still holds it); no write starts later than the answer's `holdMs` after pigeon SENT the ticket call (its
//!   monotonic clock): past that, a new ticket; every write is given up after `TICKET_WRITE_TIMEOUT` (20 s) without
//!   the old child's answer, carries that bound as the instant past which the old child refuses it
//!   ([`super::OLD_NOT_AFTER`], [`Lu::write`]), and the ticket is left for the importer to expire;
//! - a send's write is claimed under its take right before it starts, so of two requests holding one ticket only one
//!   writes; it starts only when the claim's answer came within [`CLAIM_ANSWER_MAX`] of pigeon sending it and before
//!   the hold (magpie stamps the claim when it takes it, and the claim's settle time counts from then), otherwise the
//!   hand-out closes `refused` with nothing written and a new ticket is taken; a request that gives up without
//!   writing reports `refused` under its own take, which closes nothing once another request holds the ticket or
//!   while another take's claim stands; a request whose claim answers that its resend holds the ticket answers its
//!   tab nothing (the resend's answer is the send's);
//! - the old child does not de-duplicate a send by `clientMessageId` (a second send is a second turn), so a send
//!   whose answer names an earlier hand-out (`reused`), an open earlier ticket (`supersedes`) or an earlier ticket
//!   (`prior`) is checked first: a `prior` the old child accepted is answered `accepted` with nothing sent; otherwise
//!   pigeon reads the old child's timeline (an early read, then once every earlier write has settled, `settleMs` from
//!   when it sent the call) and sends only when the send's user row is not there. When it is, the ticket is
//!   reported from what the old child shows and the send is answered as accepted; when the old child cannot be read,
//!   nothing is sent. A row that is not there is sent: the old hub refuses it itself while a turn runs. The read
//!   pages back through the whole old timeline (a timeline longer than pigeon reads, or one that changes epoch
//!   between pages, cannot be read);
//! - a send whose LU drops (or times out) with its write in flight, under a tab that stays, is settled the same way
//!   through the reopened LU, read once the write's whole claim settle time has passed (by then the old child has
//!   applied it or refuses it): accepted when the old child has it, written again within the hold when it does not;
//!   under a tab whose LU is not reopened (DESIGN 9.5's E4) the write is left unanswered with its ticket open, and the
//!   tab's resend after its 1012 reconnect is settled from the old timeline;
//! - a write made after such a check (a repeat, an LU drop) that the old hub refuses may be refused because an
//!   earlier write of the same send landed late and is the turn that runs: the old child is read once more, and when
//!   it has the send the ticket is reported from what it shows and the send is answered as accepted;
//! - a send of magpie-only files copies each into the old child first, each under an upload ticket of its own (a
//!   new one when its hold ran out), once for the whole send, then sends with the old paths under the send's ticket;
//!   a copy may run past its upload ticket's settle time ([`copy_bound`], sized by the file, up to 120 s per file and
//!   ten files). The send's ticket is not what keeps that safe: it is unclaimed while the files are copied, so the
//!   importer may expire it `TICKET_SETTLE_MAX` (90 s) after its issue and release the conversation mid-copy. What
//!   keeps it safe is the hold: a send whose ticket's hold passed during the copies takes a new ticket before it
//!   writes, which answers `native` once the conversation is released, and the send goes natively with the tab's own
//!   message (every path magpie's); a copy that lands after a release is a file in the old asset folder nothing
//!   refers to;
//! - an outcome is `refused` only when the old route itself answered no (a 4xx, the old child's own refusal) or
//!   nothing was written; a 5xx, a gateway error or no answer may come after the write was applied, so the ticket is
//!   left for the importer to expire after its settle read;
//! - a socket with a ticketed write in flight counts as a transfer for the drain (its 1012 waits for the write, up
//!   to the transfer bound), an outcome still being reported in the background counts as one of the process (the
//!   drain waits for it before the process exits), and every write the switch fails counts in
//!   `pigeon_switch_write_refused_total`;
//! - a holder with no unexpired token on this replica (a tab that outlived its Access token) gets the signed-out
//!   answer, never a write error; an LU that does not open still takes the ticket (magpie may own the conversation
//!   by now), and only a `legacy` answer reports the LU's failure.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use magpie_api::client::ClientError;
use magpie_api::legacy::{
    TicketClaim, TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketResponse, TicketRoute,
};
use serde_json::{Value, json};
use uuid::Uuid;

use super::lu::{Lu, LuError};
use super::upstream::CallError;
use super::{CLAIM_ANSWER_MAX, Legacy, OLD_FILE_MAX, OLD_USER_ROW_PREFIX, READ_TIMEOUT};
use crate::feeds::{BoxFuture, Delegate};
use crate::paseo::dispatch;
use crate::paseo::messages::{rpc_error, str_field};
use crate::paseo::strings::{self, Locale};
use crate::paseo::transport::Conn;

/// Tickets taken for one write at most (each past its hold is closed `refused` and another taken).
const MAX_TICKETS: usize = 4;

/// Writes under one ticket at most (a send whose LU dropped mid-write, found absent, is written again).
const MAX_WRITES: usize = 3;

/// One page of a repeat's read of the old timeline (the old hub's page limit).
const OLD_TAIL: u64 = 500;

/// Pages a repeat's read covers at most (a longer timeline cannot be read: nothing is sent).
const OLD_PAGES: usize = 40;

/// The longest a copy-at-send reads one upload's bytes from magpie.
const COPY_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How long after an LU drop pigeon waits before it asks for the reopened LU (past a blip, as `lu_ended` does).
const LU_REOPEN_WAIT: Duration = Duration::from_millis(400);

/// The legacy delegate installed for every legacy conversation.
pub(crate) struct LegacyDelegate(pub(crate) Legacy);

impl Delegate for LegacyDelegate {
    fn request(&self, socket: Arc<Conn>, kind: String, message: Value) -> BoxFuture {
        let l = self.0.clone();
        Box::pin(async move { l.socket_request(socket, kind, message).await })
    }

    fn subscribe(&self, socket: Arc<Conn>, conversation: Uuid) {
        self.0.watch(&socket, conversation);
    }

    fn unsubscribe(&self, socket: &Arc<Conn>, conversation: Uuid) {
        if let Some(lu) = self.0.lu_of(socket.id) {
            lu.unwatch(conversation);
        }
    }
}

/// The ticket a write takes (`None`: a read, sent as it is).
fn ticket_kind(kind: &str, deleted: bool) -> Option<TicketKind> {
    Some(match kind {
        "send_agent_message_request" => TicketKind::Send,
        "cancel_agent_request" => TicketKind::Stop,
        "update_agent_request" | "workspace.title.set.request" => TicketKind::Rename,
        "delete_agent_request" if deleted => TicketKind::Purge,
        "archive_agent_request" | "archive_workspace_request" | "delete_agent_request" => TicketKind::Delete,
        // a refresh restores a deleted conversation and otherwise reads; pigeon's view of Recently deleted can lag
        // the old child, and a ticket on a read is harmless where a missing one on a write is not
        "refresh_agent_request" => TicketKind::Restore,
        _ => return None,
    })
}

/// Whether a conversation-scoped socket request is a write (it takes a ticket for a legacy conversation).
#[must_use]
pub fn is_write(kind: &str) -> bool {
    ticket_kind(kind, false).is_some() || kind == "agent_permission_response"
}

/// The old child's answer as a ticket outcome.
fn outcome(kind: TicketKind, typ: &str, p: &Value) -> TicketOutcome {
    let refused = typ == "rpc_error"
        || p.get("accepted") == Some(&Value::Bool(false))
        || (p.get("accepted").is_none() && p.get("error").is_some_and(|e| !e.is_null()));
    match (refused, kind) {
        (true, _) => TicketOutcome::Refused,
        (false, TicketKind::Send) => TicketOutcome::Started,
        (false, _) => TicketOutcome::Applied,
    }
}

/// Whether `v` holds the old timeline's user message of `cmid`.
fn holds_send(v: &Value, cmid: &str) -> bool {
    match v {
        Value::Object(o) => {
            o.get("clientMessageId").and_then(Value::as_str) == Some(cmid)
                || o.get("id").and_then(Value::as_str) == Some(&format!("{OLD_USER_ROW_PREFIX}{cmid}"))
                || o.values().any(|x| holds_send(x, cmid))
        }
        Value::Array(a) => a.iter().any(|x| holds_send(x, cmid)),
        _ => false,
    }
}

/// The `messageId` of a send the tab sent without one: stable for the same request (a resend of it on another
/// socket carries the same request id and text), so its ticket's reuse and the timeline check still match it; a
/// random one when there is no request id to tie it to (the tab cannot match an answer then either). The paseo
/// client always sets one (`daemon-client.ts` `sendAgentMessage`).
fn stable_message_id(c: Uuid, rid: &str, m: &Value) -> String {
    if rid.is_empty() {
        return Uuid::new_v4().to_string();
    }
    let text = str_field(m, "text").unwrap_or("");
    Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("pigeon-send\n{c}\n{rid}\n{text}").as_bytes(),
    )
    .to_string()
}

/// How a copy-at-send ended other than with every file in the old child.
enum Copy {
    /// An upload ticket answered `native`: send natively with the magpie paths.
    Native,
    /// The holder is signed out (no unexpired token, or the old route's 401): the signed-out answer.
    SignedOut,
    /// A copy failed (the sentence for the send's answer).
    Failed(String),
}

/// What the old child shows of a send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OldSend {
    /// It has it; whether its turn still runs.
    Present { running: bool },
    /// It does not.
    Absent,
}

/// One hand-out of a ticket: what a write under it is reported and claimed with.
#[derive(Debug, Clone, Copy)]
struct Held {
    ticket: Uuid,
    take: Option<Uuid>,
    /// No write starts past this (pigeon's clock: when it sent the ticket call, plus `holdMs`).
    hold: Instant,
}

/// A claim's answer.
enum Claim {
    /// The write may start now.
    Claimed,
    /// Another request holds the ticket now and writes.
    Elsewhere,
    /// The ticket is closed or past its hold, or the send was accepted under another ticket: a new ticket.
    Closed,
    /// magpie did not answer.
    Unknown,
}

/// Whether an old HTTP refusal is the old route's own answer (a 4xx): anything else may follow an applied write.
pub(crate) fn refused_by_old(e: &CallError) -> bool {
    matches!(e, CallError::Status(s, _) if (400..500).contains(s)) || *e == CallError::SignedOut
}

impl Legacy {
    fn locale(&self) -> Locale {
        Locale::from_zh(self.app().is_some_and(|a| a.identities().is_zh()))
    }

    /// One conversation-scoped request of a legacy conversation.
    pub(crate) async fn socket_request(&self, conn: Arc<Conn>, kind: String, mut m: Value) {
        let rid = str_field(&m, "requestId").unwrap_or("").to_owned();
        let Some(c) = dispatch::conversation_of(&kind, &m) else {
            return;
        };
        match ticket_kind(&kind, self.is_deleted(&c)) {
            None => self.forward(&conn, &kind, m, &rid).await,
            Some(tk) => {
                if tk == TicketKind::Send && str_field(&m, "messageId").is_none_or(str::is_empty) {
                    // the old child keys the user row by it, and the ticket's retry by it
                    m["messageId"] = json!(stable_message_id(c, &rid, &m));
                }
                self.ticketed(&conn, &kind, c, m, &rid, tk).await;
            }
        }
    }

    /// A read (or an ask answer, which the old stack does not take): through the LU as it is.
    async fn forward(&self, conn: &Arc<Conn>, kind: &str, m: Value, rid: &str) {
        let lu = self.lu_for(conn);
        match lu.call(m, READ_TIMEOUT).await {
            Ok((typ, p)) => answer(conn, &typ, p, rid),
            Err(e) => self.lu_failed(conn, kind, rid, e),
        }
    }

    /// An LU call that has no answer: the signed-out answer for a 401 (the socket's 4401), otherwise an error.
    fn lu_failed(&self, conn: &Arc<Conn>, kind: &str, rid: &str, e: LuError) {
        if e == LuError::SignedOut {
            conn.close(4401, "signed out");
            return;
        }
        if self.reconnecting(conn) {
            // the tab is about to be closed 1012 and resends the request after it reconnects: no error for it
            tracing::info!(error = ?e, kind, "LU call failed under a tab that reconnects: left unanswered");
            return;
        }
        tracing::warn!(error = ?e, kind, "LU call failed");
        self.0.metrics.lu_errors.inc();
        if !rid.is_empty() {
            conn.reply(
                "rpc_error",
                &rpc_error(rid, kind, self.locale().unreachable(), "failed"),
            );
        }
    }

    async fn take(
        &self,
        conn: &Conn,
        c: Uuid,
        kind: TicketKind,
        cmid: Option<String>,
    ) -> Result<(TicketResponse, Instant), ClientError> {
        let app = self.app().ok_or_else(|| ClientError::Decode {
            route: "legacy_ticket",
            error: "pigeon is not started".into(),
        })?;
        let sent = Instant::now();
        let t = conn
            .calls(&app.0.magpie_turns)
            .legacy_ticket(
                c,
                &TicketRequest {
                    kind,
                    client_message_id: cmid,
                },
            )
            .await?;
        Ok((t, sent))
    }

    /// Reports a ticket's outcome under its take (retried in the background: until it lands the importer keeps the
    /// conversation legacy, which only delays its release).
    fn report(&self, conn: &Arc<Conn>, h: Held, outcome: TicketOutcome) {
        let (me, conn) = (self.clone(), conn.clone());
        // a transfer of the process, which the drain waits for before it exits (up to the transfer bound): an
        // outcome lost at exit would leave the ticket to expire, the conversation held legacy meanwhile
        let transfer = self.app().map(|a| a.transfers().guard());
        tokio::spawn(async move {
            me.report_now(&conn, h, outcome).await;
            drop(transfer);
        });
    }

    /// [`Self::report`], waited for; `false` when it did not land.
    async fn report_now(&self, conn: &Conn, h: Held, outcome: TicketOutcome) -> bool {
        let Some(app) = self.app().cloned() else { return false };
        let mut wait = Duration::from_millis(250);
        for _ in 0..8 {
            match conn
                .calls(&app.0.magpie)
                .legacy_ticket_done(h.ticket, &TicketDone { outcome, take: h.take })
                .await
            {
                Ok(()) => return true,
                Err(e) if e.api().is_some_and(|r| r.status < 500 && r.status != 429) => {
                    tracing::warn!(error = %e, "ticket outcome refused");
                    return false;
                }
                Err(e) => tracing::warn!(error = %e, "ticket outcome not reported; retrying"),
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(Duration::from_secs(5));
        }
        false
    }

    /// Claims a send's write under `h` (the write starts at once after [`Claim::Claimed`]).
    async fn claim(&self, conn: &Conn, h: Held) -> Claim {
        let (Some(app), Some(take)) = (self.app().cloned(), h.take) else {
            return Claim::Unknown;
        };
        match conn
            .calls(&app.0.magpie_turns)
            .legacy_ticket_claim(h.ticket, &TicketClaim { take })
            .await
        {
            Ok(a) if a.claimed => Claim::Claimed,
            Ok(a) if a.held_elsewhere => Claim::Elsewhere,
            Ok(_) => Claim::Closed,
            Err(e) => {
                tracing::warn!(error = %e, "write not claimed");
                Claim::Unknown
            }
        }
    }

    /// A write the switch could not make: counted, and the tab's error (the signed-out answer for a 401). Under a tab
    /// that is about to be closed 1012 to reconnect it is neither: the tab resends it after the reconnect (a write
    /// that may have reached the old child left its ticket open, so the resend's settled read finds it).
    fn write_lost(&self, conn: &Arc<Conn>, kind: &str, rid: &str, e: LuError) {
        if e != LuError::SignedOut && !self.reconnecting(conn) {
            self.0.metrics.write_refused.inc();
        }
        self.lu_failed(conn, kind, rid, e);
    }

    /// A send the old child already has: its ticket reported from what it shows, the tab answered as accepted.
    fn already_sent(&self, conn: &Arc<Conn>, h: Held, c: Uuid, rid: &str, running: bool) {
        let o = if running {
            TicketOutcome::Started
        } else {
            TicketOutcome::Completed
        };
        self.report(conn, h, o);
        tracing::info!(conversation = %c, "a repeated send had reached the old child: not sent again");
        let p = json!({"agentId": c, "accepted": true, "error": null});
        answer(conn, "send_agent_message_response", p, rid);
    }

    /// A write: ticket, then (a send) its repeat check and the copies of its magpie-only files, then the claim and
    /// the write through the LU.
    #[allow(clippy::many_single_char_names)] // c, m, t, o, p, h: the conversation, message, ticket, outcome, payload
    #[allow(clippy::too_many_lines)] // one write's steps in order: ticket, repeat check, copies, hold, claim, write
    async fn ticketed(&self, conn: &Arc<Conn>, kind: &str, c: Uuid, m: Value, rid: &str, tk: TicketKind) {
        let Some(app) = self.app().cloned() else { return };
        // the drain's 1012 waits for this write (its ticket stays open otherwise and the tab's resend meets it)
        let _transfer = conn.transfer();
        let cmid = (tk == TicketKind::Send).then(|| str_field(&m, "messageId").unwrap_or("").to_owned());
        let mut lu = self.lu_for(conn);
        // not closed as unused between this write's calls
        let mut _held = lu.hold();
        // the LU is open before any ticket, so its upgrade does not eat into a hold; one that does not open still
        // takes the ticket (magpie may own the conversation by now: a native write needs no LU)
        let lu_down = match lu.ready().await {
            Ok(()) => None,
            Err(LuError::SignedOut) => return self.write_lost(conn, kind, rid, LuError::SignedOut),
            Err(e) => Some(e),
        };
        let write_timeout = self.0.write_timeout;
        // the tab's message stays as it is (every native fallback sends it); the LU send carries the old paths, copied
        // once for the whole send
        let mut out = m.clone();
        let mut copies = Copies::default();
        // a write after a check that an earlier write of this send may have landed (a repeat, an LU drop)
        let mut again = false;
        'tickets: for _ in 0..MAX_TICKETS {
            let (t, sent) = match self.take(conn, c, tk, cmid.clone()).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, kind, "ticket not taken");
                    let not_found = e.api().is_some_and(|r| r.status == 404);
                    if !not_found {
                        self.0.metrics.write_refused.inc();
                    }
                    return self.write_failed(conn, kind, rid, not_found);
                }
            };
            let h = match (t.route, t.ticket) {
                (TicketRoute::Native, _) => {
                    // magpie owns it now: a native write (the release's replacement comes with the owner change)
                    self.mark(&conn.person, c, false);
                    let hub = app.hub().unwrap_or_default();
                    return dispatch::native(&app, &hub, conn, kind.to_owned(), m).await;
                }
                (TicketRoute::Legacy, Some(ticket)) => Held {
                    ticket,
                    take: t.take,
                    hold: sent + Duration::from_millis(t.hold_ms.unwrap_or(0)),
                },
                (TicketRoute::Legacy, None) => {
                    self.0.metrics.write_refused.inc();
                    return self.write_failed(conn, kind, rid, false);
                }
            };
            if let Some(e) = lu_down {
                if t.prior.as_ref().is_some_and(|p| p.accepted) {
                    // the old child took this send under an earlier ticket: accepted, nothing to write or read
                    self.report(conn, h, TicketOutcome::Refused);
                    let p = json!({"agentId": c, "accepted": true, "error": null});
                    return answer(conn, "send_agent_message_response", p, rid);
                }
                // the old stack owns it and cannot be reached: nothing written under this hand-out. Its `refused`
                // closes nothing an earlier holder may have written under (magpie keeps a ticket an earlier take
                // claimed open, and a reused ticket's earlier take can no longer claim; `prior` and `supersedes` are
                // other tickets), so the next resend still checks the old timeline
                self.report(conn, h, TicketOutcome::Refused);
                return self.write_lost(conn, kind, rid, e);
            }
            if let Some(cmid) = &cmid {
                if t.prior.as_ref().is_some_and(|p| p.accepted) {
                    // the old child took this send under an earlier ticket: never sent twice, nothing written here
                    self.report(conn, h, TicketOutcome::Refused);
                    tracing::info!(conversation = %c, "a send the old child already accepted: not sent again");
                    let p = json!({"agentId": c, "accepted": true, "error": null});
                    return answer(conn, "send_agent_message_response", p, rid);
                }
                if t.reused || t.supersedes.is_some() || t.prior.is_some() {
                    // an earlier write of this send may have reached the old child: read it once they have settled
                    let settle = t
                        .settle_ms
                        .unwrap_or(0)
                        .max(t.supersedes.as_ref().map_or(0, |s| s.settle_ms));
                    match self
                        .settled_read(&lu, c, cmid, sent + Duration::from_millis(settle))
                        .await
                    {
                        Some(OldSend::Present { running }) => return self.already_sent(conn, h, c, rid, running),
                        Some(OldSend::Absent) => again = true,
                        // the old child cannot be read: nothing is sent (a send twice is a second turn), and the
                        // ticket stays open (an earlier write may still land)
                        None => return self.write_lost(conn, kind, rid, LuError::NotSent),
                    }
                }
            }
            if tk == TicketKind::Send {
                match self.copy_files(conn, c, &mut out, &mut copies).await {
                    Ok(()) => {}
                    Err(Copy::Native) => {
                        self.report(conn, h, TicketOutcome::Refused);
                        self.mark(&conn.person, c, false);
                        let hub = app.hub().unwrap_or_default();
                        return dispatch::native(&app, &hub, conn, kind.to_owned(), m).await;
                    }
                    Err(Copy::SignedOut) => {
                        self.report(conn, h, TicketOutcome::Refused);
                        return self.write_lost(conn, kind, rid, LuError::SignedOut);
                    }
                    Err(Copy::Failed(sentence)) => {
                        self.report(conn, h, TicketOutcome::Refused);
                        self.0.metrics.write_refused.inc();
                        let p = json!({"agentId": c, "accepted": false, "error": sentence});
                        return answer(conn, "send_agent_message_response", p, rid);
                    }
                }
            }
            for _ in 0..MAX_WRITES {
                if conn.is_closing() {
                    // no write for a tab that has gone (its resend may already be on its way): this hand-out closes
                    // with nothing written (a refused that changes nothing once the resend holds the ticket)
                    self.report(conn, h, TicketOutcome::Refused);
                    return;
                }
                if Instant::now() >= h.hold {
                    // no write starts past the hold: this hand-out closes with nothing written, before another is
                    // taken (a send's next ticket call would otherwise get this one back, open)
                    if !self.report_now(conn, h, TicketOutcome::Refused).await {
                        self.0.metrics.write_refused.inc();
                        return self.write_failed(conn, kind, rid, false);
                    }
                    continue 'tickets;
                }
                if tk == TicketKind::Send {
                    let asked = Instant::now();
                    match self.claim(conn, h).await {
                        Claim::Claimed if asked.elapsed() > CLAIM_ANSWER_MAX || Instant::now() >= h.hold => {
                            // a late answer: the claim's settle time counts from when magpie took it, so a write
                            // started now could land after it (or after the hold); this hand-out closes with nothing
                            // written and the write goes under a new ticket
                            tracing::warn!(conversation = %c, after = ?asked.elapsed(),
                                           "a send's claim answered late: not written under it");
                            if !self.report_now(conn, h, TicketOutcome::Refused).await {
                                self.0.metrics.write_refused.inc();
                                return self.write_failed(conn, kind, rid, false);
                            }
                            continue 'tickets;
                        }
                        Claim::Claimed => {}
                        Claim::Elsewhere => {
                            // the tab's resend holds the ticket now and writes it, and its answer is the send's: this
                            // request answers nothing (an error here would show a failed send that is delivered)
                            tracing::info!(conversation = %c, "a send's ticket was handed to its resend");
                            return;
                        }
                        Claim::Closed => continue 'tickets,
                        Claim::Unknown => {
                            // nothing was written under it
                            if !self.report_now(conn, h, TicketOutcome::Refused).await {
                                self.0.metrics.write_refused.inc();
                                return self.write_failed(conn, kind, rid, false);
                            }
                            continue 'tickets;
                        }
                    }
                }
                let started = Instant::now();
                match lu.write(out.clone(), write_timeout).await {
                    Ok((typ, p)) => {
                        let o = outcome(tk, &typ, &p);
                        if o == TicketOutcome::Refused
                            && again
                            && let Some(cmid) = &cmid
                        {
                            // the old hub's busy refusal may be for this send's own earlier write, landed late
                            match self.old_has_send(&lu, c, cmid).await {
                                Some(OldSend::Present { running }) => {
                                    return self.already_sent(conn, h, c, rid, running);
                                }
                                Some(OldSend::Absent) => {}
                                None => {
                                    // it cannot be told: the ticket stays for the importer (an earlier write may
                                    // have landed), and the tab gets the old hub's answer (its resend is checked)
                                    tracing::warn!(conversation = %c, "a repeated send refused, the old child unread");
                                    return answer(conn, &typ, p, rid);
                                }
                            }
                        }
                        self.report(conn, h, o);
                        if o == TicketOutcome::Applied {
                            match tk {
                                TicketKind::Delete => self.set_deleted(&c, true),
                                TicketKind::Restore => self.set_deleted(&c, false),
                                TicketKind::Purge => self.forget(&c),
                                _ => {}
                            }
                        }
                        return answer(conn, &typ, p, rid);
                    }
                    Err(e) if !e.maybe_sent() => {
                        // nothing went out
                        self.report(conn, h, TicketOutcome::Refused);
                        return self.write_lost(conn, kind, rid, e);
                    }
                    Err(e) => {
                        let Some(cmid) = cmid.as_deref().filter(|_| !conn.is_closing()) else {
                            // it may have been applied: the importer expires the ticket after its settle read
                            return self.write_lost(conn, kind, rid, e);
                        };
                        // the LU dropped or timed out with the send in flight, under a tab that stays: the reopened
                        // LU reads the old child once the write has settled, and the answer comes from what it shows
                        tracing::info!(error = ?e, conversation = %c, "a send's LU write has no answer: settling it");
                        tokio::time::sleep(LU_REOPEN_WAIT).await;
                        lu = self.lu_for(conn);
                        _held = lu.hold();
                        if let Err(e) = lu.ready().await {
                            return self.write_lost(conn, kind, rid, e);
                        }
                        // the write carried its bound as the instant past which the old child refuses it (it may
                        // still be inside the old hub's steps before the user row, or held by a hop between): the
                        // claim's whole settle time, never less
                        let until = started + self.0.settle;
                        match self.settled_read(&lu, c, cmid, until).await {
                            Some(OldSend::Present { running }) => {
                                self.report(
                                    conn,
                                    h,
                                    if running {
                                        TicketOutcome::Started
                                    } else {
                                        TicketOutcome::Completed
                                    },
                                );
                                let p = json!({"agentId": c, "accepted": true, "error": null});
                                return answer(conn, "send_agent_message_response", p, rid);
                            }
                            // it never arrived: written again (claimed again) within the hold, else under a new ticket
                            Some(OldSend::Absent) => again = true,
                            None => return self.write_lost(conn, kind, rid, e),
                        }
                    }
                }
            }
            break;
        }
        tracing::warn!(kind, "no ticket held long enough for the write");
        self.0.metrics.write_refused.inc();
        self.write_failed(conn, kind, rid, false);
    }

    /// A write that could not go anywhere (no ticket): the tab's error for it.
    fn write_failed(&self, conn: &Arc<Conn>, kind: &str, rid: &str, not_found: bool) {
        let l = self.locale();
        let sentence = if not_found { strings::NOT_FOUND } else { l.unreachable() };
        if !rid.is_empty() {
            conn.reply("rpc_error", &rpc_error(rid, kind, sentence, "failed"));
        }
    }

    /// What the old child shows of the send of `cmid` once every earlier write of it has settled (`until`): a read
    /// at once (a send that is there is answered without waiting), then one at `until` (`None`: it cannot be read).
    async fn settled_read(&self, lu: &Lu, c: Uuid, cmid: &str, until: Instant) -> Option<OldSend> {
        if Instant::now() < until {
            if let Some(found @ OldSend::Present { .. }) = self.old_has_send(lu, c, cmid).await {
                return Some(found);
            }
            tokio::time::sleep_until(until.into()).await;
        }
        self.old_has_send(lu, c, cmid).await
    }

    /// What the old child shows of the send of `cmid` (`None`: it cannot be read). The old hub puts a send's user row
    /// (keyed by its `clientMessageId`) into the timeline before it answers, so the row is the evidence: the busy
    /// state is per conversation, not per message (`running` lists background work too), so a conversation that is
    /// busy with something else does not mean the send arrived. A row that is not there is sent, and the old hub
    /// itself refuses it while a turn runs. The read pages back to the timeline's start ([`OLD_PAGES`] pages of
    /// [`OLD_TAIL`] at most; beyond, or across an epoch change, `None`); a row the old child lost in a restart is not
    /// found (its turn died with that restart).
    async fn old_has_send(&self, lu: &Lu, c: Uuid, cmid: &str) -> Option<OldSend> {
        let mut ask = json!({"type": "fetch_agent_timeline_request", "agentId": c, "direction": "tail",
                             "limit": OLD_TAIL, "projection": "projected"});
        let mut tail: Option<Value> = None;
        let mut found = false;
        for _ in 0..OLD_PAGES {
            let (typ, page) = lu.call(ask, READ_TIMEOUT).await.ok()?;
            if typ == "rpc_error" || page.get("error").is_some_and(|e| !e.is_null()) {
                return None;
            }
            if tail.as_ref().is_some_and(|t| t["epoch"] != page["epoch"]) {
                // the old child rebuilt its timeline between pages
                return None;
            }
            found = holds_send(&page, cmid);
            let rows = page["entries"].as_array().map_or(0, Vec::len);
            let older = page["hasOlder"].as_bool().unwrap_or(rows as u64 >= OLD_TAIL);
            let start = page["startCursor"].clone();
            tail.get_or_insert(page);
            if found {
                break;
            }
            if !older {
                return Some(OldSend::Absent);
            }
            if !start.is_object() {
                return None;
            }
            ask = json!({"type": "fetch_agent_timeline_request", "agentId": c, "direction": "before",
                         "cursor": start, "limit": OLD_TAIL, "projection": "projected"});
        }
        if !found {
            // longer than pigeon reads: not known to be absent
            return None;
        }
        let page = tail.unwrap_or_default();
        // the old bridge's agent snapshot says `running` or `idle` (`paseo-connection.ts`); only for the outcome
        // reported, so an unreadable one is `completed` (either closes the ticket)
        let agent = json!({"type": "fetch_agent_request", "agentId": c});
        let running = match lu.call(agent, READ_TIMEOUT).await {
            Ok((_, a)) => a["agent"]["status"] == "running" || page["agent"]["status"] == "running",
            Err(_) => page["agent"]["status"] == "running",
        };
        Some(OldSend::Present { running })
    }

    /// The copy-at-send (DESIGN 9.5): each attachment that is a magpie upload (`uploads/<id>/…`) is written into the
    /// old child under an upload ticket of its own (a new one when its hold ran out before the write), and `m` (the
    /// LU send's own copy of the tab's message) carries its old path instead. Each file is copied once per send:
    /// `copies` keeps the old paths across the send's tickets.
    #[allow(clippy::many_single_char_names)] // c, m, t, r, b: the conversation, message, ticket, answer, bytes
    async fn copy_files(&self, conn: &Arc<Conn>, c: Uuid, m: &mut Value, copies: &mut Copies) -> Result<(), Copy> {
        let Some(app) = self.app().cloned() else {
            return Ok(());
        };
        let Some(list) = m.get_mut("attachments").and_then(Value::as_array_mut) else {
            return Ok(());
        };
        let failed = || Copy::Failed(self.locale().upload_failed().to_owned());
        for a in list.iter_mut() {
            let path = str_field(a, "path").unwrap_or("").to_owned();
            let Some(upload) = magpie_upload_id(&path) else {
                continue;
            };
            let old = if let Some(old) = copies.0.get(&upload) {
                old.clone()
            } else {
                let read = tokio::time::timeout(COPY_READ_TIMEOUT, async {
                    let r = conn
                        .calls(&app.0.magpie_transfer)
                        .upload_raw(upload)
                        .await
                        .map_err(|e| e.to_string())?;
                    let mime = r
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .and_then(super::upstream::clean_mime);
                    Ok::<_, String>((mime, r.bytes().await.map_err(|e| e.to_string())?))
                })
                .await
                .unwrap_or_else(|_| Err("not read in time".into()));
                let (stored, bytes) = match read {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = %e, "copy-at-send: the upload's bytes not read");
                        return Err(failed());
                    }
                };
                let name = str_field(a, "fileName")
                    .map_or_else(|| path.rsplit('/').next().unwrap_or("file").to_owned(), str::to_owned);
                // magpie's stored type first (the tab's goes into a part header only when it is a clean type)
                let mime = stored
                    .or_else(|| str_field(a, "mimeType").and_then(super::upstream::clean_mime))
                    .unwrap_or_else(|| "application/octet-stream".to_owned());
                let old = self.copy_one(conn, c, &name, &mime, bytes).await?;
                copies.0.insert(upload, old.clone());
                old
            };
            if let Some(o) = a.as_object_mut() {
                o.insert("path".into(), old);
            }
        }
        Ok(())
    }

    /// One file written into the old child under an upload ticket (a new ticket when one's hold ran out before the
    /// write); its old path.
    async fn copy_one(
        &self,
        conn: &Arc<Conn>,
        c: Uuid,
        name: &str,
        mime: &str,
        bytes: axum::body::Bytes,
    ) -> Result<Value, Copy> {
        let failed = || Copy::Failed(self.locale().upload_failed().to_owned());
        for _ in 0..MAX_TICKETS {
            let (t, sent) = self
                .take(conn, c, TicketKind::Upload, None)
                .await
                .map_err(|_| failed())?;
            let h = match (t.route, t.ticket) {
                (TicketRoute::Native, _) => return Err(Copy::Native),
                (TicketRoute::Legacy, Some(ticket)) => Held {
                    ticket,
                    take: t.take,
                    hold: sent + Duration::from_millis(t.hold_ms.unwrap_or(0)),
                },
                (TicketRoute::Legacy, None) => return Err(failed()),
            };
            let Some(who) = self.who_of_conn(conn) else {
                // no unexpired token of the holder (a tab that outlived its Access token): signed out
                self.report(conn, h, TicketOutcome::Refused);
                self.0.metrics.lu_401.inc();
                return Err(Copy::SignedOut);
            };
            if Instant::now() >= h.hold {
                // nothing written under it: closed, and the copy goes under a new one
                self.report(conn, h, TicketOutcome::Refused);
                continue;
            }
            let bound = copy_bound(bytes.len(), self.0.write_timeout);
            return match self.old_upload(&who, name, mime, bytes.clone(), bound).await {
                Ok(rec) => {
                    self.report(conn, h, TicketOutcome::Applied);
                    rec.get("path").cloned().ok_or_else(failed)
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "copy-at-send: the old child did not take the file");
                    if e == CallError::SignedOut {
                        self.report(conn, h, TicketOutcome::Refused);
                        return Err(Copy::SignedOut);
                    }
                    if refused_by_old(&e) {
                        self.report(conn, h, TicketOutcome::Refused);
                    } else if let CallError::AppliedUnread(_) = e {
                        // written, its record unread: the ticket closes, the send fails (the tab sends again)
                        self.report(conn, h, TicketOutcome::Applied);
                    }
                    // otherwise it may have been written: the importer expires the ticket after its settle read
                    Err(failed())
                }
            };
        }
        Err(failed())
    }

    /// A create while a rollback is under way (DESIGN 9.10 step 1): through the LU, born legacy.
    pub(crate) async fn create(&self, conn: Arc<Conn>, rid: String, mut m: Value) {
        let subscribe = m.get("subscribe").is_some_and(|s| !s.is_null());
        if let Some(o) = m.as_object_mut() {
            // the old child's create updates carry no request id: pigeon sends the tab its own (below)
            o.remove("subscribe");
        }
        let key = str_field(&m, "idempotencyKey").unwrap_or("").to_owned();
        let lu = self.lu_for(&conn);
        // bounded at the old child too: a create the tab retries is de-duplicated by its idempotency key across the
        // person's sockets there (claudecodeui `paseo-connection.ts`)
        match lu.write(m, super::CONNECT_TIMEOUT).await {
            Ok((typ, p)) => {
                let agent = p["agent"]["id"]
                    .as_str()
                    .or_else(|| p["workspace"]["id"].as_str().and_then(|w| w.strip_prefix("wks_")))
                    .and_then(|s| Uuid::parse_str(s).ok());
                if let Some(a) = agent {
                    self.mark(&conn.person, a, true);
                    if subscribe && p["error"].is_null() {
                        for (revision, phase) in [
                            "accepted",
                            "workspace_ready",
                            "agent_ready",
                            "prompt_started",
                            "completed",
                        ]
                        .iter()
                        .enumerate()
                        {
                            conn.reply(
                                "workspace.create.update",
                                &json!({"kind": "workspace", "idempotencyKey": key, "revision": revision,
                                        "phase": phase, "error": null, "workspaceId": p["workspace"]["id"],
                                        "agentId": a, "workspace": p["workspace"], "agent": p["agent"]}),
                            );
                        }
                    }
                }
                answer(&conn, &typ, p, &rid);
            }
            Err(e) => self.lu_failed(&conn, "workspace.create.request", &rid, e),
        }
    }
}

/// A copy-at-send's bound: the upload tee's for the old route's largest file, in proportion to the file, never less
/// than a write's own. It may run past the upload ticket's settle time (the module's doc says why that is safe).
fn copy_bound(len: usize, write_timeout: Duration) -> Duration {
    let full = super::uploads::OLD_UPLOAD_BOUND;
    let ms = u64::try_from(len)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(full.as_millis()).unwrap_or(u64::MAX))
        / OLD_FILE_MAX;
    Duration::from_millis(ms).clamp(write_timeout, full)
}

/// The old paths a send's files were copied to, by upload id (kept across the send's tickets).
#[derive(Default)]
struct Copies(HashMap<Uuid, Value>);

/// The upload id of a magpie upload path (`uploads/<uploadId>/<name>`).
fn magpie_upload_id(path: &str) -> Option<Uuid> {
    let rest = path.strip_prefix("uploads/")?;
    let (id, name) = rest.split_once('/')?;
    if name.is_empty() {
        return None;
    }
    Uuid::parse_str(id).ok()
}

/// The old child's answer to the tab, under the tab's own request id.
fn answer(conn: &Conn, typ: &str, mut p: Value, rid: &str) {
    if let Some(o) = p.as_object_mut()
        && !rid.is_empty()
    {
        o.insert("requestId".into(), json!(rid));
    }
    conn.reply(typ, &p);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_of_the_old_answers() {
        let send = TicketKind::Send;
        assert_eq!(
            outcome(
                send,
                "send_agent_message_response",
                &json!({"accepted": true, "error": null})
            ),
            TicketOutcome::Started
        );
        assert_eq!(
            outcome(
                send,
                "send_agent_message_response",
                &json!({"accepted": false, "error": "busy"})
            ),
            TicketOutcome::Refused
        );
        let stop = TicketKind::Stop;
        assert_eq!(
            outcome(stop, "cancel_agent_response", &json!({"agent": null, "error": null})),
            TicketOutcome::Applied
        );
        assert_eq!(
            outcome(TicketKind::Delete, "rpc_error", &json!({"error": "x"})),
            TicketOutcome::Refused
        );
        assert_eq!(
            outcome(TicketKind::Delete, "agent_archived", &json!({"agentId": "a"})),
            TicketOutcome::Applied
        );
        assert_eq!(
            outcome(
                TicketKind::Rename,
                "update_agent_response",
                &json!({"accepted": true, "error": null})
            ),
            TicketOutcome::Applied
        );
    }

    #[test]
    fn a_send_is_found_by_its_client_message_id_or_row_id() {
        let page = json!({"entries": [{"item": {"type": "user_message", "id": "paseo-user-m1"}},
                                      {"item": {"type": "user_message", "clientMessageId": "m2"}}]});
        assert!(holds_send(&page, "m1"));
        assert!(holds_send(&page, "m2"));
        assert!(!holds_send(&page, "m3"));
    }

    #[test]
    fn a_send_without_a_message_id_keeps_one_across_its_resend() {
        let c = Uuid::new_v4();
        let m = json!({"text": "繼續"});
        assert_eq!(stable_message_id(c, "r1", &m), stable_message_id(c, "r1", &m));
        assert_ne!(stable_message_id(c, "r1", &m), stable_message_id(c, "r2", &m));
        assert_ne!(
            stable_message_id(c, "", &m),
            stable_message_id(c, "", &m),
            "no request id: never matched"
        );
    }

    #[test]
    fn writes_and_their_tickets() {
        assert_eq!(ticket_kind("send_agent_message_request", false), Some(TicketKind::Send));
        assert_eq!(ticket_kind("delete_agent_request", true), Some(TicketKind::Purge));
        assert_eq!(ticket_kind("delete_agent_request", false), Some(TicketKind::Delete));
        assert_eq!(ticket_kind("refresh_agent_request", false), Some(TicketKind::Restore));
        assert_eq!(ticket_kind("refresh_agent_request", true), Some(TicketKind::Restore));
        assert_eq!(ticket_kind("fetch_agent_timeline_request", false), None);
        assert_eq!(
            magpie_upload_id("uploads/0190f6c2-7a3b-7c4d-8e5f-6a7b8c9d0e4c/a.pdf").map(|u| u.to_string()),
            Some("0190f6c2-7a3b-7c4d-8e5f-6a7b8c9d0e4c".into())
        );
        assert_eq!(magpie_upload_id("/h/u1/.claude/cloudcli-assets/a.pdf"), None);
    }
}
