//! magpie's answers as FUSE outcomes, once (supervisor 9.5 items 4-7, magpie 6.12's table).

use magpie_api::client::ClientError;
use magpie_api::errors::ErrorCode;

pub use libc::{
    EACCES, EDQUOT, EEXIST, EFBIG, EINTR, EINVAL, EIO, ENAMETOOLONG, ENODATA, ENOENT, ENOSYS, ENOTDIR, ENOTEMPTY,
    ENOTSUP, EOPNOTSUPP, EPERM, EXDEV,
};

/// What one failed magpie call means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// 503, a connection error, a timeout, a gateway 502/504, and every answer 9.5 item 5 does not name as
    /// definitive (a 500, another 5xx, an unknown code such as 409 `conversation_migrating`, a body that does not
    /// decode during a deploy's version skew): retried with the same `opId` under the general-unavailability and
    /// stuck-op rule of 9.5 item 4, so one transient answer never fails a transcript append.
    Retry,
    /// A definitive answer: this errno, no retry.
    Errno(i32),
    /// 409 `offset_mismatch` / `offset_beyond_eof`: the repair from the cache (9.5 item 6).
    Repair,
    /// 409 `lease_lost`: EIO to everything in flight, the agent killed, the lease loop.
    LeaseLost,
}

/// Classifies a failed call. Only the answers 9.5 items 5-7 (magpie 6.12's table) name are definitive; everything
/// else is retried, and the stuck-op rule ends it with EIO after `STUCK_OP` while magpie answers other calls.
/// (A recorded reading of 9.5: its item 4 lists 503, connection errors, timeouts and 502/504; a 500 or an unknown
/// code is in neither list, and failing it at once would be the log-and-drop outcome 9.5 item 4 ends.)
#[must_use]
pub fn classify(e: &ClientError) -> Failure {
    match e {
        ClientError::Http(_) | ClientError::Decode { .. } => Failure::Retry,
        ClientError::Api(r) => match r.code {
            ErrorCode::NotFound => Failure::Errno(ENOENT),
            ErrorCode::Protected => Failure::Errno(EPERM),
            ErrorCode::Exists => Failure::Errno(EEXIST),
            ErrorCode::NotEmpty => Failure::Errno(ENOTEMPTY),
            ErrorCode::TooLarge => Failure::Errno(EFBIG),
            ErrorCode::QuotaExceeded => Failure::Errno(EDQUOT),
            ErrorCode::BadRequest | ErrorCode::BadPath => Failure::Errno(EINVAL),
            ErrorCode::OffsetMismatch | ErrorCode::OffsetBeyondEof => Failure::Repair,
            ErrorCode::LeaseLost => Failure::LeaseLost,
            _ => Failure::Retry,
        },
    }
}

/// Whether magpie itself refused before doing anything (a `503 unavailable` it answered): the call made no
/// operation, so an INTERRUPT may still answer EINTR (9.3's INTERRUPT row).
#[must_use]
pub fn refused_unexecuted(e: &ClientError) -> bool {
    matches!(e, ClientError::Api(r) if r.status == 503 && r.code == ErrorCode::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magpie_api::errors::Refusal;

    fn api(status: u16, code: ErrorCode) -> ClientError {
        let mut r = Refusal::new(code, "x");
        r.status = status;
        ClientError::Api(r)
    }

    #[test]
    fn the_table() {
        let cases = [
            (404, ErrorCode::NotFound, Failure::Errno(ENOENT)),
            (403, ErrorCode::Protected, Failure::Errno(EPERM)),
            (409, ErrorCode::Exists, Failure::Errno(EEXIST)),
            (409, ErrorCode::NotEmpty, Failure::Errno(ENOTEMPTY)),
            (413, ErrorCode::TooLarge, Failure::Errno(EFBIG)),
            (507, ErrorCode::QuotaExceeded, Failure::Errno(EDQUOT)),
            (400, ErrorCode::BadRequest, Failure::Errno(EINVAL)),
            (409, ErrorCode::OffsetMismatch, Failure::Repair),
            (409, ErrorCode::OffsetBeyondEof, Failure::Repair),
            (409, ErrorCode::LeaseLost, Failure::LeaseLost),
            (503, ErrorCode::Unavailable, Failure::Retry),
            (502, ErrorCode::Internal, Failure::Retry),
            (504, ErrorCode::Internal, Failure::Retry),
            (500, ErrorCode::Internal, Failure::Retry),
            (501, ErrorCode::NotSupported, Failure::Retry),
            (409, ErrorCode::ConversationMigrating, Failure::Retry),
            (429, ErrorCode::RateLimited, Failure::Retry),
        ];
        for (s, c, want) in cases {
            let what = format!("{s} {c:?}");
            assert_eq!(classify(&api(s, c)), want, "{what}");
        }
        let decode = ClientError::Decode {
            route: "cc_write",
            error: "x".into(),
        };
        assert_eq!(classify(&decode), Failure::Retry, "a deploy's version skew is retried");
        assert!(refused_unexecuted(&api(503, ErrorCode::Unavailable)));
        assert!(!refused_unexecuted(&api(504, ErrorCode::Internal)));
    }
}
