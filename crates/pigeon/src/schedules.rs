//! Schedules (magpie's [`magpie_api::schedules`]): the app's panel of one conversation
//! (`GET <sessions>/{id}/schedules` under the compat API, the same shape on the old stack's router), and the admin's page
//! `/ops/schedules` (English, Deyao only: everyone's schedules with their runs, and the pause-all switch).

use std::fmt::Write as _;

use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use magpie_api::errors::ErrorCode;
use magpie_api::schedules::{AdminSchedules, PauseAll, Schedule};

use crate::App;
use crate::http::{api_error, escape, json, person_calls, redirect, same_origin, small_body};
use crate::identity::{Identity, Person};

/// The admin page's path.
pub const ADMIN_PATH: &str = "/ops/schedules";
/// The pause-all switch's form target.
pub const PAUSE_PATH: &str = "/ops/schedules/pause";

/// `GET <sessions>/{id}/schedules`: `{"schedules": [...]}`, each with its last runs (the switch window: a
/// legacy conversation's from the old child).
pub async fn conversation(app: &App, req: &Parts, id: &Identity, conversation: &str) -> Response {
    let not_found = || api_error(StatusCode::NOT_FOUND, "SESSION_NOT_FOUND", "Conversation not found.");
    let Ok(cid) = uuid::Uuid::parse_str(conversation) else {
        return not_found();
    };
    let Some(calls) = person_calls(&app.0.magpie, id) else {
        return not_found();
    };
    if let Some(r) = app.legacy().schedules(app, req, id, cid).await {
        return r;
    }
    match calls.conversation_schedules(cid).await {
        Ok(list) => json(StatusCode::OK, &serde_json::to_value(&list).unwrap_or_default()),
        Err(e) if e.code() == Some(&ErrorCode::NotFound) => not_found(),
        Err(e) => {
            tracing::warn!(error = %e, "schedules: magpie failed");
            api_error(
                StatusCode::BAD_GATEWAY,
                "SCHEDULES_FAILED",
                "The schedules could not be read.",
            )
        }
    }
}

/// `/ops/schedules` (GET) and `/ops/schedules/pause` (a same-origin form POST), for a verified admin.
pub async fn admin(app: &App, req: Request, path: &str, admin: &Person) -> Response {
    let calls = app.0.magpie.service(Some(&admin.email));
    if path == PAUSE_PATH && req.method() == Method::POST {
        if !same_origin(req.headers(), false) {
            return text_page(StatusCode::FORBIDDEN, "Cross-origin requests are refused.");
        }
        let Some(body) = small_body(req.into_body(), 1024).await else {
            return text_page(StatusCode::PAYLOAD_TOO_LARGE, "Too large.");
        };
        let body = String::from_utf8_lossy(&body);
        let paused = body.split('&').any(|kv| kv == "paused=1");
        return match calls.admin_schedules_pause(&PauseAll { paused }).await {
            Ok(_) => {
                tracing::info!(paused, "schedules: pause-all set by an admin");
                redirect(StatusCode::SEE_OTHER, ADMIN_PATH, false)
            }
            Err(e) => {
                tracing::warn!(error = %e, "schedules: pause-all not set");
                text_page(StatusCode::BAD_GATEWAY, "magpie did not take the change.")
            }
        };
    }
    if path != ADMIN_PATH || !(req.method() == Method::GET || req.method() == Method::HEAD) {
        return text_page(StatusCode::NOT_FOUND, "Not found.");
    }
    match calls.admin_schedules().await {
        Ok(v) => html(StatusCode::OK, render(&v)),
        Err(e) => {
            tracing::warn!(error = %e, "schedules: magpie failed");
            text_page(StatusCode::BAD_GATEWAY, "magpie did not answer.")
        }
    }
}

fn html(status: StatusCode, body: String) -> Response {
    let mut r = (status, body).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; form-action 'self'"),
    );
    r
}

fn text_page(status: StatusCode, message: &str) -> Response {
    html(
        status,
        format!(
            "<!doctype html><meta charset=utf-8><title>Schedules</title><p>{}</p>",
            escape(message)
        ),
    )
}

/// A run's outcome in words.
#[must_use]
pub fn outcome_words(o: &str) -> &'static str {
    match o {
        "sent" => "sent (a new turn)",
        "sent_mid_turn" => "sent into the running turn",
        "pending" => "being delivered into the running turn",
        "redeliver" => "to be sent again (the turn ended first)",
        "skipped_late" => "skipped: the service was down for more than 10 minutes",
        "skipped_paused" => "skipped: all schedules paused",
        "skipped_deleted" => "skipped: the conversation is in Recently deleted",
        "failed" => "failed",
        _ => "unknown",
    }
}

