//! The T0→gate upload tee (DESIGN 9.5; switch-final C14.3 limited to that span, and a rollback's): while an old-path
//! socket may still send what the composer holds, an upload is written into the person's old child through the old
//! upload route AND into magpie, the old asset path is recorded on the magpie upload (`uploads.legacy_path`), and the
//! answer carries the old path: the old child accepts it on the old socket and through LU, and magpie resolves it for
//! a native conversation. When the old child refuses or fails, the answer is the old route's (the failure the old path
//! would have shown, `pigeon_upload_tee_failed_total`), never a magpie-only path. One file per request, at most the
//! old route's 30 MB, read whole (it goes to both).

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use magpie_api::legacy::LegacyPathRequest;
use serde_json::json;

use super::OLD_FILE_MAX;
use super::upstream::CallError;
use crate::App;
use crate::http::{api_error, json, person_calls};
use crate::identity::Identity;
use crate::uploads::{MAX_NAME, calls_upload, close, error, rest, sanitise_name, too_large_old};

/// The old route's answer time for one file.
pub(crate) const OLD_UPLOAD_BOUND: Duration = Duration::from_secs(120);

/// The tee.
#[allow(clippy::too_many_lines)] // one request's steps in order: read, old child, magpie, the record
pub async fn tee(app: &App, req: Request, id: &Identity) -> Response {
    let headers = req.headers().clone();
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > OLD_FILE_MAX + (1 << 20)) {
        return too_large_old(app, "");
    }
    let Some(boundary) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| multer::parse_boundary(ct).ok())
    else {
        return error(app, StatusCode::BAD_REQUEST, "");
    };
    let (Some(calls), Some(who)) = (person_calls(&app.0.magpie_upload, id), app.legacy().who(id)) else {
        return error(app, StatusCode::BAD_GATEWAY, "");
    };
    let _guard = app.0.transfers.guard();
    let idle = app.0.tuning.transfer_idle;
    let mut parts = multer::Multipart::with_constraints(
        req.into_body().into_data_stream(),
        boundary,
        multer::Constraints::new().size_limit(multer::SizeLimit::new().whole_stream(OLD_FILE_MAX + (1 << 20))),
    );
    let mut field = loop {
        match tokio::time::timeout(idle, parts.next_field()).await {
            Ok(Ok(Some(f))) if f.name() == Some("files") && f.file_name().is_some() => break f,
            Ok(Ok(Some(_))) => {}
            Ok(Err(multer::Error::StreamSizeExceeded { .. })) => return close(too_large_old(app, "")),
            Ok(Ok(None) | Err(_)) | Err(_) => return error(app, StatusCode::BAD_REQUEST, ""),
        }
    };
    let name = sanitise_name(field.file_name().unwrap_or("file"));
    debug_assert!(name.len() <= MAX_NAME);
    let content_type = match field.content_type().map(ToString::to_string) {
        Some(t) if !t.is_empty() && t != "application/octet-stream" => t,
        _ => magpie_api::mime::from_name(&name).to_owned(),
    };
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match tokio::time::timeout(idle, field.chunk()).await {
            Ok(Ok(Some(c))) => {
                if (buf.len() + c.len()) as u64 > OLD_FILE_MAX {
                    return close(too_large_old(app, &name));
                }
                buf.extend_from_slice(&c);
            }
            Ok(Ok(None)) => break,
            Ok(Err(multer::Error::StreamSizeExceeded { .. })) => return close(too_large_old(app, &name)),
            Ok(Err(_)) | Err(_) => return close(error(app, StatusCode::BAD_REQUEST, &name)),
        }
    }
    drop(field);
    if let Some(status) = rest(&mut parts, idle).await {
        return close(error(app, status, &name));
    }
    let bytes = Bytes::from(buf);
    app.0.metrics.upload_bytes(bytes.len() as u64);
    let legacy = app.legacy();
    // the old child first: its refusal is the answer, and nothing is kept in magpie for a file it refused
    let old = match legacy
        .old_upload(&who, &name, &content_type, bytes.clone(), OLD_UPLOAD_BOUND)
        .await
    {
        Ok(rec) => rec,
        Err(e) => {
            legacy.0.metrics.tee_failed.inc();
            tracing::warn!(error = ?e, "upload tee: the old child did not take the file");
            return match e {
                CallError::SignedOut => api_error(StatusCode::UNAUTHORIZED, "NOT_SIGNED_IN", "Not signed in."),
                CallError::Status(s, body) => old_answer(s, body),
                CallError::Unreachable | CallError::AppliedUnread(_) => error(app, StatusCode::BAD_GATEWAY, &name),
            };
        }
    };
    let Some(old_path) = old.get("path").and_then(serde_json::Value::as_str).map(str::to_owned) else {
        legacy.0.metrics.tee_failed.inc();
        return error(app, StatusCode::BAD_GATEWAY, &name);
    };
    let u = match calls_upload(calls.clone(), &name, &content_type, reqwest::Body::from(bytes)).await {
        Ok(u) => u,
        Err(status) => {
            legacy.0.metrics.tee_failed.inc();
            return error(app, status, &name);
        }
    };
    if let Err(e) = calls
        .upload_legacy_path(
            u.upload_id,
            &LegacyPathRequest {
                legacy_path: old_path.clone(),
            },
        )
        .await
    {
        legacy.0.metrics.tee_failed.inc();
        tracing::warn!(error = %e, "upload tee: the old path not recorded");
        return error(app, StatusCode::BAD_GATEWAY, &name);
    }
    let mut r = json(
        StatusCode::CREATED,
        &json!({"attachments": [{"path": old_path, "name": u.name, "size": u.size, "mimeType": u.mime_type}]}),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// The old route's own refusal, passed on.
fn old_answer(status: u16, body: Bytes) -> Response {
    let mut r = Response::new(axum::body::Body::from(body));
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    r
}
