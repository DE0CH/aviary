//! A legacy conversation's REST calls (DESIGN 9.1, 9.4-9.5): its warm calls before the gate (a ticketed write; from
//! the gate a warm is a hermit hint only, so typing does not keep resetting its quiet period), its share links
//! (listed, created and revoked through the old child, the writes under tickets) and its files (the old child's).
//! A share magpie does not know yet (made in the old child, not copied by the importer) is found by asking the old
//! child for each of the person's legacy conversations' links, never from one replica's memory alone; a revoke whose
//! share could not be looked for everywhere is a retryable 503, never magpie's 404. An outcome is `refused` only for
//! the old route's own 4xx; a 5xx or no answer leaves the ticket for the importer's expiry, and a 2xx whose body was
//! lost is `applied` (a create's link is then found in the old child's list, so a retry does not make a second).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use magpie_api::legacy::{TICKET_WRITE_TIMEOUT, TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketRoute};
use uuid::Uuid;

use super::upstream::CallError;
use super::{Legacy, OLD_WARM, Seen, Unknown};
use crate::App;
use crate::http::{api_error, person_calls};
use crate::identity::Identity;
use magpie_api::conversations::{ListQuery, ListState};

/// Shares created through the legacy upstream on this replica, by share id: the conversation a revoke's ticket names
/// before the importer has copied the share into magpie.
#[derive(Debug, Default)]
pub(crate) struct LuShares(Mutex<HashMap<String, Uuid>>);

impl LuShares {
    fn put(&self, share: &str, conversation: Uuid) {
        let mut m = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if m.len() > 10_000 {
            m.clear();
        }
        m.insert(share.to_owned(), conversation);
    }

    fn get(&self, share: &str) -> Option<Uuid> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(share)
            .copied()
    }
}

/// How many conversations' old-child links a revoke's owner lookup reads at a time.
const SHARE_LOOKUPS: usize = 8;

/// The longest a revoke's owner lookup takes (the tab's request is answered 503 past it).
const SHARE_OWNER_BOUND: Duration = Duration::from_secs(20);

/// The outcome of a ticketed REST write.
enum Ticketed {
    /// magpie owns the conversation: the caller does it natively.
    Native,
    /// The old router's answer.
    Done(Result<(u16, Bytes), CallError>),
}

impl Legacy {
    /// Whether the conversation (of the request's person) is legacy, looked up when pigeon has not read it while the
    /// person has legacy conversations. `Err` when that cannot be told (the window or the conversation not read).
    ///
    /// # Errors
    /// The owner could not be read.
    pub async fn rest_owner(&self, app: &App, id: &Identity, conversation: Uuid) -> Result<bool, Unknown> {
        let Some(person) = id.subject() else { return Ok(false) };
        if let Some(l) = self.is_legacy(person, &conversation) {
            return Ok(l);
        }
        match self.window(person).await {
            Ok(w) if w.person_legacy > 0 => {}
            Ok(_) => return Ok(false),
            Err(_) => return Err(Unknown),
        }
        let Some(calls) = person_calls(&app.0.magpie, id) else {
            return Ok(false);
        };
        let sent = std::time::Instant::now();
        match calls.get_conversation(conversation).await {
            Ok(c) => {
                self.observe(person, &c, Seen::Read(sent));
                Ok(c.migrating)
            }
            // not the person's: magpie's own answer is the right one
            Err(e) if e.api().is_some_and(|r| r.status < 500 && r.status != 429) => Ok(false),
            Err(_) => Err(Unknown),
        }
    }

    /// [`Legacy::rest_owner`], an unknown owner taken as native (for writes: magpie's 409 re-routes them).
    pub async fn rest_is_legacy(&self, app: &App, id: &Identity, conversation: Uuid) -> bool {
        self.rest_owner(app, id, conversation).await.unwrap_or(false)
    }

