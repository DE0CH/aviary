//! H2 `GET /v1/people/{email}` and `GET /v1/people` (hermit 3.4), answered from the in-memory catalogue.

use hermit_api::error::{self, ApiError, codes};
use hermit_api::people::{People, Person, normalise_email};

use crate::catalogue::Catalogue;

fn no_catalogue() -> ApiError {
    ApiError::unavailable("no staff catalogue loaded", 5)
}

/// H2: 200 for every well-formed address.
///
/// # Errors
/// 400 `bad_email`; 503 before any catalogue loaded (hermit is not ready then).
pub fn person(cat: &Catalogue, raw: &str) -> Result<Person, ApiError> {
    let email = normalise_email(raw).ok_or_else(|| error::api(400, codes::BAD_EMAIL, "not an email address"))?;
    Ok(cat.get().ok_or_else(no_catalogue)?.person(&email))
}

/// The catalogue as loaded.
///
/// # Errors
/// 503 before any catalogue loaded.
pub fn people(cat: &Catalogue) -> Result<People, ApiError> {
    Ok(cat.get().ok_or_else(no_catalogue)?.people())
}
