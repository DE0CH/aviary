//! The error envelope (platform 3.6): every non-2xx JSON answer is the flat object
//! `{"code": "<lower_snake_case>", "message": "<English, for logs>", …type-specific fields}`.

use std::fmt;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// The body of an error answer, as sent and as parsed by clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// `lower_snake_case`: `bad_request`, `wrong_caller`, `conversation_migrating`, ….
    pub code: String,
    /// English, for logs; never shown to a person as is.
    pub message: String,
    /// The type-specific fields (camelCase names), flattened beside `code` and `message`.
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// An error answer: status, envelope and the headers its status needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
    retry_after: Option<u64>,
}

impl ApiError {
    /// An answer with `status`, `code` (`lower_snake_case`) and `message`.
    #[must_use]
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        debug_assert!(
            code.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "error codes are lower_snake_case: {code}"
        );
        Self {
            status,
            body: ErrorBody {
                code,
                message: message.into(),
                fields: serde_json::Map::new(),
            },
            retry_after: None,
        }
    }

    /// 400 `bad_request`.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// 401 `unauthenticated` (sent with `www-authenticate: Bearer`).
    #[must_use]
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "missing or unknown caller token",
        )
    }

    /// 403 `wrong_caller`: a valid token on a route its caller may not use; names the caller.
    #[must_use]
    pub fn wrong_caller(caller: crate::auth::Caller) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "wrong_caller",
            "this route does not accept this caller",
        )
        .with("caller", caller.as_str())
    }

    /// 404 `not_found`.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// 409 with a domain code.
    #[must_use]
    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    /// 410 `cursor_expired`.
    #[must_use]
    pub fn cursor_expired(message: impl Into<String>) -> Self {
        Self::new(StatusCode::GONE, "cursor_expired", message)
    }

    /// 413 `too_large`.
    #[must_use]
    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", message)
    }

    /// 415 `unsupported_type`.
    #[must_use]
    pub fn unsupported_type(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_type", message)
    }

    /// 429 `rate_limited` with `retry-after` seconds.
    #[must_use]
    pub fn rate_limited(retry_after_secs: u64) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many requests").retry_after(retry_after_secs)
    }

    /// 503 `unavailable` with `retry-after` seconds.
    #[must_use]
    pub fn unavailable(message: impl Into<String>, retry_after_secs: u64) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message).retry_after(retry_after_secs)
    }

    /// 507 `quota_exceeded`.
    #[must_use]
    pub fn quota_exceeded(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INSUFFICIENT_STORAGE, "quota_exceeded", message)
    }

    /// 500 `internal`.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    /// Adds a type-specific field (camelCase name).
    #[must_use]
    pub fn with(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        debug_assert!(!key.contains('_'), "envelope fields are camelCase: {key}");
        debug_assert!(key != "code" && key != "message", "{key} is the envelope's own field");
        self.body.fields.insert(key.to_owned(), value.into());
        self
    }

    /// Sets `retry-after` (seconds).
    #[must_use]
    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    /// The status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.body.code
    }

    /// The envelope.
    #[must_use]
    pub fn body(&self) -> &ErrorBody {
        &self.body
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.status.as_u16(), self.body.code, self.body.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut r = super::json(self.status, &self.body);
        if self.status == StatusCode::UNAUTHORIZED {
            r.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(secs) = self.retry_after {
            r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn envelope_is_flat() {
        let e = ApiError::conflict("conversation_migrating", "conversation is owned by the old stack")
            .with("route", "legacy");
        assert_eq!(
            serde_json::to_value(e.body()).unwrap(),
            json!({"code": "conversation_migrating", "message": "conversation is owned by the old stack", "route": "legacy"})
        );
        assert_eq!(e.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn envelope_round_trips() {
        let text = r#"{"code":"rate_limited","message":"slow down","retryAfterMs":1500}"#;
        let b: ErrorBody = serde_json::from_str(text).unwrap();
        assert_eq!(b.code, "rate_limited");
        assert_eq!(b.fields["retryAfterMs"], 1500);
        assert_eq!(serde_json::to_string(&b).unwrap(), text);
    }

    #[test]
    fn constructors_have_the_platform_codes() {
        let cases = [
            (ApiError::bad_request("x"), 400, "bad_request"),
            (ApiError::unauthenticated(), 401, "unauthenticated"),
            (
                ApiError::wrong_caller(crate::auth::Caller::Gateway),
                403,
                "wrong_caller",
            ),
            (ApiError::not_found("x"), 404, "not_found"),
            (ApiError::cursor_expired("x"), 410, "cursor_expired"),
            (ApiError::too_large("x"), 413, "too_large"),
            (ApiError::unsupported_type("x"), 415, "unsupported_type"),
            (ApiError::rate_limited(2), 429, "rate_limited"),
            (ApiError::unavailable("x", 1), 503, "unavailable"),
            (ApiError::quota_exceeded("x"), 507, "quota_exceeded"),
            (ApiError::internal("x"), 500, "internal"),
        ];
        for (e, status, code) in cases {
            assert_eq!(e.status().as_u16(), status);
            assert_eq!(e.code(), code);
        }
    }

    #[tokio::test]
    async fn response_has_headers_and_body() {
        let r = ApiError::unauthenticated().into_response();
        assert_eq!(r.status(), 401);
        assert_eq!(r.headers()["www-authenticate"], "Bearer");
        assert_eq!(r.headers()["content-type"], "application/json; charset=utf-8");
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v,
            json!({"code": "unauthenticated", "message": "missing or unknown caller token"})
        );

        let r = ApiError::unavailable("busy", 3).into_response();
        assert_eq!(r.headers()["retry-after"], "3");
        let r = ApiError::wrong_caller(crate::auth::Caller::Tools).into_response();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["caller"], "tools");
        assert!(v.get("error").is_none());
    }
}
