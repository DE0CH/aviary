//! Bytes streamed through from magpie: workspace and sent files (spec 5.8) and timeline images by signed URL (5.9).
//! No body is buffered; the client's range and conditional headers go up and magpie's answer comes back as is.

use std::time::Duration;

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt as _;
use magpie_api::files::RawFileQuery;
use magpie_api::uploads::{BlobQuery, BlobVariant};
use ring::hmac;

use crate::http::{api_error, person_calls, query_param};
use crate::identity::Identity;
use crate::server::TransferGuard;
use crate::{App, KeyList};

/// The longest wait for magpie's answer headers on a transfer (spec 13).
pub const FIRST_BYTE: Duration = Duration::from_secs(30);
/// Characters of the URL signature: 22 base64url characters, 128 bits (spec 5.9).
pub const SIG_LEN: usize = 22;
/// The longest `path` (spec 13).
pub const MAX_PATH: usize = 4096;

/// magpie's answer headers passed to the app (spec 5.8).
pub const PASSED: [HeaderName; 11] = [
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::CONTENT_DISPOSITION,
    header::LAST_MODIFIED,
    header::ETAG,
    header::ACCEPT_RANGES,
    header::CONTENT_RANGE,
    header::CONTENT_SECURITY_POLICY,
    header::X_CONTENT_TYPE_OPTIONS,
    header::CACHE_CONTROL,
    header::VARY,
];

/// A variant name of 5.9.
#[must_use]
pub fn variant_name(v: BlobVariant) -> &'static str {
    match v {
        BlobVariant::Thumb => "thumb",
        BlobVariant::Display => "display",
        BlobVariant::Full => "full",
    }
}

fn parse_variant(s: &str) -> Option<BlobVariant> {
    match s {
        "thumb" => Some(BlobVariant::Thumb),
        "display" => Some(BlobVariant::Display),
        "full" => Some(BlobVariant::Full),
        _ => None,
    }
}

/// Signs and checks image URLs (spec 5.9). Cheap to clone.
#[derive(Debug, Clone)]
pub struct Signer(KeyList);

impl Signer {
    /// A signer over `keys` (the first signs, all verify).
    #[must_use]
    pub fn new(keys: &KeyList) -> Self {
        Self(keys.clone())
    }

    fn sig_with(key: &[u8; 32], blob_id: &str, variant: &str) -> String {
        let tag = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, key),
            format!("{blob_id}|{variant}").as_bytes(),
        );
        let mut s = URL_SAFE_NO_PAD.encode(tag.as_ref());
        s.truncate(SIG_LEN);
        s
    }

    /// The signature of (`blob_id`, `variant`).
    #[must_use]
    pub fn sig(&self, blob_id: &str, variant: BlobVariant) -> String {
        self.0
            .0
            .first()
            .map(|k| Self::sig_with(k, blob_id, variant_name(variant)))
            .unwrap_or_default()
    }

    /// The app's URL of an image: `/api/blobs/<id>?v=<variant>&sig=<sig>`.
    #[must_use]
    pub fn url(&self, blob_id: &str, variant: BlobVariant) -> String {
        format!(
            "/api/blobs/{}?v={}&sig={}",
            magpie_api::routes::encode_segment(blob_id),
            variant_name(variant),
            self.sig(blob_id, variant)
        )
    }

    /// Whether `sig` is the signature of (`blob_id`, `variant`) under any key, compared in constant time.
    #[must_use]
    pub fn check(&self, blob_id: &str, variant: &str, sig: &str) -> bool {
        use subtle::ConstantTimeEq as _;
        let mut ok = subtle::Choice::from(0);
        for k in &self.0.0 {
            let want = Self::sig_with(k, blob_id, variant);
            ok |= want.as_bytes().ct_eq(sig.as_bytes());
        }
        bool::from(ok)
    }
}

/// The request headers relayed to magpie (range and conditionals).
fn forwarded(headers: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for name in &magpie_api::client::RELAY_REQUEST_HEADERS {
        for v in headers.get_all(name) {
            out.append(name.clone(), v.clone());
        }
    }
    out
}

/// Relays magpie's answer: status, the headers of [`PASSED`] and the body as a stream that holds a transfer guard
/// (the drain waits for it, spec 11.2 step 4). `cache` replaces `cache-control` on a 2xx.
pub(crate) fn relay(res: reqwest::Response, guard: TransferGuard, cache: Option<&'static str>) -> Response {
    let status = res.status();
    let mut h = HeaderMap::new();
    for name in &PASSED {
        for v in res.headers().get_all(name) {
            h.append(name.clone(), v.clone());
        }
    }
    if let Some(c) = cache
        && status.is_success()
    {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(c));
    }
    let stream = res.bytes_stream().map(move |chunk| {
        let _held = &guard;
        chunk
    });
    let mut r = Response::new(Body::from_stream(stream));
    *r.status_mut() = status;
    *r.headers_mut() = h;
    r
}

fn bad(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
}

