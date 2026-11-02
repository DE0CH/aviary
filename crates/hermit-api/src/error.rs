//! Every error code hermit's contract names (hermit 3), and constructors for the hermit-specific answers. The
//! envelope itself is the platform's, `aviary_core::http::{ApiError, ErrorBody}` (platform 3.6), re-exported here.

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
    pub const INTERNAL: &str = "internal";
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

pub use aviary_core::http::{ApiError, ErrorBody};
use axum::http::StatusCode;

/// An error answer from a numeric status (an unknown status becomes 500).
#[must_use]
pub fn api(status: u16, code: &str, message: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        code,
        message,
    )
}

/// 501 `not_supported`: the implementation lacks this call (a normal answer, hermit 3.1).
#[must_use]
pub fn not_supported() -> ApiError {
    api(501, codes::NOT_SUPPORTED, "not supported by this implementation")
}

/// 403 `not_allowed`: the person is not allowed by the catalogue (or a token is not valid).
#[must_use]
pub fn not_allowed() -> ApiError {
    api(403, codes::NOT_ALLOWED, "not allowed")
}

/// A 404 with a domain code (`no_sandbox`, `no_workspace`, `not_granted`, …).
#[must_use]
pub fn not_found(code: &str, message: impl Into<String>) -> ApiError {
    api(404, code, message)
}

/// 503 `sandbox_busy` while the operator has sandboxes disabled (hermit 3.6.2):
/// `{reason: "disabled", retryAfterS: 300}` with `retry-after: 300`.
#[must_use]
pub fn sandboxes_disabled() -> ApiError {
    api(503, codes::SANDBOX_BUSY, "sandboxes are disabled by the operator")
        .with("reason", "disabled")
        .with("retryAfterS", 300)
        .retry_after(300)
}
