//! The error envelope, magpie's HTTP error codes, and the dotted data codes carried inside turns and rows.
//!
//! Every non-2xx JSON answer is flat: `{"code": "<lower_snake_case>", "message": "<English, for logs>", …}`
//! (platform 3.6), never `{"error": {…}}`. pigeon localises by code; magpie never produces sentences for people.
//!
//! [`Refusal`] is magpie's envelope with typed fields, as the client reads it and as magpie builds it; it converts
//! into `aviary-core`'s [`aviary_core::http::ApiError`] (the one answer type the services' routers return), so the
//! workspace has one wire envelope.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::common::open_enum;

open_enum! {
    /// HTTP error codes (the `code` of [`Refusal`]). Each has its status in [`ErrorCode::status`].
    pub enum ErrorCode {
        /// 400: a malformed request.
        BadRequest = "bad_request",
        /// 401: missing or unknown pair token (with `www-authenticate: Bearer`).
        Unauthenticated = "unauthenticated",
        /// 403: a valid token on a route its caller may not use.
        WrongCaller = "wrong_caller",
        /// 404: not found, or not the person's (ids never leak through 403).
        NotFound = "not_found",
        /// 410: a change-feed cursor older than the retention.
        CursorExpired = "cursor_expired",
        /// 413: a body, file, line or text over its limit.
        TooLarge = "too_large",
        /// 415: not a supported image, or a preview of a non-image.
        UnsupportedType = "unsupported_type",
        /// 429.
        RateLimited = "rate_limited",
        /// 500: a magpie bug or a failed integrity check.
        Internal = "internal",
        /// 501: a route this server (a fake) does not implement (the same code as hermit-api's).
        NotSupported = "not_supported",
        /// 503: Postgres, the bucket or hermit cannot be reached (with `retry-after`).
        Unavailable = "unavailable",
        /// 507: the conversation's Claude Code state is over its quota.
        QuotaExceeded = "quota_exceeded",
        /// 400 (M1): a list cursor that does not parse.
        BadCursor = "bad_cursor",
        /// 400 (M3, M7): an upload id or path that names no unattached upload of the person.
        UnknownUpload = "unknown_upload",
        /// 400 (M3, M7): more than 10 images or 10 files.
        TooManyAttachments = "too_many_attachments",
        /// 400 (M5).
        EmptyTitle = "empty_title",
        /// 400 (M5).
        TitleTooLong = "title_too_long",
        /// 400 (M14, sent files): a path that is absolute, not normalised, has `..` or NUL, or is too long.
        BadPath = "bad_path",
        /// 400 (blob PUT): the body does not hash to the named sha.
        ShaMismatch = "sha_mismatch",
        /// 403: a state change refused while an admin views as the person.
        ViewAsReadOnly = "view_as_read_only",
        /// 503 (pigeon, M3/M7): the view-as audit row could not be written.
        ViewAsNotRecorded = "view_as_not_recorded",
        /// 403 (cc unlink): the main transcript cannot be removed (EPERM).
        Protected = "protected",
        /// 409: the conversation is owned by the switch (carries `retryAfterMs`, and `route: "legacy"` in the window).
        ConversationMigrating = "conversation_migrating",
        /// 409 (M7).
        ConversationDeleted = "conversation_deleted",
        /// 409 (M6 `DELETE`): only a conversation in Recently deleted can be deleted for good.
        NotDeleted = "not_deleted",
        /// 409 (M7): a turn is already admitted, preparing, running or stopping.
        TurnRunning = "turn_running",
        /// 409 (M11).
        AlreadyResolved = "already_resolved",
        /// 409 (M16 create): no finished assistant row to share.
        NothingToShare = "nothing_to_share",
        /// 500 (M16 create): hermit's workspace clone failed.
        ShareFailed = "share_failed",
        /// 500 (M16 continue): hermit's workspace clone failed.
        ContinueFailed = "continue_failed",
        /// 409 (hermit person purge): hermit still allows the person.
        PersonAllowed = "person_allowed",
        /// 409 (lease): the caller is not hermit's current sandbox of the conversation.
        NotCurrentSandbox = "not_current_sandbox",
        /// 409: the call's lease is not the current one (the supervisor answers EIO and stops).
        LeaseLost = "lease_lost",
        /// 409 (cc write): the append's offset is not the file's size.
        OffsetMismatch = "offset_mismatch",
        /// 409 (cc write/truncate): the offset is past the end of the file.
        OffsetBeyondEof = "offset_beyond_eof",
        /// 409 (cc mkdir).
        Exists = "exists",
        /// 409 (cc rmdir).
        NotEmpty = "not_empty",
        /// 409 (turn state): a transition 8.1 does not allow (carries the current `state`).
        BadTransition = "bad_transition",
        /// 409 (turn end): the turn's last transcript line is not stored yet; retried.
        TranscriptPending = "transcript_pending",
        /// 409 (live frames): the turn is not the conversation's active turn; frames are dropped, never retried.
        TurnNotActive = "turn_not_active",
    }
}

