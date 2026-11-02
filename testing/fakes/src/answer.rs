//! Turning a scripted [`Reply`] into the return types of the contract traits.

use aviary_core::http::ApiError;
use serde::de::DeserializeOwned;

use crate::script::{Reply, ReplyBody};

/// The contract's answer for a call nobody scripted and the fake does not model.
#[must_use]
pub fn not_supported(route: &str) -> ApiError {
    hermit_api::error::api(
        501,
        hermit_api::error::codes::NOT_SUPPORTED,
        format!("fake: {route} is not scripted"),
    )
}

/// An idempotent create (magpie's `Admitted`): 200 is a repeat, any other success is new.
///
/// # Errors
/// The scripted error, or 500 when the body does not parse.
pub fn admitted<T: DeserializeOwned>(r: &Reply, route: &str) -> Result<magpie_api::Admitted<T>, ApiError> {
    let body = r.typed::<T>(route)?;
    Ok(if r.status == 200 {
        magpie_api::Admitted::Repeat(body)
    } else {
        magpie_api::Admitted::New(body)
    })
}

/// An optional body: 204 (or no body) is `None`.
///
/// # Errors
/// The scripted error, or 500 when the body does not parse.
pub fn option<T: DeserializeOwned>(r: &Reply, route: &str) -> Result<Option<T>, ApiError> {
    if r.is_error() {
        return Err(r.api_error());
    }
    if r.status == 204 || r.body == ReplyBody::None {
        return Ok(None);
    }
    r.typed(route).map(Some)
}
