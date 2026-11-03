//! A legacy conversation's socket requests (DESIGN 9.2-9.5): reads through the LU as they are; writes under a
//! magpie ticket that closes only on the old stack's outcome (WP-51's contract, `magpie_api::legacy`):
//!
//! - every write takes a ticket first; `native` (magpie owns the conversation now, the fence won) sends it natively,
//!   `legacy` sends it through the LU, and the old child's answer is the ticket's outcome (a send: `started` when it
//!   accepted, since the old hub starts the turn before it answers; `refused` otherwise; anything else `applied` or
//!   `refused`);
//! - a send's ticket call carries its `clientMessageId` (a retry gets the open ticket); no write starts later than
//!   the answer's `holdMs` after pigeon SENT the ticket call (its monotonic clock): past that, a new ticket; every
//!   write is given up after `TICKET_WRITE_TIMEOUT` (20 s) without the old child's answer, and the ticket is left for
//!   the importer to expire;
//! - an answer with `supersedes` or `reused` means a first send may have reached the old child, which does not
//!   de-duplicate by `clientMessageId` (a tab's resend after its socket dropped, the drain or a dead replica):
//!   pigeon waits `settleMs` (a reuse: [`REUSE_SETTLE`]) from when it sent that call, reads the old child, and sends
//!   only when the first send is absent: no user row of it in the timeline, and the conversation not running,
//!   preparing or holding a pending message (an old child that restarted rebuilds its timeline without the row's
//!   id). When present, the ticket is reported from what the old child shows and the send is answered as accepted;
//!   when the old child cannot be read, nothing is sent. Every other send is not sent again either when the
//!   timeline already holds its user row (a resend whose answer the tab never got);
//! - a send of magpie-only files copies each into the old child first, each under an upload ticket of its own, then
//!   sends with the old paths under the send's ticket; a copy whose ticket answers `native` sends natively with the
//!   tab's own message (every path magpie's);
//! - an outcome is `refused` only when the old route itself answered no (a 4xx, the old child's own refusal); a
//!   5xx, a gateway error or no answer may come after the write was applied, so the ticket is left for the importer
//!   to expire after its settle read;
//! - a socket with a ticketed write in flight counts as a transfer for the drain (its 1012 waits for the write, up
//!   to the transfer bound), and every write the switch fails counts in `pigeon_switch_write_refused_total`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use magpie_api::client::ClientError;
use magpie_api::legacy::{
    TICKET_WRITE_TIMEOUT, TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketResponse, TicketRoute,
};
use serde_json::{Value, json};
use uuid::Uuid;

use super::lu::{Lu, LuError};
use super::upstream::CallError;
use super::{Legacy, OLD_BUSY, OLD_USER_ROW_PREFIX, READ_TIMEOUT};
use crate::feeds::{BoxFuture, Delegate};
use crate::paseo::dispatch;
use crate::paseo::messages::{rpc_error, str_field};
use crate::paseo::strings::{self, Locale};
use crate::paseo::transport::Conn;

/// Tickets taken for one write at most (each past its hold is closed `refused` and another taken).
const MAX_TICKETS: usize = 4;

/// How long after a reused ticket's call pigeon reads the old child: a first write under it was either sent on a
/// socket that has closed since (its bytes have landed by then) or is still being answered.
pub const REUSE_SETTLE: Duration = Duration::from_secs(2);

/// The legacy delegate installed for every legacy conversation.
pub(crate) struct LegacyDelegate(pub(crate) Legacy);

impl Delegate for LegacyDelegate {
    fn request(&self, socket: Arc<Conn>, kind: String, message: Value) -> BoxFuture {
        let l = self.0.clone();
        Box::pin(async move { l.socket_request(socket, kind, message).await })
    }

