//! HTTP conventions (platform 3.4-3.6, 3.9): the error envelope, the `Aviary-*` headers, request ids, response
//! defaults, the outbound `reqwest` client and the retry helper.
//!
//! A service's API router is wrapped by [`standard`]: request ids ([`request_id`]), response defaults
//! ([`defaults`]), the error envelope for every non-JSON error answer ([`envelope`]) and a body limit. Routes are
//! guarded per router with [`crate::auth::Guard`]. An edge that takes requests from outside (pigeon's public site,
//! the gateway's sandbox side) also strips every `Aviary-*` header first with [`strip_all_aviary`], added outside
//! [`standard`].

mod client;
mod error;
pub mod headers;
mod middleware;
mod retry;

pub use client::{RequestBuilderExt, client_builder, tls_config};
pub use error::{ApiError, ErrorBody};
pub use middleware::{RequestId, current_request_id, defaults, envelope, json, request_id, standard, strip_all_aviary};
pub use retry::{Backoff, Idempotence, send};
