//! The raw view (Deyao, 2026-11-05: "a more raw outputs like the transcript view or the subagent view, in addition to
//! the normal ui"): beside the normal conversation of a view-as tab, every line of the Claude Code transcript in
//! order, each as a typed cell (user, assistant, thinking, tool call, tool result, system, compaction, interrupt…)
//! with its time and its content as text. Admin only and read-only. English, as everything only Deyao sees.
//!
//! Standalone (Deyao, 2026-11-05: "just work standalone just with the admin identity"): the page and its data need
//! only the admin's own Cloudflare Access sign-in (the assertion header, the `CF_Authorization` cookie or the app's
//! header), checked on every request; no view-as token, no prior visit to the app.
//!
//! - `GET /raw/<conversation id>` (the bare uuid or `wks_<uuid>`): the page (no data in it; it polls the lines
//!   route: every 2 s while a turn runs, every 10 s otherwise). Signed out, it sends the browser once through the
//!   Access-covered sign-in route and back (`?signin=1` marks the return, so it never loops); a signed-in non-admin
//!   gets the admins-only page.
//! - `GET /raw/<id>/lines?after=<ord>`: one page of cells with the owner and title, from magpie's
//!   `GET /v1/admin/conversations/{id}/transcript` (pigeon names the admin in `Aviary-Admin`; magpie records the
//!   admin's first read per conversation and day in the view-as log).
//! - `GET <compat::SESSIONS><id>/transcript?after=<ord>&<compat::VIEW_AS_QUERY>=…`: the same cells for a view-as
//!   identity, from magpie's `GET /v1/conversations/{id}/transcript`.
//! - The shell carries a small script ([`toggle_script`]) that shows a "Raw transcript" button in a view-as tab whose
//!   address names a conversation; nothing shows in a normal tab.

pub mod cells;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use magpie_api::errors::ErrorCode;
use magpie_api::rows::TranscriptQuery;
use serde_json::json;

use crate::App;
use crate::http::{Page, api_error, json, page, person_calls, query_param, redirect};
use crate::identity::{Identity, Sources};
use crate::paseo::compat;

/// The page's path prefix.
pub const PAGE_PREFIX: &str = "/raw/";

/// The page (static; data comes from the transcript route).
const PAGE_TEMPLATE: &str = include_str!("page.html");

/// The script the shell carries before `</body>` (signed-in variant): the raw view's button in a view-as tab.
const TOGGLE_TEMPLATE: &str = include_str!("toggle.html");

/// A template with the app's wire names filled in (they live in `paseo::compat` only).
fn fill(t: &str) -> String {
    t.replace("{{STORAGE_KEY}}", compat::VIEW_AS_STORAGE)
        .replace("{{HASH_KEY}}", compat::VIEW_AS_FRAGMENT.trim_end_matches('='))
        .replace("{{SESSIONS}}", compat::SESSIONS)
        .replace("{{VIEW_AS_QUERY}}", compat::VIEW_AS_QUERY)
}

/// The page.
#[must_use]
pub fn page_html() -> &'static str {
    static P: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| fill(PAGE_TEMPLATE));
    &P
}

/// The shell's button script.
#[must_use]
pub fn toggle_script() -> &'static str {
    static T: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| fill(TOGGLE_TEMPLATE).trim_end().to_owned());
    &T
}

fn not_available() -> Response {
    api_error(
        StatusCode::NOT_FOUND,
        "NOT_AVAILABLE",
        "This feature is not available here.",
    )
}

/// A conversation id as the raw page's address carries it: the bare uuid, or the app's workspace form `wks_<uuid>`
/// (the address an admin copies from a view-as tab's path).
fn conversation_id(s: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(s.strip_prefix("wks_").unwrap_or(s)).ok()
}

/// The lines route's suffix after the conversation id.
const LINES: &str = "/lines";

/// What a `/raw/…` path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The page of this conversation.
    Page(uuid::Uuid),
    /// Its lines.
    Lines(uuid::Uuid),
}

fn target(path: &str) -> Option<Target> {
    let rest = path.strip_prefix(PAGE_PREFIX)?;
    if let Some(id) = rest.strip_suffix(LINES) {
        return conversation_id(id).map(Target::Lines);
    }
    conversation_id(rest.trim_end_matches('/')).map(Target::Page)
}

/// The admin a request comes from (its own Access sign-in from any source; no view-as), or why not.
enum Caller {
    Admin(String),
    SignedOut,
    NotAdmin,
    Unavailable,
}

async fn caller(app: &App, headers: &HeaderMap) -> Caller {
    match app.0.identity.resolve(headers, Sources::All, None).await {
        Identity::Person(p) if p.is_admin() => Caller::Admin(p.email),
        Identity::Person(_) | Identity::NotAllowed(_) => Caller::NotAdmin,
        Identity::Unavailable => Caller::Unavailable,
        _ => Caller::SignedOut,
    }
}

/// The marker on the address the sign-in route sends the browser back to.
const SIGNED_IN_MARK: &str = "signin";

/// The sign-in route (Access-covered at the edge) with `next` back to this page, marked.
fn signin_location(id: uuid::Uuid) -> String {
    // the next path holds only `/`, `?`, `=`, letters, digits and `-`: those three are escaped
    format!("{}?next=%2Fraw%2F{id}%3F{SIGNED_IN_MARK}%3D1", compat::SIGNIN)
}

/// `GET /raw/<id>` and `GET /raw/<id>/lines`.
pub async fn serve(app: &App, headers: &HeaderMap, path: &str, query: Option<&str>) -> Response {
    match target(path) {
        None => page(app, Page::NotAvailable, None),
        Some(Target::Page(id)) => serve_page(app, headers, id, query).await,
        Some(Target::Lines(id)) => lines(app, headers, id, query).await,
    }
}

