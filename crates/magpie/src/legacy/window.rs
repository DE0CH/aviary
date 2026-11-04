//! The window's state for pigeon (`GET /v1/legacy/window`, [`magpie_api::legacy::WindowState`]) and the raw bytes
//! of an upload that pigeon copies into the old child (`GET /v1/uploads/{uploadId}/raw`, DESIGN 9.5).

use axum::body::Body;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use magpie_api::Timestamp;
use magpie_api::legacy::WindowState;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::Magpie;
use crate::blobs::Sha;
use crate::conversations::Person;
use crate::error::{Error, Result};

/// The gate (for everyone, else the person's own, cutover 7.9 `allow-handover --person`), the rollback mark, whether
/// anything is legacy, and how much of it is the person's.
///
/// # Errors
/// The database's.
pub async fn state(app: &Magpie, person: &Person) -> Result<WindowState> {
    let row = app
        .pool
        .get()
        .await?
        .query_one(
            "select coalesce(c.handover_allowed_at,
                             (select g.allowed_at from import_gate_persons g where g.person = $1)),
                    c.rollback_at,
                    exists (select 1 from conversations where migrating),
                    (select count(*) from conversations where person = $1 and migrating)
               from import_control c",
            &[&person.email],
        )
        .await?;
    let gate: Option<OffsetDateTime> = row.get(0);
    let rollback: Option<OffsetDateTime> = row.get(1);
    let mine: i64 = row.get(3);
    Ok(WindowState {
        handover_allowed_at: gate.map(Timestamp),
        rollback_at: rollback.map(Timestamp),
        any_legacy: row.get(2),
        person_legacy: u64::try_from(mine).unwrap_or(0),
    })
}

/// The upload's stored bytes, `Content-Type` its type; 404 for an upload that is not the person's.
///
/// # Errors
/// 404, the database's, the bucket's.
pub async fn upload_raw(app: &Magpie, person: &Person, upload: Uuid) -> Result<Response> {
    let row = app
        .pool
        .get()
        .await?
        .query_opt(
            "select blob, mime from uploads where id = $1 and person = $2",
            &[&upload, &person.email],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let blob: Vec<u8> = row.get(0);
    let mime: String = row.get(1);
    let sha = Sha::try_from(blob.as_slice()).map_err(|_| Error::Internal("a blob id is not 32 bytes".into()))?;
    let mut found = crate::blobs::fetch_stored(app, &[sha]).await?;
    let bytes = found
        .remove(&sha)
        .ok_or_else(|| Error::Internal("an upload's blob was not fetched".into()))?;
    let mut r = Response::new(Body::from(bytes));
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}