    /// A ticketed write to the old router: the ticket, the call within the hold, the outcome reported.
    async fn ticketed_rest(
        &self,
        app: &App,
        id: &Identity,
        conversation: Uuid,
        kind: TicketKind,
        call: impl Fn() -> reqwest::RequestBuilder,
    ) -> Ticketed {
        let Some(calls) = person_calls(&app.0.magpie_turns, id) else {
            return Ticketed::Done(Err(CallError::Unreachable));
        };
        for _ in 0..3 {
            let sent = std::time::Instant::now();
            let t = match calls
                .legacy_ticket(
                    conversation,
                    &TicketRequest {
                        kind,
                        client_message_id: None,
                    },
                )
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, "ticket not taken");
                    self.0.metrics.write_refused.inc();
                    return Ticketed::Done(Err(CallError::Unreachable));
                }
            };
            let ticket = match (t.route, t.ticket) {
                (TicketRoute::Native, _) => {
                    if let Some(p) = id.subject() {
                        self.mark(p, conversation, false);
                    }
                    return Ticketed::Native;
                }
                (TicketRoute::Legacy, Some(t)) => t,
                (TicketRoute::Legacy, None) => {
                    self.0.metrics.write_refused.inc();
                    return Ticketed::Done(Err(CallError::Unreachable));
                }
            };
            let take = t.take;
            let report = |outcome: TicketOutcome| {
                let calls = calls.clone();
                async move {
                    for _ in 0..4 {
                        if calls
                            .legacy_ticket_done(ticket, &TicketDone { outcome, take })
                            .await
                            .is_ok()
                        {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                    tracing::warn!("ticket outcome not reported");
                }
            };
            if sent.elapsed() >= Duration::from_millis(t.hold_ms.unwrap_or(0)) {
                report(TicketOutcome::Refused).await;
                continue;
            }
            let r = self.send(call(), TICKET_WRITE_TIMEOUT).await;
            match &r {
                Ok(_) | Err(CallError::AppliedUnread(_)) => report(TicketOutcome::Applied).await,
                // the old route's own no: nothing was written
                Err(e) if super::delegate::refused_by_old(e) => report(TicketOutcome::Refused).await,
                // a 5xx or a gateway's error, or no answer: it may have been applied after all; the importer expires
                // the ticket after its settle read
                Err(_) => {
                    self.0.metrics.write_refused.inc();
                }
            }
            return Ticketed::Done(r);
        }
        self.0.metrics.write_refused.inc();
        Ticketed::Done(Err(CallError::Unreachable))
    }

    /// `POST /api/…/warm` of a legacy conversation before the gate: a ticketed old warm, in the background (the
    /// route answers at once). `false` when it is not one (the caller sends hermit's hint).
    pub async fn warm(&self, app: &App, id: &Identity, conversation: Uuid, body: Bytes) -> bool {
        let Some(person) = id.subject() else { return false };
        if !self.rest_is_legacy(app, id, conversation).await {
            return false;
        }
        match self.window(person).await {
            // from the gate: a hermit hint only (DESIGN 9.4)
            Ok(w) if w.handover_allowed_at.is_some() && w.rollback_at.is_none() => return false,
            Ok(_) => {}
            Err(_) => return false,
        }
        let Some(who) = self.who(id) else { return false };
        let (me, app, id) = (self.clone(), app.clone(), id.clone());
        // the drain waits for it before the process exits (its ticket's outcome with it)
        let transfer = app.transfers().guard();
        tokio::spawn(async move {
            let _transfer = transfer;
            let call = || {
                me.request(Method::POST, OLD_WARM, &who)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(body.clone())
            };
            if let Ticketed::Done(Err(e)) = me.ticketed_rest(&app, &id, conversation, TicketKind::Warm, call).await {
                tracing::debug!(error = ?e, "old warm failed");
            }
        });
        true
    }

    /// A legacy conversation's share links (`GET` the list, `POST` a new one) through the old child. `None` when
    /// magpie owns the conversation (the caller serves it).
    pub async fn shares(&self, app: &App, req: &Parts, id: &Identity, conversation: Uuid) -> Option<Response> {
        match self.rest_owner(app, id, conversation).await {
            Ok(true) => {}
            Ok(false) => return None,
            // a create goes to magpie, which re-routes it with its 409; a list is not answered from magpie's copy
            Err(Unknown) if req.method == Method::POST => return None,
            Err(Unknown) => return Some(retry_later()),
        }
        let who = self.who(id)?;
        let path = req.uri.path().to_owned();
        if req.method == Method::POST {
            // the links before the create: a create whose answer is lost is found by what is new in the list
            let before = self.share_ids(&path, &who).await;
            let call = || self.request(Method::POST, &path, &who);
            return match self
                .ticketed_rest(app, id, conversation, TicketKind::ShareCreate, call)
                .await
            {
                Ticketed::Native => None,
                Ticketed::Done(Err(CallError::AppliedUnread(_))) => {
                    Some(self.created_unread(&path, &who, before).await)
                }
                Ticketed::Done(r) => {
                    if let Ok((_, body)) = &r
                        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(body)
                        && let Some(s) = v.get("id").and_then(serde_json::Value::as_str)
                    {
                        self.0.lu_shares.put(s, conversation);
                    }
                    Some(rest_answer(r))
                }
            };
        }
        Some(rest_answer(
            self.send(self.request(Method::GET, &path, &who), Duration::from_secs(30))
                .await,
        ))
    }

    /// A conversation's old-child links (`path` its shares route): `None` when they cannot be read.
    async fn share_list(&self, path: &str, who: &super::upstream::Who) -> Option<Vec<serde_json::Value>> {
        let (_, body) = self
            .send(self.request(Method::GET, path, who), Duration::from_secs(30))
            .await
            .ok()?;
        let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
        v["shares"].as_array().cloned()
    }

    async fn share_ids(&self, path: &str, who: &super::upstream::Who) -> Option<Vec<String>> {
        Some(
            self.share_list(path, who)
                .await?
                .iter()
                .filter_map(|s| s["id"].as_str().map(str::to_owned))
                .collect(),
        )
    }

    /// A create the old route applied whose answer was lost: the one link the list has now and did not before, as
    /// the create's answer; otherwise a retryable 503 (a retry may then make a second link, which the person can
    /// revoke; nothing is lost).
    async fn created_unread(&self, path: &str, who: &super::upstream::Who, before: Option<Vec<String>>) -> Response {
        if let (Some(before), Some(now)) = (before, self.share_list(path, who).await) {
            let new: Vec<&serde_json::Value> = now
                .iter()
                .filter(|s| s["id"].as_str().is_some_and(|id| !before.iter().any(|b| b == id)))
                .collect();
            if let [one] = new.as_slice() {
                let mut resp = Response::new(axum::body::Body::from(one.to_string()));
                *resp.status_mut() = StatusCode::CREATED;
                let h = resp.headers_mut();
                h.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json; charset=utf-8"),
                );
                h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                return resp;
            }
        }
        tracing::warn!("a share create was applied, its link not found");
        retry_later()
    }

    /// A share create magpie refused with 409 `conversation_migrating` `route: "legacy"` (pigeon's view of the owner
    /// was stale, or the owner could not be read): the conversation is legacy, and the create goes to the old child
    /// under a ticket.
    pub async fn share_rerouted(&self, app: &App, req: &Parts, id: &Identity, conversation: Uuid) -> Option<Response> {
        let person = id.subject()?;
        self.mark(person, conversation, true);
        self.shares(app, req, id, conversation).await
    }

    /// A revoke of a share whose conversation is legacy: magpie answered 409 `route: "legacy"` with the conversation,
    /// or 404 while the person has legacy conversations (a link made in the old child that magpie has not copied
    /// yet). In that case the share's conversation is the one whose old-child links list it (the links pigeon made
    /// here are tried first). `None`: not one of those (magpie's 404 stands). A share whose owner could not be looked
    /// for in every legacy conversation is a retryable 503, never a 404 for a link that may still be live.
    pub async fn revoke(
        &self,
        app: &App,
        req: &Parts,
        id: &Identity,
        share: &str,
        conversation: Option<Uuid>,
    ) -> Option<Response> {
        let known = conversation.or_else(|| self.0.lu_shares.get(share));
        let conversation = if let Some(c) = known {
            c
        } else {
            let Ok(c) = self.share_owner(app, id, share).await else {
                self.0.metrics.write_refused.inc();
                return Some(retry_later());
            };
            c?
        };
        let who = self.who(id)?;
        let path = req.uri.path().to_owned();
        let call = || self.request(Method::DELETE, &path, &who);
        match self
            .ticketed_rest(app, id, conversation, TicketKind::ShareRevoke, call)
            .await
        {
            Ticketed::Native => None,
            // revoked; its answer (a 204) was lost on the way
            Ticketed::Done(Err(CallError::AppliedUnread(s))) => Some(rest_answer(Ok((s, Bytes::new())))),
            Ticketed::Done(r) => Some(rest_answer(r)),
        }
    }

    /// The legacy conversation of the person whose old-child links list `share` (`Ok(None)`: none does, or the person
    /// has no legacy conversation; `Err`: a list could not be read, so it cannot be told). The conversations' links
    /// are read [`SHARE_LOOKUPS`] at a time, all within [`SHARE_OWNER_BOUND`].
    async fn share_owner(&self, app: &App, id: &Identity, share: &str) -> Result<Option<Uuid>, Unknown> {
        use futures_util::StreamExt as _;
        let Some(person) = id.subject() else { return Ok(None) };
        match self.window(person).await {
            Ok(w) if w.person_legacy > 0 => {}
            Ok(_) => return Ok(None),
            Err(_) => return Err(Unknown),
        }
        let calls = person_calls(&app.0.magpie, id).ok_or(Unknown)?;
        let who = self.who(id).ok_or(Unknown)?;
        let walk = async {
            let mut legacy = Vec::new();
            for state in [ListState::Active, ListState::Deleted] {
                let mut cursor = None;
                let mut done = false;
                for _ in 0..50 {
                    let q = ListQuery {
                        state: Some(state),
                        limit: Some(200),
                        cursor: cursor.take(),
                        search: None,
                    };
                    let page = calls.list_conversations(&q).await.map_err(|_| Unknown)?;
                    legacy.extend(page.conversations.iter().filter(|c| c.migrating).map(|c| c.id));
                    if let Some(n) = page.next_cursor {
                        cursor = Some(n);
                    } else {
                        done = true;
                        break;
                    }
                }
                if !done {
                    return Err(Unknown);
                }
            }
            let who = &who;
            let mut lists = futures_util::stream::iter(legacy)
                .map(|c| async move {
                    let path = format!("{}{c}/shares", crate::paseo::compat::SESSIONS);
                    (c, self.share_ids(&path, who).await)
                })
                .buffer_unordered(SHARE_LOOKUPS);
            let mut unread = false;
            while let Some((c, ids)) = lists.next().await {
                match ids {
                    Some(ids) if ids.iter().any(|s| s == share) => return Ok(Some(c)),
                    Some(_) => {}
                    None => unread = true,
                }
            }
            if unread { Err(Unknown) } else { Ok(None) }
        };
        let found = tokio::time::timeout(SHARE_OWNER_BOUND, walk)
            .await
            .unwrap_or(Err(Unknown));
        if let Ok(Some(c)) = found {
            self.0.lu_shares.put(share, c);
        }
        found
    }

    /// A legacy conversation's schedules (the app's panel) from the old child, which fires them until the release
    /// carries them to magpie. `None` when magpie owns the conversation (the caller serves it).
    pub async fn schedules(&self, app: &App, req: &Parts, id: &Identity, conversation: Uuid) -> Option<Response> {
        match self.rest_owner(app, id, conversation).await {
            Ok(true) => {}
            Ok(false) => return None,
            Err(Unknown) => return Some(retry_later()),
        }
        let who = self.who(id)?;
        Some(rest_answer(
            self.send(self.request(Method::GET, req.uri.path(), &who), Duration::from_secs(30))
                .await,
        ))
    }

    /// A legacy conversation's file (`files/raw`): the old child's answer, streamed, with the range headers passed.
    /// An owner that cannot be read answers 503 (retryable), not magpie's copy.
    pub async fn files_raw(&self, app: &App, req: &Parts, id: &Identity, conversation: Uuid) -> Option<Response> {
        match self.rest_owner(app, id, conversation).await {
            Ok(true) => {}
            Ok(false) => return None,
            Err(Unknown) => return Some(retry_later()),
        }
        let who = self.who(id)?;
        // the view travels in the old key's header: pigeon's own token in the query would be refused there
        let path_q = match crate::http::query_without(req.uri.query(), crate::paseo::compat::VIEW_AS_QUERY) {
            Some(q) if !q.is_empty() => format!("{}?{q}", req.uri.path()),
            _ => req.uri.path().to_owned(),
        };
        let mut b = self.request(Method::GET, &path_q, &who);
        for name in [
            header::RANGE,
            header::IF_NONE_MATCH,
            header::IF_MODIFIED_SINCE,
            header::IF_RANGE,
        ] {
            if let Some(v) = req.headers.get(&name) {
                b = b.header(name, v.clone());
            }
        }
        let guard = app.0.transfers.guard();
        match tokio::time::timeout(Duration::from_secs(30), b.send()).await {
            Ok(Ok(r)) if matches!(r.status().as_u16(), 401 | 403) => {
                self.0.metrics.lu_401.inc();
                Some(signed_out())
            }
            Ok(Ok(r)) => Some(crate::blobs::relay(r, guard, None)),
            _ => {
                self.0.metrics.lu_errors.inc();
                Some(api_error(
                    StatusCode::BAD_GATEWAY,
                    "UPSTREAM_UNAVAILABLE",
                    "The file could not be read.",
                ))
            }
        }
    }
}

/// An answer that depends on an owner pigeon could not read: retry shortly.
fn retry_later() -> Response {
    let mut r = api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "UPSTREAM_UNAVAILABLE",
        "Please try again in a moment.",
    );
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
    r
}

fn signed_out() -> Response {
    api_error(StatusCode::UNAUTHORIZED, "NOT_SIGNED_IN", "Not signed in.")
}

/// The old router's answer as the tab's: its status and JSON body; a 401 as pigeon's signed-out answer.
fn rest_answer(r: Result<(u16, Bytes), CallError>) -> Response {
    let (status, body) = match r {
        Ok(ok) => ok,
        Err(CallError::Status(s, b)) => (s, b),
        Err(CallError::SignedOut) => return signed_out(),
        Err(CallError::Unreachable | CallError::AppliedUnread(_)) => {
            return api_error(
                StatusCode::BAD_GATEWAY,
                "SHARE_FAILED",
                "The share could not be completed.",
            );
        }
    };
    let mut resp = Response::new(axum::body::Body::from(body));
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}