    fn subscribe(&self, socket: Arc<Conn>, conversation: Uuid) {
        self.0.lu_for(&socket).watch(conversation);
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

/// How a copy-at-send ended other than with every file in the old child.
enum Copy {
    /// An upload ticket answered `native`: send natively with the magpie paths.
    Native,
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
                    m["messageId"] = json!(Uuid::new_v4().to_string());
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

    /// Reports a ticket's outcome (retried in the background: until it lands the importer keeps the conversation
    /// legacy, which only delays its release).
    fn report(&self, conn: &Arc<Conn>, ticket: Uuid, outcome: TicketOutcome) {
        let (me, conn) = (self.clone(), conn.clone());
        tokio::spawn(async move { me.report_now(&conn, ticket, outcome).await });
    }

    /// [`Self::report`], waited for; `false` when it did not land.
    async fn report_now(&self, conn: &Conn, ticket: Uuid, outcome: TicketOutcome) -> bool {
        let Some(app) = self.app().cloned() else { return false };
        let mut wait = Duration::from_millis(250);
        for _ in 0..8 {
            match conn
                .calls(&app.0.magpie)
                .legacy_ticket_done(ticket, &TicketDone { outcome })
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

    /// A write the switch could not make: counted, and the tab's error (the signed-out answer for a 401).
    fn write_lost(&self, conn: &Arc<Conn>, kind: &str, rid: &str, e: LuError) {
        if e != LuError::SignedOut {
            self.0.metrics.write_refused.inc();
        }
        self.lu_failed(conn, kind, rid, e);
    }

    /// A write: ticket, then (a send) the copies of its magpie-only files, then the write through the LU.
    #[allow(clippy::many_single_char_names)] // c, m, t, o, p: the conversation, message, ticket, outcome, payload
    #[allow(clippy::too_many_lines)] // one write's steps in order: ticket, repeat check, copies, hold, write
    async fn ticketed(&self, conn: &Arc<Conn>, kind: &str, c: Uuid, m: Value, rid: &str, tk: TicketKind) {
        let Some(app) = self.app().cloned() else { return };
        // the drain's 1012 waits for this write (its ticket stays open otherwise and the tab's resend meets it)
        let _transfer = conn.transfer();
        let cmid = (tk == TicketKind::Send).then(|| str_field(&m, "messageId").unwrap_or("").to_owned());
        let lu = self.lu_for(conn);
        // the LU is open before any ticket, so its upgrade does not eat into a hold
        if let Err(e) = lu.ready().await {
            return self.write_lost(conn, kind, rid, e);
        }
        for _ in 0..MAX_TICKETS {
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
            let ticket = match (t.route, t.ticket) {
                (TicketRoute::Native, _) => {
                    // magpie owns it now: a native write (the release's replacement comes with the owner change)
                    self.mark(&conn.person, c, false);
                    let hub = app.hub().unwrap_or_default();
                    return dispatch::native(&app, &hub, conn, kind.to_owned(), m).await;
                }
                (TicketRoute::Legacy, Some(ticket)) => ticket,
                (TicketRoute::Legacy, None) => {
                    self.0.metrics.write_refused.inc();
                    return self.write_failed(conn, kind, rid, false);
                }
            };
            let hold = sent + Duration::from_millis(t.hold_ms.unwrap_or(0));
            if let Some(cmid) = &cmid {
                // a first send may have reached the old child (a repeat under a reused or superseded ticket):
                // read it once its writes have settled, and take the safe side
                let settle = match (&t.supersedes, t.reused) {
                    (Some(sup), _) => Some(Duration::from_millis(sup.settle_ms)),
                    (None, true) => Some(REUSE_SETTLE),
                    (None, false) => None,
                };
                let found = match settle {
                    Some(settle) => {
                        tokio::time::sleep_until((sent + settle).into()).await;
                        match self.old_has_send(conn, &lu, c, cmid, true).await {
                            Some(f) => f,
                            None => {
                                // the old child cannot be read: nothing is sent (a send twice is a second turn)
                                return self.write_lost(conn, kind, rid, LuError::NotSent);
                            }
                        }
                    }
                    // a resend whose first answer never reached the tab: its user row is in the timeline
                    None => self
                        .old_has_send(conn, &lu, c, cmid, false)
                        .await
                        .unwrap_or(OldSend::Absent),
                };
                if let OldSend::Present { running } = found {
                    let o = if running {
                        TicketOutcome::Started
                    } else {
                        TicketOutcome::Completed
                    };
                    self.report(conn, ticket, o);
                    tracing::info!(conversation = %c, "a repeated send had reached the old child: not sent again");
                    let p = json!({"agentId": c, "accepted": true, "error": null});
                    return answer(conn, "send_agent_message_response", p, rid);
                }
            }
            // the tab's message stays as it is (every native fallback sends it); the LU send carries the old paths
            let mut out = m.clone();
            if tk == TicketKind::Send {
                match self.copy_files(conn, c, &mut out).await {
                    Ok(()) => {}
                    Err(Copy::Native) => {
                        self.report(conn, ticket, TicketOutcome::Refused);
                        self.mark(&conn.person, c, false);
                        let hub = app.hub().unwrap_or_default();
                        return dispatch::native(&app, &hub, conn, kind.to_owned(), m).await;
                    }
                    Err(Copy::Failed(sentence)) => {
                        self.report(conn, ticket, TicketOutcome::Refused);
                        self.0.metrics.write_refused.inc();
                        let p = json!({"agentId": c, "accepted": false, "error": sentence});
                        return answer(conn, "send_agent_message_response", p, rid);
                    }
                }
            }
            if Instant::now() >= hold || conn.is_closing() {
                // no write starts past the hold (nor for a tab that has gone: its resend may already be on its way):
                // this ticket closes with nothing written, before another is taken (a send's next ticket call would
                // otherwise get this one back, open, and write under a ticket about to close `refused`)
                if conn.is_closing() {
                    self.report(conn, ticket, TicketOutcome::Refused);
                    return;
                }
                if !self.report_now(conn, ticket, TicketOutcome::Refused).await {
                    self.0.metrics.write_refused.inc();
                    return self.write_failed(conn, kind, rid, false);
                }
                continue;
            }
            match lu.call(out, TICKET_WRITE_TIMEOUT).await {
                Ok((typ, p)) => {
                    let o = outcome(tk, &typ, &p);
                    self.report(conn, ticket, o);
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
                Err(e) => {
                    if !e.maybe_sent() {
                        self.report(conn, ticket, TicketOutcome::Refused);
                    }
                    return self.write_lost(conn, kind, rid, e);
                }
            }
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

    /// What the old child shows of the send of `cmid` (`None`: it cannot be read). Its timeline lives in the old
    /// hub's memory and is rebuilt from history without the row's id after a restart, and a long turn can push the
    /// row out of the tail, so with `strict` (a repeat whose first write may have landed) a conversation that runs,
    /// is preparing, or holds a pending message (the agent's status, and the old child's busy state) counts as
    /// having it, and an unreadable busy state is `None`.
    async fn old_has_send(&self, conn: &Conn, lu: &Lu, c: Uuid, cmid: &str, strict: bool) -> Option<OldSend> {
        let tail = json!({"type": "fetch_agent_timeline_request", "agentId": c, "direction": "tail",
                          "limit": 200, "projection": "projected"});
        let (_, page) = lu.call(tail, READ_TIMEOUT).await.ok()?;
        let held = holds_send(&page, cmid);
        if !strict && !held {
            return Some(OldSend::Absent);
        }
        let agent = json!({"type": "fetch_agent_request", "agentId": c});
        let (_, a) = lu.call(agent, READ_TIMEOUT).await.ok()?;
        // the old bridge's agent snapshot says `running` or `idle` (`paseo-connection.ts`)
        let mut running = a["agent"]["status"] == "running";
        if !strict {
            return Some(OldSend::Present { running });
        }
        let busy = self.old_busy(conn).await?;
        let id = c.to_string();
        let listed = |k: &str| {
            busy.get(k)
                .and_then(Value::as_array)
                .is_some_and(|l| l.iter().any(|x| x.as_str() == Some(id.as_str())))
        };
        running = running || listed("running") || listed("preparing") || listed("pending");
        Some(if held || running {
            OldSend::Present { running }
        } else {
            OldSend::Absent
        })
    }

    /// The old child's busy state (`running`, `processes`, `preparing`, `pending`: session ids), through LU HTTP.
    async fn old_busy(&self, conn: &Conn) -> Option<Value> {
        let who = self.who_of_conn(conn)?;
        let (_, body) = self
            .send(self.request(reqwest::Method::GET, OLD_BUSY, &who), READ_TIMEOUT)
            .await
            .ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// The copy-at-send (DESIGN 9.5): each attachment that is a magpie upload (`uploads/<id>/…`) is written into the
    /// old child under an upload ticket of its own, and `m` (the LU send's own copy of the tab's message) carries its
    /// old path instead.
    #[allow(clippy::many_single_char_names)] // c, m, t, r, b: the conversation, message, ticket, answer, bytes
    async fn copy_files(&self, conn: &Arc<Conn>, c: Uuid, m: &mut Value) -> Result<(), Copy> {
        let Some(app) = self.app().cloned() else {
            return Ok(());
        };
        let Some(list) = m.get_mut("attachments").and_then(Value::as_array_mut) else {
            return Ok(());
        };
        for a in list.iter_mut() {
            let path = str_field(a, "path").unwrap_or("").to_owned();
            let Some(upload) = magpie_upload_id(&path) else {
                continue;
            };
            let failed = || Copy::Failed(self.locale().upload_failed().to_owned());
            let (t, sent) = self
                .take(conn, c, TicketKind::Upload, None)
                .await
                .map_err(|_| failed())?;
            let ticket = match (t.route, t.ticket) {
                (TicketRoute::Native, _) => return Err(Copy::Native),
                (TicketRoute::Legacy, Some(t)) => t,
                (TicketRoute::Legacy, None) => return Err(failed()),
            };
            let hold = sent + Duration::from_millis(t.hold_ms.unwrap_or(0));
            let read = match conn.calls(&app.0.magpie_transfer).upload_raw(upload).await {
                Ok(r) => r.bytes().await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let bytes = match read {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e, "copy-at-send: the upload's bytes not read");
                    self.report(conn, ticket, TicketOutcome::Refused);
                    return Err(failed());
                }
            };
            let Some(who) = self.who_of_conn(conn) else {
                self.report(conn, ticket, TicketOutcome::Refused);
                return Err(failed());
            };
            if Instant::now() >= hold {
                self.report(conn, ticket, TicketOutcome::Refused);
                return Err(failed());
            }
            let name = str_field(a, "fileName")
                .map_or_else(|| path.rsplit('/').next().unwrap_or("file").to_owned(), str::to_owned);
            let mime = str_field(a, "mimeType")
                .unwrap_or("application/octet-stream")
                .to_owned();
            match self.old_upload(&who, &name, &mime, bytes, TICKET_WRITE_TIMEOUT).await {
                Ok(rec) => {
                    self.report(conn, ticket, TicketOutcome::Applied);
                    if let (Some(old), Some(o)) = (rec.get("path").cloned(), a.as_object_mut()) {
                        o.insert("path".into(), old);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "copy-at-send: the old child did not take the file");
                    if refused_by_old(&e) {
                        self.report(conn, ticket, TicketOutcome::Refused);
                    }
                    // otherwise it may have been written: the importer expires the ticket after its settle read
                    return Err(failed());
                }
            }
        }
        Ok(())
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
        match lu.call(m, super::CONNECT_TIMEOUT).await {
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
