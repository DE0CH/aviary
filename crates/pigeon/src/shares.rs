//! Shares (spec 5.10): the share sheet and the read-only share page, forwarded to magpie's M16 with the person
//! headers; the shapes to the app are today's.

use axum::body::{Body, Bytes};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use magpie_api::client::ClientError;
use magpie_api::errors::ErrorCode;
use serde_json::{Value, json};

use crate::App;
use crate::blobs::{FIRST_BYTE, raw_query, relay};
use crate::http::{api_error, json, person_calls};
use crate::identity::Identity;
use crate::paseo::ProjectCtx;

/// Whether `id` can be a share id (base64url, at most 64 characters).
#[must_use]
pub fn valid_share_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn not_found() -> Response {
    api_error(StatusCode::NOT_FOUND, "SHARE_NOT_FOUND", "Share not found.")
}

fn failed(e: &ClientError) -> Response {
    match e.code() {
        Some(ErrorCode::NotFound) => not_found(),
        Some(ErrorCode::NothingToShare) => api_error(
            StatusCode::CONFLICT,
            "NOTHING_TO_SHARE",
            "There is nothing to share yet.",
        ),
        Some(ErrorCode::ViewAsReadOnly) => crate::http::view_as_read_only(),
        _ => {
            tracing::warn!(error = %e, "shares: magpie failed");
            api_error(
                StatusCode::BAD_GATEWAY,
                "SHARE_FAILED",
                "The share could not be completed.",
            )
        }
    }
}

fn link(id: &str, created_at: magpie_api::Timestamp) -> Value {
    json!({"id": id, "url": format!("/s/{id}"), "createdAt": created_at})
}

/// `GET|POST …/sessions/{id}/shares`.
pub async fn conversation_shares(app: &App, req: &Parts, id: &Identity, conversation: &str) -> Response {
    let Ok(cid) = uuid::Uuid::parse_str(conversation) else {
        return not_found();
    };
    let Some(calls) = person_calls(&app.0.magpie, id) else {
        return not_found();
    };
    if req.method == Method::POST {
        return match calls.create_share(cid).await {
            Ok(s) => json(StatusCode::CREATED, &link(&s.id, s.created_at)),
            Err(e) => failed(&e),
        };
    }
    match calls.list_shares(cid).await {
        Ok(list) => {
            let shares: Vec<Value> = list.shares.iter().map(|s| link(&s.id, s.created_at)).collect();
            json(StatusCode::OK, &json!({"shares": shares}))
        }
        Err(e) => failed(&e),
    }
}

/// `GET|DELETE …/shares/{shareId}`.
pub async fn share(app: &App, req: &Parts, id: &Identity, share_id: &str) -> Response {
    if !valid_share_id(share_id) {
        return not_found();
    }
    let Some(calls) = person_calls(&app.0.magpie, id) else {
        return not_found();
    };
    if req.method == Method::DELETE {
        return match calls.revoke_share(share_id).await {
            Ok(()) => {
                let mut r = Response::new(Body::empty());
                *r.status_mut() = StatusCode::NO_CONTENT;
                r.headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                r
            }
            Err(e) => failed(&e),
        };
    }
    let view = match calls.get_share(share_id).await {
        Ok(v) => v,
        Err(e) => return failed(&e),
    };
    // the head, then each projected row as it is produced, so the projection is never held whole
    let head = json!({
        "id": view.id,
        "title": view.title,
        "ownerName": view.owner_name,
        "isOwner": view.is_owner,
        "createdAt": view.created_at,
    });
    let mut head = serde_json::to_vec(&head).unwrap_or_default();
    head.pop(); // the closing brace: the timeline goes in before it
    head.extend_from_slice(br#","timeline":["#);
    let projector = app.0.projector.clone();
    let signer = app.blob_signer();
    let zh = app.0.identity.is_zh();
    let rows = view.rows.into_iter();
    let state = (Some(head), rows, true, false);
    let stream = futures_util::stream::unfold(state, move |(head, mut rows, first, done)| {
        let projector = projector.clone();
        let signer = signer.clone();
        async move {
            if let Some(h) = head {
                return Some((Ok::<_, std::io::Error>(Bytes::from(h)), (None, rows, first, done)));
            }
            if done {
                return None;
            }
            let ctx = ProjectCtx { zh, signer: &signer };
            for row in rows.by_ref() {
                if let Some(v) = projector.project(&row, &ctx) {
                    let mut out = if first { Vec::new() } else { vec![b','] };
                    out.extend_from_slice(&serde_json::to_vec(&v).unwrap_or_default());
                    return Some((Ok(Bytes::from(out)), (None, rows, false, false)));
                }
            }
            Some((Ok(Bytes::from_static(b"]}")), (None, rows, first, true)))
        }
    });
    let mut r = Response::new(Body::from_stream(stream));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// `GET|HEAD …/shares/{shareId}/files/raw` (bytes as 5.8).
pub async fn share_files(app: &App, req: &Parts, id: &Identity, share_id: &str, query: Option<&str>) -> Response {
    if !valid_share_id(share_id) {
        return not_found();
    }
    let q = match raw_query(query) {
        Ok(q) => q,
        Err(r) => return *r,
    };
    let Some(calls) = person_calls(&app.0.magpie_transfer, id) else {
        return not_found();
    };
    let mut forward = reqwest::header::HeaderMap::new();
    for name in &magpie_api::client::RELAY_REQUEST_HEADERS {
        for v in req.headers.get_all(name) {
            forward.append(name.clone(), v.clone());
        }
    }
    let guard = app.0.transfers.guard();
    match tokio::time::timeout(FIRST_BYTE, calls.share_files_raw_relay(share_id, &q, &forward)).await {
        Ok(Ok(res)) => relay(res, guard, None),
        Ok(Err(_)) | Err(_) => api_error(
            StatusCode::BAD_GATEWAY,
            "SHARE_FAILED",
            "The file could not be fetched.",
        ),
    }
}

/// `POST …/shares/{shareId}/continue`: a new conversation from the share, owned by the caller.
pub async fn continue_share(app: &App, _req: &Parts, id: &Identity, share_id: &str) -> Response {
    if !valid_share_id(share_id) {
        return not_found();
    }
    let Some(calls) = person_calls(&app.0.magpie, id) else {
        return not_found();
    };
    match calls.continue_share(share_id).await {
        Ok(c) => json(StatusCode::CREATED, &json!({"sessionId": c.conversation_id})),
        Err(e) => failed(&e),
    }
}