fn card(out: &mut String, s: &Schedule) {
    let kind = if s.kind == "cron" { "repeating" } else { "once" };
    let _ = write!(
        out,
        "<section><h3>{} <small>{kind} · {}</small></h3><dl>",
        escape(&s.describe),
        escape(&s.state)
    );
    let mut row = |k: &str, v: &str| {
        let _ = write!(out, "<dt>{k}</dt><dd>{}</dd>", escape(v));
    };
    row("Next", s.next.as_deref().unwrap_or("none"));
    row("Message", &s.prompt);
    let title = s.conversation_title.as_deref().unwrap_or("(untitled)");
    row("Conversation", &format!("{title} · {}", s.conversation_id));
    if let Some(c) = &s.cron {
        row("Rule", &format!("{c} ({})", s.tz));
    } else if let Some(a) = &s.at {
        row("At", &format!("{a} ({})", s.tz));
    }
    if let Some(u) = &s.until {
        row("Until", u);
    }
    row("Set", &format!("{} · {}", s.created_by, s.created_at));
    row("Id", &s.id);
    out.push_str("</dl><h4>Runs</h4><ul>");
    let runs = s.runs.as_deref().unwrap_or_default();
    if runs.is_empty() {
        out.push_str("<li>none yet</li>");
    }
    for r in runs {
        let _ = write!(
            out,
            "<li><b>{}</b> {}{}</li>",
            escape(&r.due),
            outcome_words(&r.outcome),
            r.detail
                .as_deref()
                .map(|d| format!(" ({})", escape(d)))
                .unwrap_or_default()
        );
    }
    out.push_str("</ul></section>");
}

/// The admin page.
#[must_use]
pub fn render(v: &AdminSchedules) -> String {
    let mut out = String::from(
        "<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport \
         content=\"width=device-width,initial-scale=1\"><title>Schedules</title><style>\
         :root{--ink:#111;--line:#d4d4d4;--muted:#666;--bg:#fff;--warn:#b42318}\
         @media (prefers-color-scheme:dark){:root{--ink:#eee;--line:#3a3a3a;--muted:#999;--bg:#111;--warn:#ff8a80}}\
         body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.45 system-ui,sans-serif}\
         main{max-width:1100px;margin:0 auto;padding:16px}h1{font-size:22px;margin:0 0 4px}\
         h2{font-size:17px;border-bottom:1px solid var(--ink);padding-bottom:4px;margin:28px 0 8px}\
         h3{font-size:16px;margin:0 0 6px}h3 small{font-weight:400;color:var(--muted)}h4{margin:10px 0 4px;font-size:13px}\
         section{border:1px solid var(--line);padding:12px;margin:8px 0}dl{display:grid;\
         grid-template-columns:110px 1fr;gap:2px 10px;margin:0}dt{color:var(--muted)}dd{margin:0;overflow-wrap:anywhere}\
         ul{margin:0;padding-left:18px;font-size:13px}button{font:inherit;padding:8px 14px;border:1px solid var(--ink);\
         background:var(--bg);color:var(--ink);border-radius:0;cursor:pointer}.paused{color:var(--warn);font-weight:600}\
         a{color:inherit}</style></head><body><main><h1>Schedules</h1>\
         <p>Every person's schedules with their last runs. <a href=\"/ops/\">Operator page</a></p>",
    );
    if v.paused_all {
        let _ = write!(
            out,
            "<p class=paused>All schedules are paused{}{}. Due times are recorded as skipped.</p>\
             <form method=post action=\"{PAUSE_PATH}\"><input type=hidden name=paused value=0>\
             <button>Resume all schedules</button></form>",
            v.paused_by
                .as_deref()
                .map(|b| format!(" by {}", escape(b)))
                .unwrap_or_default(),
            v.paused_at
                .as_deref()
                .map(|a| format!(" at {}", escape(a)))
                .unwrap_or_default()
        );
    } else {
        let _ = write!(
            out,
            "<p>Schedules fire.</p><form method=post action=\"{PAUSE_PATH}\"><input type=hidden name=paused value=1>\
             <button>Pause all schedules</button></form>"
        );
    }
    if v.schedules.is_empty() {
        out.push_str("<p>No schedules.</p>");
    }
    let mut person = "";
    for s in &v.schedules {
        if s.created_by != person {
            person = &s.created_by;
            let _ = write!(out, "<h2>{}</h2>", escape(person));
        }
        card(&mut out, s);
    }
    out.push_str("</main></body></html>");
    out
}

#[cfg(test)]
mod tests {
    use magpie_api::schedules::{AdminSchedules, Schedule, ScheduleRun};

    use super::render;

    #[test]
    fn the_page_escapes_and_offers_the_switch() {
        let s = Schedule {
            id: "sch_1".into(),
            conversation_id: uuid::Uuid::nil(),
            kind: "cron".into(),
            prompt: "<b>news</b>".into(),
            at: None,
            cron: Some("30 8 * * 1-5".into()),
            tz: "Asia/Hong_Kong".into(),
            until: None,
            describe: "每個工作日 08:30".into(),
            next: Some("2026-11-09T08:30:00+08:00".into()),
            state: "active".into(),
            created_by: "a@example.com".into(),
            created_at: "2026-11-06T09:00:00+08:00".into(),
            conversation_title: Some("每日新聞".into()),
            runs: Some(vec![ScheduleRun {
                due: "2026-11-06T08:30:00+08:00".into(),
                at: "2026-11-06T08:30:01+08:00".into(),
                outcome: "skipped_late".into(),
                detail: None,
            }]),
        };
        let page = render(&AdminSchedules {
            paused_all: false,
            paused_by: None,
            paused_at: None,
            schedules: vec![s],
        });
        assert!(page.contains("&lt;b&gt;news&lt;/b&gt;") && !page.contains("<b>news"));
        assert!(page.contains("Pause all schedules") && page.contains("name=paused value=1"));
        assert!(page.contains("skipped: the service was down for more than 10 minutes"));
        let paused = render(&AdminSchedules {
            paused_all: true,
            paused_by: Some("deyao@example.com".into()),
            paused_at: None,
            schedules: vec![],
        });
        assert!(paused.contains("Resume all schedules") && paused.contains("by deyao@example.com"));
    }
}
