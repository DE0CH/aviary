//! `/ops`, `/ops/*` (spec 5.11): the operator page, for admins only and read-only, forwarded to hermit's own
//! `/ops/*` pages with pigeon's pair token and the admin's email.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, Method, StatusCode, header};
use axum::response::Response;

use crate::App;
use crate::http::{Page, encode_path, page, query_param, query_without, redirect};
use crate::identity::{Identity, Sources};
use crate::paseo::compat;

/// The answer headers passed back to the browser.
const PASSED: [HeaderName; 6] = [
    header::CONTENT_TYPE,
    header::CACHE_CONTROL,
    header::CONTENT_SECURITY_POLICY,
    header::CONTENT_ENCODING,
    header::X_CONTENT_TYPE_OPTIONS,
    header::CONTENT_LENGTH,
];

/// Handles a request whose normalised path is `/ops` or under `/ops/`.
pub async fn handle(app: &App, req: Request, path: &str) -> Response {
    let query = req.uri().query().map(str::to_owned);
    let view_as = query_param(query.as_deref(), compat::VIEW_AS_QUERY);
    let id = app
        .0
        .identity
        .resolve(req.headers(), Sources::All, view_as.as_deref())
        .await;
    let admin = match &id {
        Identity::Person(p) if p.is_admin() => p.clone(),
        Identity::SignedOut => return page(app, Page::SignedOut, None),
        Identity::NotAllowed(email) => return page(app, Page::NotAllowed, Some(email)),
        Identity::Unavailable => return page(app, Page::Unavailable, None),
        other => {
            // a staff member, or an admin's view-as tab (whose identity is the person)
            let who = other.subject().map(aviary_core::log::person_key).unwrap_or_default();
            tracing::warn!(person = %who, "ops refused: not an admin");
            return page(app, Page::AdminsOnly, None);
        }
    };
    // the schedules page and its pause-all switch (the one write under /ops, a same-origin form)
    if path == crate::schedules::ADMIN_PATH || path.starts_with("/ops/schedules/") {
        return crate::schedules::admin(app, req, path, &admin).await;
    }
    let method = req.method().clone();
    if method != Method::GET && method != Method::HEAD {
        return page(app, Page::ReadOnly, None);
    }
    let query = query_without(query.as_deref(), compat::VIEW_AS_QUERY);
    if path == "/ops" {
        // the query stays (spec 5.11 `/ops?...`), without the view-as token
        let to = match &query {
            Some(q) => format!("/ops/?{q}"),
            None => "/ops/".to_owned(),
        };
        return redirect(StatusCode::FOUND, &to, false);
    }
    // the checked path, encoded again: nothing in it can become a dot segment, a query or a fragment upstream
    let path = encode_path(path);
    let url = match &query {
        Some(q) => format!("{}{path}?{q}", app.0.upstreams.hermit),
        None => format!("{}{path}", app.0.upstreams.hermit),
    };
    let mut up = app
        .0
        .hermit_http
        .request(method, url)
        .bearer_auth(&app.0.secrets.token_hermit)
        .header(hermit_api::headers::PERSON, &admin.email);
    if let Some(id) = aviary_core::http::current_request_id() {
        up = up.header(hermit_api::headers::REQUEST, id.to_string());
    }
    for name in [header::ACCEPT, header::ACCEPT_ENCODING] {
        if let Some(v) = req.headers().get(&name) {
            up = up.header(name, v.clone());
        }
    }
    match up.send().await {
        Ok(res) if res.status().is_server_error() => {
            tracing::warn!(status = res.status().as_u16(), "ops: hermit failed");
            page(app, Page::OpsUnavailable, None)
        }
        Ok(res) => {
            let status = res.status();
            let mut headers = HeaderMap::new();
            for name in &PASSED {
                if let Some(v) = res.headers().get(name) {
                    headers.insert(name.clone(), v.clone());
                }
            }
            let mut r = Response::new(Body::from_stream(res.bytes_stream()));
            *r.status_mut() = status;
            *r.headers_mut() = headers;
            r
        }
        Err(e) => {
            tracing::warn!(error = %e, "ops: hermit did not answer");
            page(app, Page::OpsUnavailable, None)
        }
    }
}
