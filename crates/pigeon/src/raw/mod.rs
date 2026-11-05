//! The raw view (Deyao, 2026-11-05: "a more raw outputs like the transcript view or the subagent view, in addition to
//! the normal ui"): beside the normal conversation of a view-as tab, every line of the Claude Code transcript in
//! order, each as a typed cell (user, assistant, thinking, tool call, tool result, system, compaction, interrupt…)
//! with its time and its content as text. Admin only and read-only: the data route answers only a view-as identity
//! (an admin's own Access sign-in plus a valid view token, spec 6.2, checked on every request), the page only an
//! admin's cookie. English, as everything only Deyao sees.
//!
//! - `GET /raw/<conversation id>`: the page (no data in it; it reads the tab's view-as token from `sessionStorage`,
//!   where the app keeps it, and polls the data route: every 2 s while a turn runs, every 10 s otherwise).
//! - `GET <compat::SESSIONS><id>/transcript?after=<ord>&<compat::VIEW_AS_QUERY>=…`: one page of cells, from magpie's
//!   `GET /v1/conversations/{id}/transcript` (the main transcript's lines; long text whole, media as a placeholder).
//! - The shell carries a small script ([`toggle_script`]) that shows a "Raw transcript" button in a view-as tab whose
//!   address names a conversation; nothing shows in a normal tab.

pub mod cells;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use magpie_api::errors::ErrorCode;
use magpie_api::rows::TranscriptQuery;
use serde_json::json;

use crate::App;
use crate::http::{Page, api_error, json, page, person_calls, query_param};
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

/// `GET /raw/<id>`: the page, for an admin's cookie.
pub async fn serve_page(app: &App, headers: &HeaderMap, path: &str) -> Response {
    let ok_id = path
        .strip_prefix(PAGE_PREFIX)
        .is_some_and(|id| uuid::Uuid::parse_str(id.trim_end_matches('/')).is_ok());
    if !ok_id {
        return page(app, Page::NotAvailable, None);
    }
    match app.0.identity.resolve(headers, Sources::CookieOnly, None).await {
        Identity::Person(p) if p.is_admin() => {}
        Identity::Person(_) | Identity::NotAllowed(_) => return page(app, Page::AdminsOnly, None),
        Identity::Unavailable => return page(app, Page::Unavailable, None),
        _ => return page(app, Page::SignedOut, None),
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

/// `GET …/sessions/{id}/transcript`: a page of cells, only for a view-as identity.
pub async fn transcript(app: &App, id: &Identity, conversation: &str, query: Option<&str>) -> Response {
    if !matches!(id, Identity::ViewAs(_)) {
        return not_available();
    }
    let Ok(cid) = uuid::Uuid::parse_str(conversation) else {
        return not_available();
    };
    let Some(calls) = person_calls(&app.0.magpie, id) else {
        return not_available();
    };
    let q = TranscriptQuery {
        after: query_param(query, "after").and_then(|a| a.parse().ok()),
        limit: query_param(query, "limit").and_then(|a| a.parse().ok()),
    };
    match calls.transcript(cid, &q).await {
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
            let mut r = api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "UNAVAILABLE",
                "The service is busy. Try again shortly.",
            );
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            r
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
        assert!(page_html().contains(compat::SESSIONS));
        assert!(toggle_script().contains(compat::VIEW_AS_STORAGE));
    }
}
