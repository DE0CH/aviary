//! Sign-in (spec 5.3) and `GET …/me` (5.4).

use std::fmt::Write as _;

use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};

use crate::App;
use crate::http::{Page, is_signed_in_path, json, not_found, page, query_param, redirect};
use crate::identity::{ACCESS_COOKIE, ASSERTION_HEADER, Identity, cookie};

/// Whether `next` is a same-origin path pigeon may send a fresh sign-in to (spec 5.3.1): `^/(?![/\\])\S*$`, and its
/// path part is not under a signed-in prefix.
#[must_use]
pub fn safe_next(next: &str) -> bool {
    let Some(rest) = next.strip_prefix('/') else {
        return false;
    };
    if rest.starts_with('/') || rest.starts_with('\\') || next.chars().any(char::is_whitespace) {
        return false;
    }
    let path = next.split(['?', '#']).next().unwrap_or(next);
    !is_signed_in_path(path)
}

/// `GET …/signin?next=` (5.3.1): the person has just signed in at Access; send them back.
#[must_use]
pub fn signin(query: Option<&str>) -> Response {
    let next = query_param(query, "next").unwrap_or_default();
    let to = if safe_next(&next) { next.as_str() } else { "/" };
    redirect(StatusCode::FOUND, to, true)
}

/// Whether `state` is spec 5.3.3's `^[A-Za-z0-9_-]{16,128}$`.
#[must_use]
pub fn valid_state(state: &str) -> bool {
    (16..=128).contains(&state.len())
        && state
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn fragment_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `GET …/app-signin?state=` (5.3.3): hands the iOS app the Access token through its URL scheme.
#[must_use]
pub fn app_signin(app: &App, req: &Parts, id: &Identity) -> Response {
    let Some(scheme) = app.0.identity.facts().and_then(|f| f.sign_in.app_scheme.clone()) else {
        // the browser-facing 404 page of 5.2.3's kind: the route is opened in the system browser, not fetched
        return page(app, Page::NotAvailable, None);
    };
    if req.method != Method::GET {
        return page(app, Page::ReadOnly, None);
    }
    let state = query_param(req.uri.query(), "state").unwrap_or_default();
    if !valid_state(&state) {
        return crate::http::api_error(StatusCode::BAD_REQUEST, "BAD_STATE", "Bad sign-in state.");
    }
    let person = match id {
        Identity::Person(p) => p,
        Identity::SignedOut | Identity::ViewAs(_) | Identity::ViewAsRefused => {
            return crate::http::api_error(StatusCode::UNAUTHORIZED, "NOT_SIGNED_IN", "Not signed in.");
        }
        Identity::NotAllowed(_) => {
            return crate::http::api_error(
                StatusCode::FORBIDDEN,
                "NOT_ALLOWED",
                "This account may not use this service.",
            );
        }
        Identity::Unavailable => {
            return crate::http::api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "UNAVAILABLE",
                "The service is busy. Try again shortly.",
            );
        }
    };
    let token = cookie(&req.headers, ACCESS_COOKIE)
        .or_else(|| {
            req.headers
                .get(ASSERTION_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| person.token.clone());
    tracing::info!(person = %aviary_core::log::person_key(&person.email), "app-signin");
    redirect(
        StatusCode::FOUND,
        &format!("{scheme}://signed-in?state={state}#token={}", fragment_encode(&token)),
        true,
    )
}

/// `GET …/me` (5.4): the greeting's first name, and who is being viewed in view-as.
pub async fn me(app: &App, id: &Identity) -> Response {
    let body: Value = match id {
        Identity::Person(p) => json!({"firstName": app.0.identity.first_name(p).await, "sharing": true}),
        Identity::ViewAs(v) => {
            let expires = time::OffsetDateTime::from_unix_timestamp(v.exp)
                .map(|t| magpie_api::Timestamp(t).to_wire())
                .unwrap_or_default();
            json!({
                "firstName": app.0.identity.cached_first_name(&v.person).await,
                "sharing": true,
                "viewAs": {"user": v.person, "expiresAt": expires},
            })
        }
        _ => return not_found(),
    };
    json(StatusCode::OK, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_rules() {
        assert!(safe_next("/"));
        assert!(safe_next("/h/x/agent/1?a=b#c"));
        assert!(!safe_next("//evil.example"));
        assert!(!safe_next("/\\evil.example"));
        assert!(!safe_next("https://evil.example"));
        assert!(!safe_next("/a b"));
        assert!(!safe_next("/api/x"));
        assert!(!safe_next("/ops"));
        assert!(!safe_next("/paseo-ws?x"));
        assert!(safe_next("/apix"));
        assert!(!safe_next(""));
    }

    #[test]
    fn states() {
        assert!(valid_state("abcdefghijklmnop"));
        assert!(!valid_state("short"));
        assert!(!valid_state("abcdefghijklmnop!"));
        assert!(!valid_state(&"a".repeat(129)));
    }

    #[test]
    fn fragment() {
        assert_eq!(fragment_encode("a.b-c_d~"), "a.b-c_d~");
        assert_eq!(fragment_encode("a+b/c="), "a%2Bb%2Fc%3D");
    }
}