impl ErrorCode {
    /// The HTTP status this code is answered with.
    #[must_use]
    pub fn status(&self) -> u16 {
        use ErrorCode as C;
        match self {
            C::BadRequest
            | C::BadCursor
            | C::UnknownUpload
            | C::TooManyAttachments
            | C::EmptyTitle
            | C::TitleTooLong
            | C::BadPath
            | C::ShaMismatch => 400,
            C::Unauthenticated => 401,
            C::WrongCaller | C::ViewAsReadOnly | C::Protected => 403,
            C::NotFound => 404,
            C::CursorExpired => 410,
            C::TooLarge => 413,
            C::UnsupportedType => 415,
            C::RateLimited => 429,
            C::Internal | C::ShareFailed | C::ContinueFailed | C::Other(_) => 500,
            C::NotSupported => 501,
            C::Unavailable | C::ViewAsNotRecorded => 503,
            C::QuotaExceeded => 507,
            C::ConversationMigrating
            | C::ConversationDeleted
            | C::NotDeleted
            | C::TurnRunning
            | C::AlreadyResolved
            | C::NothingToShare
            | C::PersonAllowed
            | C::NotCurrentSandbox
            | C::LeaseLost
            | C::OffsetMismatch
            | C::OffsetBeyondEof
            | C::Exists
            | C::NotEmpty
            | C::BadTransition
            | C::TranscriptPending
            | C::TurnNotActive => 409,
        }
    }
}

/// The error envelope of every non-2xx JSON answer, with magpie's typed fields.
///
/// `status` is the HTTP status (not on the wire): set from [`ErrorCode::status`] by [`Refusal::new`], and from the
/// response by the client, which also reads `retry-after`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    /// HTTP status of the answer (not serialised).
    #[serde(skip, default = "default_status")]
    pub status: u16,
    /// The `retry-after` header, seconds (not in the body).
    #[serde(skip)]
    pub retry_after: Option<u64>,
    /// The machine-readable code.
    pub code: ErrorCode,
    /// English, for logs.
    pub message: String,
    /// `conversation_migrating`: when to try again (2000, or 5000 while an old turn runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// `conversation_migrating` during the switch window: `"legacy"`, the conversation's writes go through pigeon's
    /// legacy upstream (platform R37).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteHint>,
    /// Type-specific fields beyond these (`state` of `bad_transition`: the turn's current state), and fields a
    /// newer magpie adds, kept as received.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_status() -> u16 {
    500
}

impl Refusal {
    /// An error with the code's own status.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Refusal {
            status: code.status(),
            retry_after: None,
            code,
            message: message.into(),
            retry_after_ms: None,
            route: None,
            extra: Map::new(),
        }
    }

    /// 404 `not_found`.
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(ErrorCode::NotFound, "not found")
    }

    /// 409 `conversation_migrating` with `retryAfterMs`.
    #[must_use]
    pub fn migrating(retry_after_ms: u64) -> Self {
        let mut e = Self::new(ErrorCode::ConversationMigrating, "conversation is being moved");
        e.retry_after_ms = Some(retry_after_ms);
        e
    }

    /// 409 `conversation_migrating` with `route: "legacy"` (the switch window's re-route answer, platform R37).
    #[must_use]
    pub fn legacy_route() -> Self {
        let mut e = Self::new(
            ErrorCode::ConversationMigrating,
            "conversation is owned by the old stack",
        );
        e.route = Some(RouteHint::Legacy);
        e
    }

    /// 409 `bad_transition` with the turn's current state.
    #[must_use]
    pub fn bad_transition(current: &str) -> Self {
        let mut e = Self::new(ErrorCode::BadTransition, format!("the turn is {current}"));
        e.extra.insert("state".to_owned(), Value::String(current.to_owned()));
        e
    }

    /// `state` of a `bad_transition` answer.
    #[must_use]
    pub fn current_state(&self) -> Option<&str> {
        self.extra.get("state").and_then(Value::as_str)
    }

    /// The same error with another HTTP status (an unknown code read off the wire).
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }
}

/// Where pigeon must send a conversation's writes (`route` of `conversation_migrating`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RouteHint {
    /// Through the legacy upstream (the switch window).
    Legacy,
    /// A value this build does not know.
    #[serde(other)]
    Unknown,
}

impl From<Refusal> for aviary_core::http::ApiError {
    fn from(r: Refusal) -> Self {
        let status =
            axum::http::StatusCode::from_u16(r.status).unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let mut e = aviary_core::http::ApiError::new(status, r.code.as_str(), r.message);
        if let Some(ms) = r.retry_after_ms {
            e = e.with("retryAfterMs", ms);
        }
        if let Some(route) = r.route {
            e = e.with("route", serde_json::to_value(route).unwrap_or(Value::Null));
        }
        for (k, v) in r.extra {
            e = e.with(&k, v);
        }
        if let Some(secs) = r.retry_after {
            e = e.retry_after(secs);
        }
        e
    }
}

