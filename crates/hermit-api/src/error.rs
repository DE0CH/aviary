//! The error envelope (platform 3.6, hermit 3.1) and every code hermit's contract names.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Error codes. Values, not keys: they are `lower_snake_case` by the platform rule.
pub mod codes {
    // platform 3.6
    pub const BAD_REQUEST: &str = "bad_request";
    pub const UNAUTHENTICATED: &str = "unauthenticated";
    pub const WRONG_CALLER: &str = "wrong_caller";
    pub const NOT_FOUND: &str = "not_found";
    pub const CURSOR_EXPIRED: &str = "cursor_expired";
    pub const TOO_LARGE: &str = "too_large";
    pub const UNSUPPORTED_TYPE: &str = "unsupported_type";
    pub const UNAVAILABLE: &str = "unavailable";
    /// An implementation that lacks a call (hermit 3.1): a normal answer.
    pub const NOT_SUPPORTED: &str = "not_supported";
    pub const METHOD_NOT_ALLOWED: &str = "method_not_allowed";
    // the codes magpie maps to turn codes and pigeon renders (hermit 3.1)
    pub const NOT_ALLOWED: &str = "not_allowed";
    pub const SANDBOX_BUSY: &str = "sandbox_busy";
    pub const SANDBOX_FAILED: &str = "sandbox_failed";
    pub const SANDBOX_LOST: &str = "sandbox_lost";
    pub const SANDBOX_UNREACHABLE: &str = "sandbox_unreachable";
    // people (3.4)
    pub const BAD_EMAIL: &str = "bad_email";
    // sandboxes (3.6)
    pub const PERSON_MISMATCH: &str = "person_mismatch";
    pub const NO_SANDBOX: &str = "no_sandbox";
    // workspaces (3.8)
    pub const WORKSPACE_UNAVAILABLE: &str = "workspace_unavailable";
    pub const NO_WORKSPACE: &str = "no_workspace";
    pub const NO_PATH: &str = "no_path";
    pub const BAD_PATH: &str = "bad_path";
    pub const NOT_A_DIR: &str = "not_a_dir";
    pub const NOT_A_FILE: &str = "not_a_file";
    pub const EXISTS: &str = "exists";
    /// 416 on `GET /v1/workspaces/{wid}/files` (the spec names only the status).
    pub const BAD_RANGE: &str = "bad_range";
    // secrets (3.7.2)
    pub const NOT_GRANTED: &str = "not_granted";
    // token liveness (3.9)
    pub const UNKNOWN_SANDBOX: &str = "unknown_sandbox";
    // ops and admin (3.11, 16)
    pub const NOT_ADMIN: &str = "not_admin";
    pub const STILL_ALLOWED: &str = "still_allowed";
}

/// The flat error body: `{"code", "message", …type-specific fields}`. Never `{"error": {…}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    /// English, for logs; never a staff-facing sentence.
    #[serde(default)]
    pub message: String,
    /// Type-specific fields (e.g. `reason`, `retryAfterS` on 503 `sandbox_busy`).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A non-2xx answer: its status, its body and an optional `retry-after` in seconds.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{status} {}: {}", body.code, body.message)]
pub struct ApiError {
    pub status: u16,
    pub body: ErrorBody,
    pub retry_after: Option<u32>,
}

impl ApiError {
    #[must_use]
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code: code.to_owned(),
                message: message.into(),
                extra: Map::new(),
            },
            retry_after: None,
        }
    }

    /// Adds a type-specific field (a camelCase key).
    #[must_use]
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.body.extra.insert(key.to_owned(), value.into());
        self
    }

    /// Sets `retry-after` (seconds).
    #[must_use]
    pub fn retry_after(mut self, seconds: u32) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    #[must_use]
    pub fn code(&self) -> &str {
        &self.body.code
    }

    #[must_use]
    pub fn is(&self, code: &str) -> bool {
        self.body.code == code
    }

    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, codes::BAD_REQUEST, message)
    }

    #[must_use]
    pub fn unauthenticated() -> Self {
        Self::new(401, codes::UNAUTHENTICATED, "missing or unknown bearer token")
    }

    #[must_use]
    pub fn wrong_caller() -> Self {
        Self::new(403, codes::WRONG_CALLER, "this caller may not use this route")
    }

    #[must_use]
    pub fn not_found(code: &str, message: impl Into<String>) -> Self {
        Self::new(404, code, message)
    }

    #[must_use]
    pub fn too_large() -> Self {
        Self::new(413, codes::TOO_LARGE, "request body too large")
    }

    /// 501 `not_supported`: the implementation lacks this call.
    #[must_use]
    pub fn not_supported() -> Self {
        Self::new(501, codes::NOT_SUPPORTED, "not supported by this implementation")
    }

    #[must_use]
    pub fn not_allowed() -> Self {
        Self::new(403, codes::NOT_ALLOWED, "person is not allowed")
    }

    /// 503 `sandbox_busy` while the operator has sandboxes disabled (hermit 3.6.2):
    /// `{reason: "disabled", retryAfterS: 300}` with `retry-after: 300`.
    #[must_use]
    pub fn sandboxes_disabled() -> Self {
        Self::new(503, codes::SANDBOX_BUSY, "sandboxes are disabled by the operator")
            .with("reason", "disabled")
            .with("retryAfterS", 300)
            .retry_after(300)
    }

    #[must_use]
    pub fn unavailable(retry_after: u32) -> Self {
        Self::new(503, codes::UNAVAILABLE, "unavailable").retry_after(retry_after)
    }
}
