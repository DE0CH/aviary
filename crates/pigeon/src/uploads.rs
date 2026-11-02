//! `POST /api/assets/files` (spec 5.7): one file per request, its bytes streamed straight to magpie's M13 with at
//! most one chunk in flight (backpressure to the client), never buffered whole.

use axum::body::Bytes;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use magpie_api::uploads::UploadKind;
use serde_json::json;
use tokio::sync::mpsc;

use crate::App;
use crate::http::{json, person_calls};
use crate::identity::Identity;

/// The file part's cap: 50 MiB, the app's `FILE_UPLOAD_MAX_BYTES` and magpie's (spec 5.7).
pub const MAX_FILE: u64 = 52_428_800;
/// The whole multipart body's cap: 50 MiB plus 1 MiB of multipart overhead.
pub const MAX_BODY: u64 = 53_477_376;
/// The longest stored file name, bytes.
pub const MAX_NAME: usize = 255;

/// The content type of an upload from its file name, when the part names none (or `application/octet-stream`).
#[must_use]
pub fn type_from_name(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "json" => "application/json",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// The file name as stored: no `/`, `\`, NUL or control characters (each becomes `_`), at most 255 bytes.
#[must_use]
pub fn sanitise_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        let c = if c == '/' || c == '\\' || c.is_control() {
            '_'
        } else {
            c
        };
        if out.len() + c.len_utf8() > MAX_NAME {
            break;
        }
        out.push(c);
    }
    if out.is_empty() { "file".into() } else { out }
}

fn error(app: &App, status: StatusCode, name: &str) -> Response {
    let zh = app.0.identity.is_zh();
    let text = match (status, zh) {
        (StatusCode::PAYLOAD_TOO_LARGE, true) => format!("「{name}」太大（上限 50MB）。"),
        (StatusCode::PAYLOAD_TOO_LARGE, false) => format!("\"{name}\" is too large (max 50MB)."),
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, true) => "不支援這種檔案。".into(),
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, false) => "This kind of file is not supported.".into(),
        (StatusCode::BAD_REQUEST, true) => "每次只可上載一個檔案。".into(),
        (StatusCode::BAD_REQUEST, false) => "Send exactly one file.".into(),
        (_, true) => "上載失敗，請再試一次。".into(),
        (_, false) => "Upload failed. Please try again.".into(),
    };
    json(status, &json!({"error": text}))
}

/// Handles the upload.
pub async fn upload(app: &App, req: Request, id: &Identity) -> Response {
    let headers = req.headers().clone();
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_BODY) {
        return error(app, StatusCode::PAYLOAD_TOO_LARGE, "");
    }
    let Some(boundary) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| multer::parse_boundary(ct).ok())
    else {
        return error(app, StatusCode::BAD_REQUEST, "");
    };
    let Some(calls) = person_calls(&app.0.magpie_transfer, id) else {
        return error(app, StatusCode::BAD_GATEWAY, "");
    };
    let _guard = app.0.transfers.guard();
    let idle = app.0.tuning.transfer_idle;
    let body = req.into_body().into_data_stream();
    let mut parts = multer::Multipart::with_constraints(
        body,
        boundary,
        multer::Constraints::new().size_limit(multer::SizeLimit::new().whole_stream(MAX_BODY)),
    );
    let mut field = loop {
        match tokio::time::timeout(idle, parts.next_field()).await {
            Ok(Ok(Some(f))) if f.name() == Some("files") && f.file_name().is_some() => break f,
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None) | Err(_)) | Err(_) => return error(app, StatusCode::BAD_REQUEST, ""),
        }
    };
    let name = sanitise_name(field.file_name().unwrap_or("file"));
    let content_type = match field.content_type().map(ToString::to_string) {
        Some(t) if !t.is_empty() && t != "application/octet-stream" => t,
        _ => type_from_name(&name).to_owned(),
    };
    // one chunk in flight: the client is read only as fast as magpie takes the bytes
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(1);
    let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|c| (c, rx)) });
    let upstream = calls_upload(calls, &name, &content_type, reqwest::Body::wrap_stream(stream));
    let feed = async {
        let mut total: u64 = 0;
        let outcome = loop {
            match tokio::time::timeout(idle, field.chunk()).await {
                Ok(Ok(Some(chunk))) => {
                    total += chunk.len() as u64;
                    if total > MAX_FILE {
                        break Some(StatusCode::PAYLOAD_TOO_LARGE);
                    }
                    if tx.send(Ok(chunk)).await.is_err() {
                        // magpie stopped reading: its answer says why
                        break None;
                    }
                }
                Ok(Ok(None)) => break None,
                Ok(Err(multer::Error::StreamSizeExceeded { .. })) => break Some(StatusCode::PAYLOAD_TOO_LARGE),
                Ok(Err(_)) | Err(_) => break Some(StatusCode::BAD_REQUEST),
            }
        };
        if outcome.is_some() {
            // a broken upstream body: magpie stores nothing
            let _ = tx.send(Err(std::io::Error::other("upload aborted"))).await;
        }
        drop(tx);
        (total, outcome)
    };
    let (sent, (total, outcome)) = tokio::join!(upstream, feed);
    if let Some(status) = outcome {
        return error(app, status, &name);
    }
    // exactly one file part: a second one is refused (multer hands out the next part once this one is dropped)
    drop(field);
    if let Ok(Ok(Some(f))) = tokio::time::timeout(idle, parts.next_field()).await
        && f.name() == Some("files")
    {
        return error(app, StatusCode::BAD_REQUEST, &name);
    }
    app.0.metrics.upload_bytes(total);
    match sent {
        Ok(u) => {
            let mut r = json(
                StatusCode::CREATED,
                &json!({"attachments": [{"path": u.path, "name": u.name, "size": u.size, "mimeType": u.mime_type}]}),
            );
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(status) => error(app, status, &name),
    }
}

async fn calls_upload(
    calls: magpie_api::client::PersonCalls<'_>,
    name: &str,
    content_type: &str,
    body: reqwest::Body,
) -> Result<magpie_api::uploads::UploadResponse, StatusCode> {
    match calls.upload(name, UploadKind::File, content_type, body).await {
        Ok(u) => Ok(u),
        Err(e) => {
            let status = e.api().map(|r| r.status);
            tracing::warn!(error = %e, "upload: magpie refused or failed");
            Err(match status {
                Some(413) => StatusCode::PAYLOAD_TOO_LARGE,
                Some(415) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
                _ => StatusCode::BAD_GATEWAY,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(sanitise_name("a/b\\c\u{0}d.txt"), "a_b_c_d.txt");
        assert_eq!(sanitise_name("  "), "file");
        assert!(sanitise_name(&"報".repeat(200)).len() <= MAX_NAME);
        assert_eq!(
            type_from_name("Report.XLSX"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(type_from_name("x"), "application/octet-stream");
    }
}
