//! A legacy conversation's REST calls (DESIGN 9.1, 9.4-9.5): its warm calls before the gate (a ticketed write; from
//! the gate a warm is a hermit hint only, so typing does not keep resetting its quiet period), its share links
//! (listed, created and revoked through the old child, the writes under tickets) and its files (the old child's).

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
use super::{Legacy, OLD_WARM};
use crate::App;
use crate::http::{api_error, person_calls};
use crate::identity::Identity;

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

/// The outcome of a ticketed REST write.
enum Ticketed {
    /// magpie owns the conversation: the caller does it natively.
    Native,
    /// The old router's answer.
    Done(Result<(u16, Bytes), CallError>),
}

impl Legacy {
    /// Whether the conversation (of the request's person) is legacy, looked up when pigeon has not read it while the
    /// person has legacy conversations.
    pub async fn rest_is_legacy(&self, app: &App, id: &Identity, conversation: Uuid) -> bool {
        let Some(person) = id.subject() else { return false };
        if let Some(l) = self.is_legacy(person, &conversation) {
            return l;
        }
        if !self.window(person).await.is_ok_and(|w| w.person_legacy > 0) {
            return false;
        }
        match person_calls(&app.0.magpie, id).map(|c| async move { c.get_conversation(conversation).await }) {
            Some(f) => match f.await {
                Ok(c) => {
                    self.observe(person, &c);
                    c.migrating
                }
                Err(_) => false,
            },
            None => false,
        }
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
                (TicketRoute::Legacy, None) => return Ticketed::Done(Err(CallError::Unreachable)),
            };
            let report = |outcome: TicketOutcome| {
                let calls = calls.clone();
                async move {
                    for _ in 0..4 {
                        if calls.legacy_ticket_done(ticket, &TicketDone { outcome }).await.is_ok() {
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
                Ok(_) => report(TicketOutcome::Applied).await,
                Err(CallError::Status(..) | CallError::SignedOut) => report(TicketOutcome::Refused).await,
                // no answer: it may have been applied; the importer expires the ticket after its settle time
                Err(CallError::Unreachable) => {}
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
        tokio::spawn(async move {
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
        if !self.rest_is_legacy(app, id, conversation).await {
            return None;
        }
        let who = self.who(id)?;
        let path = req.uri.path().to_owned();
        if req.method == Method::POST {
            let call = || self.request(Method::POST, &path, &who);
            return match self
                .ticketed_rest(app, id, conversation, TicketKind::ShareCreate, call)
                .await
            {
                Ticketed::Native => None,
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

    /// A revoke of a share whose conversation is legacy (magpie answered 409 `route: "legacy"` with the
    /// conversation, or the share was made through the legacy upstream here). `None`: not one of those.
    pub async fn revoke(
        &self,
        app: &App,
        req: &Parts,
        id: &Identity,
        share: &str,
        conversation: Option<Uuid>,
    ) -> Option<Response> {
        let conversation = conversation.or_else(|| self.0.lu_shares.get(share))?;
        let who = self.who(id)?;
        let path = req.uri.path().to_owned();
        let call = || self.request(Method::DELETE, &path, &who);
        match self
            .ticketed_rest(app, id, conversation, TicketKind::ShareRevoke, call)
            .await
        {
            Ticketed::Native => None,
            Ticketed::Done(r) => Some(rest_answer(r)),
        }
    }

    /// A legacy conversation's file (`files/raw`): the old child's answer, streamed, with the range headers passed.
    pub async fn files_raw(&self, app: &App, req: &Parts, id: &Identity, conversation: Uuid) -> Option<Response> {
        if !self.rest_is_legacy(app, id, conversation).await {
            return None;
        }
        let who = self.who(id)?;
        let path_q = req
            .uri
            .path_and_query()
            .map_or_else(|| req.uri.path().to_owned(), |p| p.as_str().to_owned());
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

fn signed_out() -> Response {
    api_error(StatusCode::UNAUTHORIZED, "NOT_SIGNED_IN", "Not signed in.")
}

/// The old router's answer as the tab's: its status and JSON body; a 401 as pigeon's signed-out answer.
fn rest_answer(r: Result<(u16, Bytes), CallError>) -> Response {
    let (status, body) = match r {
        Ok(ok) => ok,
        Err(CallError::Status(s, b)) => (s, b),
        Err(CallError::SignedOut) => return signed_out(),
        Err(CallError::Unreachable) => {
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
