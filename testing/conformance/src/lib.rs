//! `aviary-conformance`: checks an implementation of an aviary contract from the outside (platform 8).
//!
//! - [`hermit`]: hermit spec 24.1, against a live hermit (any implementation) with a fake fabric.
//! - [`gateway`]: platform 8.2, against a sandbox gateway, with the suite's own fakes of magpie, hermit, tools and
//!   the model upstream served on one listener that answers by `Host` ([`fakes`]).
//! - [`model`]: platform 8.3, the models route and the Messages API the deployment's model route serves.
//!
//! Every suite writes one line per check to stdout and the whole run as JUnit XML ([`report`]); the process exits 0
//! only when no check failed.

// short names for answers and sandboxes read better in the checks; JUnit is a proper name here
#![allow(clippy::many_single_char_names, clippy::doc_markdown)]

pub mod fakes;
pub mod gateway;
pub mod hermit;
pub mod model;
pub mod proxy;
pub mod report;