fn unreachable_upstream() -> Response {
    api_error(
        StatusCode::BAD_GATEWAY,
        "UPSTREAM_UNAVAILABLE",
        "The file could not be fetched.",
    )
}

/// Whether `sent` is `^[A-Za-z0-9_-]{1,128}$`.
#[must_use]
pub fn valid_sent(sent: &str) -> bool {
    (1..=128).contains(&sent.len())
        && sent
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The query of a files route (spec 5.8): `path` (relative, ≤ 4096 characters, no NUL), `sent?`, `inline` `1` or
/// absent.
pub(crate) fn raw_query(query: Option<&str>) -> Result<RawFileQuery, Box<Response>> {
    let path = query_param(query, "path").ok_or_else(|| Box::new(bad("path is required")))?;
    if path.is_empty() || path.chars().count() > MAX_PATH || path.contains('\0') || path.starts_with('/') {
        return Err(Box::new(bad("bad path")));
    }
    let sent = query_param(query, "sent");
    if let Some(s) = &sent
        && !valid_sent(s)
    {
        return Err(Box::new(bad("bad sent")));
    }
    let inline = match query_param(query, "inline").as_deref() {
        None => false,
        Some("1") => true,
        Some(_) => return Err(Box::new(bad("bad inline"))),
    };
    Ok(RawFileQuery { path, sent, inline })
}

/// `GET|HEAD …/sessions/{id}/files/raw?path&inline&sent` (spec 5.8).
pub async fn files_raw(app: &App, req: &Parts, id: &Identity, conversation: &str, query: Option<&str>) -> Response {
    let Ok(cid) = uuid::Uuid::parse_str(conversation) else {
        return bad("bad conversation id");
    };
    let q = match raw_query(query) {
        Ok(q) => q,
        Err(r) => return *r,
    };
    let Some(calls) = person_calls(&app.0.magpie_transfer, id) else {
        return unreachable_upstream();
    };
    // the switch window: a legacy conversation's files are the old child's
    if let Some(r) = app.legacy().files_raw(app, req, id, cid).await {
        return r;
    }
    let guard = app.0.transfers.guard();
    match tokio::time::timeout(FIRST_BYTE, calls.files_raw_relay(cid, &q, &forwarded(&req.headers))).await {
        Ok(Ok(res)) => relay(res, guard, None),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "files: magpie did not answer");
            unreachable_upstream()
        }
        Err(_) => {
            tracing::warn!("files: magpie timed out");
            unreachable_upstream()
        }
    }
}

/// `GET /api/blobs/{blobId}?v=&sig=` (spec 5.9): the signature is the authorisation to that one blob.
pub async fn blob(app: &App, req: &Parts, blob_id: &str, query: Option<&str>) -> Response {
    let not_found = || api_error(StatusCode::NOT_FOUND, "NOT_FOUND", "Not found.");
    let v = query_param(query, "v").unwrap_or_default();
    let sig = query_param(query, "sig").unwrap_or_default();
    let Some(variant) = parse_variant(&v) else {
        return not_found();
    };
    if !Signer::new(&app.0.secrets.blob_keys).check(blob_id, &v, &sig) {
        return not_found();
    }
    let guard = app.0.transfers.guard();
    let q = BlobQuery { variant: Some(variant) };
    match tokio::time::timeout(
        FIRST_BYTE,
        app.0.magpie_transfer.blob_relay(blob_id, &q, &forwarded(&req.headers)),
    )
    .await
    {
        Ok(Ok(res)) => relay(res, guard, Some("private, max-age=86400, immutable")),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "blob: magpie did not answer");
            unreachable_upstream()
        }
        Err(_) => unreachable_upstream(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        let s = Signer::new(&KeyList(vec![[7; 32]]));
        let sig = s.sig("b1", BlobVariant::Thumb);
        assert_eq!(sig.len(), SIG_LEN);
        assert!(s.check("b1", "thumb", &sig));
        assert!(!s.check("b1", "full", &sig), "another variant");
        assert!(!s.check("b2", "thumb", &sig), "another blob");
        assert!(!s.check("b1", "thumb", &sig[..21]), "short");
        let rotated = Signer::new(&KeyList(vec![[8; 32], [7; 32]]));
        assert!(rotated.check("b1", "thumb", &sig), "the previous key verifies");
        assert_eq!(
            s.url("b1", BlobVariant::Display),
            format!("/api/blobs/b1?v=display&sig={}", s.sig("b1", BlobVariant::Display))
        );
    }

    #[test]
    fn queries() {
        assert!(raw_query(Some("path=out%2Fa.txt&sent=toolu_1&inline=1")).is_ok());
        assert!(raw_query(Some("path=/etc/passwd")).is_err());
        assert!(raw_query(Some("path=a&inline=yes")).is_err());
        assert!(raw_query(Some("path=a&sent=a.b")).is_err());
        assert!(raw_query(Some("path=a%00b")).is_err());
        assert!(raw_query(None).is_err());
    }
}