/// The page, for an admin.
async fn serve_page(app: &App, headers: &HeaderMap, id: uuid::Uuid, query: Option<&str>) -> Response {
    match caller(app, headers).await {
        Caller::Admin(_) => {}
        Caller::NotAdmin => return page(app, Page::AdminsOnly, None),
        Caller::Unavailable => return page(app, Page::Unavailable, None),
        Caller::SignedOut if query_param(query, SIGNED_IN_MARK).is_some() => {
            return page(app, Page::SignedOut, None);
        }
        Caller::SignedOut => return redirect(StatusCode::FOUND, &signin_location(id), true),
    }
    let mut r = (StatusCode::OK, page_html()).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    r
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn busy() -> Response {
    let mut r = api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "UNAVAILABLE",
        "The service is busy. Try again shortly.",
    );
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    r
}

fn query_of(query: Option<&str>) -> TranscriptQuery {
    TranscriptQuery {
        after: query_param(query, "after").and_then(|a| a.parse().ok()),
        limit: query_param(query, "limit").and_then(|a| a.parse().ok()),
    }
}

/// `GET /raw/<id>/lines`: a page of cells with the owner and title, for an admin (magpie's admin route).
async fn lines(app: &App, headers: &HeaderMap, cid: uuid::Uuid, query: Option<&str>) -> Response {
    let admin = match caller(app, headers).await {
        Caller::Admin(a) => a,
        Caller::NotAdmin => {
            return api_error(StatusCode::FORBIDDEN, "ADMINS_ONLY", "Only an admin can read this.");
        }
        Caller::Unavailable => return busy(),
        Caller::SignedOut => return api_error(StatusCode::UNAUTHORIZED, "NOT_SIGNED_IN", "Not signed in."),
    };
    let calls = app.0.magpie.service(Some(&admin));
    let calls = match aviary_core::http::current_request_id() {
        Some(rid) => calls.with_request_id(rid.to_string()),
        None => calls,
    };
    match calls.admin_transcript(cid, &query_of(query)).await {
        Ok(p) => {
            let shown: Vec<_> = p.lines.iter().flat_map(|l| cells::of_line(l.ord, &l.line)).collect();
            no_store(json(
                StatusCode::OK,
                &json!({"path": p.path, "cells": shown, "next": p.next, "more": p.more, "running": p.running,
                        "person": p.person, "title": p.title}),
            ))
        }
        Err(e) if e.code() == Some(&ErrorCode::NotFound) => {
            no_store(api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Conversation not found."))
        }
        Err(e) => {
            tracing::warn!(error = %e, "raw transcript (admin): magpie failed");
            busy()
        }
    }
}

/// `GET …/sessions/{id}/transcript`: a page of cells, only for a view-as identity.
pub async fn transcript(app: &App, id: &Identity, conversation: &str, query: Option<&str>) -> Response {
    if !matches!(id, Identity::ViewAs(_)) {
        return not_available();
    }
    let Some(cid) = conversation_id(conversation) else {
        return not_available();
    };
    let Some(calls) = person_calls(&app.0.magpie, id) else {
        return not_available();
    };
    match calls.transcript(cid, &query_of(query)).await {
        Ok(p) => {
            let shown: Vec<_> = p.lines.iter().flat_map(|l| cells::of_line(l.ord, &l.line)).collect();
            let mut r = json(
                StatusCode::OK,
                &json!({"path": p.path, "cells": shown, "next": p.next, "more": p.more, "running": p.running}),
            );
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(e) if e.code() == Some(&ErrorCode::NotFound) => {
            api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Conversation not found.")
        }
        Err(e) => {
            tracing::warn!(error = %e, "raw transcript: magpie failed");
            busy()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_placeholder_is_filled() {
        for t in [page_html(), toggle_script()] {
            assert!(!t.contains("{{"), "a placeholder left");
        }
        assert!(page_html().contains("/lines"), "the page reads the lines route");
        assert!(!page_html().contains(compat::VIEW_AS_QUERY), "and needs no view-as token");
        assert!(toggle_script().contains(compat::VIEW_AS_STORAGE));
    }

    #[test]
    fn the_address_takes_the_bare_uuid_or_the_workspace_form() {
        let id = "68abe986-9abb-4157-9ac4-e074670c70db";
        assert_eq!(conversation_id(id).map(|u| u.to_string()).as_deref(), Some(id));
        assert_eq!(conversation_id(&format!("wks_{id}")).map(|u| u.to_string()).as_deref(), Some(id));
        assert!(conversation_id("wks_nope").is_none());
        assert!(conversation_id("xyz_68abe986-9abb-4157-9ac4-e074670c70db").is_none());
    }

    #[test]
    fn the_paths_name_the_page_or_its_lines() {
        let id = "68abe986-9abb-4157-9ac4-e074670c70db";
        let u = uuid::Uuid::parse_str(id).unwrap();
        assert_eq!(target(&format!("/raw/{id}")), Some(Target::Page(u)));
        assert_eq!(target(&format!("/raw/wks_{id}/")), Some(Target::Page(u)));
        assert_eq!(target(&format!("/raw/wks_{id}/lines")), Some(Target::Lines(u)));
        assert_eq!(target(&format!("/raw/{id}/other")), None);
        assert_eq!(target("/raw/"), None);
        // the sign-in route's next is a safe same-origin path back to the page, marked
        let loc = signin_location(u);
        let query = loc.split_once('?').map(|x| x.1);
        let next = crate::http::query_param(query, "next").unwrap();
        assert_eq!(next, format!("/raw/{id}?signin=1"));
        assert!(crate::signin::safe_next(&next));
    }
}
