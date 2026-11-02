//! View-as and its audit (magpie 13): pigeon records the start of a view (M17) before it opens it, and every state
//! change an admin makes while viewing as a person is recorded in the same transaction as the change, so "record,
//! then act" never half-fails. Reads are not recorded per call (the `start` row covers the session).

use magpie_api::ops::{ViewAsEvent, ViewAsRequest};
use uuid::Uuid;

use crate::conversations::Person;
use crate::db::Db;
use crate::error::{Error, Result};

/// The longest view session id.
pub const SESSION_MAX_CHARS: usize = 64;

fn event_name(e: ViewAsEvent) -> &'static str {
    match e {
        ViewAsEvent::Start => "start",
        ViewAsEvent::Create => "create",
        ViewAsEvent::Send => "send",
        ViewAsEvent::Cancel => "cancel",
        ViewAsEvent::Upload => "upload",
        ViewAsEvent::AdminRead => "admin_read",
    }
}

/// Records `event` of the admin viewing as `person`, in `db` (the change's own transaction); nothing when no admin
/// is viewing. `chars` is the message's length for a send and the size for an upload, never content.
///
/// # Errors
/// When the statement fails.
pub async fn record(
    db: &impl Db,
    person: &Person,
    event: ViewAsEvent,
    conversation: Option<Uuid>,
    seq: Option<i64>,
    chars: Option<u64>,
) -> Result<()> {
    let Some(v) = &person.viewer else {
        return Ok(());
    };
    let chars = chars.map(|c| i32::try_from(c).unwrap_or(i32::MAX));
    db.execute(
        "insert into view_as_log (admin, person, session, event, conversation_id, seq, chars)
         values ($1, $2, $3, $4, $5, $6, $7)",
        &[
            &v.email,
            &person.email,
            &v.session,
            &event_name(event),
            &conversation,
            &seq,
            &chars,
        ],
    )
    .await?;
    Ok(())
}

fn email(s: &str) -> Option<String> {
    let e = s.trim().to_lowercase();
    (e.find('@').is_some_and(|i| i > 0) && !e.contains(char::is_whitespace)).then_some(e)
}

/// M17 `POST /v1/view-as`: records a view's start (pigeon opens the view only after this answers).
///
/// # Errors
/// 400 for an admin or person that is not an email, an empty or overlong session, a view that has already ended; a
/// statement error.
pub async fn start(db: &impl Db, req: &ViewAsRequest) -> Result<()> {
    let admin = email(&req.admin).ok_or_else(|| Error::bad_request("admin is not an email"))?;
    let person = email(&req.person).ok_or_else(|| Error::bad_request("person is not an email"))?;
    let session = req.session.trim();
    if session.is_empty() || session.chars().count() > SESSION_MAX_CHARS {
        return Err(Error::bad_request("session must be 1-64 characters"));
    }
    if req.expires_at.0 <= time::OffsetDateTime::now_utc() {
        return Err(Error::bad_request("the view has already ended"));
    }
    db.execute(
        "insert into view_as_log (admin, person, session, event) values ($1, $2, $3, 'start')",
        &[&admin, &person, &session],
    )
    .await?;
    tracing::info!(person_key = %aviary_core::log::person_key(&person), "view-as started");
    Ok(())
}

/// An admin's read of `conversation` through the admin routes (magpie 13): the first one per admin, conversation
/// and (UTC) day writes an `admin_read` row; a unique index makes concurrent first reads write one.
///
/// # Errors
/// When a statement fails.
pub async fn admin_read(db: &impl Db, admin: &str, person: &str, conversation: Uuid) -> Result<()> {
    db.execute(
        "insert into view_as_log (admin, person, session, event, conversation_id)
         values ($1, $2, 'admin', 'admin_read', $3)
         on conflict (admin, conversation_id, ((at at time zone 'UTC')::date)) where event = 'admin_read'
         do nothing",
        &[&admin, &person, &conversation],
    )
    .await?;
    Ok(())
}