impl axum::response::IntoResponse for Refusal {
    fn into_response(self) -> axum::response::Response {
        aviary_core::http::ApiError::from(self).into_response()
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for Refusal {}

open_enum! {
    /// Data codes inside turns, rows and reports (dotted; not HTTP codes, platform 3.6). magpie never produces
    /// sentences: pigeon localises by code (magpie 8.9, pigeon 7.9).
    pub enum DataCode {
        /// hermit has no capacity, or sandboxes are disabled (`detail` = hermit's reason).
        SandboxBusy = "sandbox.busy",
        /// hermit answered 403 `not_allowed`.
        SandboxNotAllowed = "sandbox.not_allowed",
        /// The sandbox could not be started.
        SandboxFailed = "sandbox.failed",
        /// Not ready within 15 min.
        SandboxTimeout = "sandbox.timeout",
        /// Started, but the supervisor never took the turn or went silent while hermit sees it.
        SandboxUnreachable = "sandbox.unreachable",
        /// The sandbox died or was released mid-turn.
        SandboxLost = "sandbox.lost",
        /// Claude Code exited abnormally (`message` carries the exit code).
        RunExited = "run.exited",
        /// The supervisor's memory guard killed Claude Code.
        RunOom = "run.oom",
        /// The supervisor reported an error, or reconciliation found the turn unknown.
        RunError = "run.error",
        /// One transcript write kept failing for 10 min while magpie was reachable.
        CcWriteStuck = "cc.write_stuck",
        /// A cancel not confirmed in 30 s.
        StopForced = "stop.forced",
        /// A magpie bug.
        Internal = "internal",
        /// Model errors (tags on an `API Error:` assistant line): the budget is used up (402).
        ModelBudget = "model.budget",
        /// 429, `rate_limit_error`.
        ModelRateLimited = "model.rate_limited",
        /// >= 500, 529, `overloaded_error`, `api_error`.
        ModelUnavailable = "model.unavailable",
        /// 401/403.
        ModelAccess = "model.access",
        /// Other 4xx, with the provider's message.
        ModelRefused = "model.refused",
        /// No status: the model could not be reached.
        ModelConnection = "model.connection",
    }
}

/// An error carried as data: a failed turn's error, a failed tool row, a model error tag (pigeon 14.1.0 `ErrorInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorInfo {
    /// The dotted data code.
    pub code: DataCode,
    /// English for logs; for `model.budget` and `model.refused`, egress's or the provider's own words.
    #[serde(default)]
    pub message: String,
    /// The model's HTTP status, for `model.*` codes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Extra detail (for `sandbox.busy`, hermit's reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// `sandbox.busy`: hermit's suggested wait in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
}

impl ErrorInfo {
    /// An error with a code and an English message.
    #[must_use]
    pub fn new(code: DataCode, message: impl Into<String>) -> Self {
        ErrorInfo {
            code,
            message: message.into(),
            status: None,
            detail: None,
            retry_after_s: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_codes_round_trip_and_answer_500() {
        let e: Refusal = serde_json::from_str(r#"{"code":"brand_new","message":"m","hint":3}"#).unwrap();
        assert_eq!(e.code, ErrorCode::Other("brand_new".into()));
        assert_eq!(e.code.status(), 500);
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"code":"brand_new","message":"m","hint":3}"#
        );
    }

    #[test]
    fn every_known_code_is_lower_snake_case_and_has_a_status() {
        for c in ErrorCode::KNOWN {
            assert!(c.as_str().chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'), "{c}");
            assert!((400..600).contains(&c.status()), "{c}");
        }
        for c in DataCode::KNOWN {
            assert!(
                c.as_str()
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_' || ch == '.'),
                "{c}"
            );
        }
    }

    #[test]
    fn the_envelope_stays_small_and_reads_extras() {
        assert!(
            std::mem::size_of::<Refusal>() <= 128,
            "{}",
            std::mem::size_of::<Refusal>()
        );
        let e = Refusal::bad_transition("running");
        let back: Refusal = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back.current_state(), Some("running"));
        let odd: Refusal =
            serde_json::from_str(r#"{"code":"conversation_migrating","message":"m","route":"elsewhere"}"#).unwrap();
        assert_eq!(odd.route, Some(RouteHint::Unknown));
    }

    #[test]
    fn a_refusal_becomes_the_core_answer_unchanged() {
        let mut r = Refusal::migrating(2000);
        r.retry_after = Some(2);
        r.extra.insert("hint".into(), Value::from(1));
        let core = aviary_core::http::ApiError::from(r.clone());
        assert_eq!(core.status().as_u16(), 409);
        assert_eq!(
            serde_json::to_value(core.body()).unwrap(),
            serde_json::to_value(&r).unwrap()
        );
    }

    #[test]
    fn the_envelope_is_flat() {
        let e = Refusal::legacy_route();
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"code": "conversation_migrating",
                               "message": "conversation is owned by the old stack", "route": "legacy"})
        );
    }
}
