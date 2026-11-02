//! Turns (magpie 8). WP-14 fills this module (admission, the driver, commands, reports, cancel, the watchdog);
//! WP-11 ships the seam the create route calls.

use aviary_pg::Tx;
use magpie_api::errors::ErrorCode;
use magpie_api::turns::{SendRequest, TurnStarted};

use crate::conversations::Person;
use crate::error::{Error, Result};

/// Whether a message carries anything to admit (text or attachments, magpie 9.2).
#[must_use]
pub fn has_content(message: &SendRequest) -> bool {
    !message.text.trim().is_empty() || !message.images.is_empty() || !message.files.is_empty()
}

/// Admits `message` as a person turn of `conversation` (locked by the caller, created in this transaction) exactly
/// as M7 does (magpie 8.2), and answers the turn. WP-14's admission replaces this body; until then a create whose
/// first message has content answers 501 `not_supported` and nothing is committed.
///
/// # Errors
/// 501 `not_supported` (until WP-14).
#[allow(clippy::unused_async)] // WP-14's admission awaits its statements
pub async fn admit_first(
    _tx: &Tx<'_>,
    _conversation: uuid::Uuid,
    _person: &Person,
    _message: &SendRequest,
) -> Result<TurnStarted> {
    Err(Error::code(
        ErrorCode::NotSupported,
        "turn admission is not built yet (WP-14)",
    ))
}
