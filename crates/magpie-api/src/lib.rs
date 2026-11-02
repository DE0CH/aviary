//! magpie's contract: types, routes, client and fixtures for the person, sandbox and hermit/ops/admin surfaces.
//!
//! This crate is the text of record for magpie's REST API (platform 4.4). pigeon, the supervisor, hermit and the ops
//! tools call magpie through [`client::Client`]; magpie and the fakes serve it through [`server::Server`]. Both sides use the same types, the same route constants ([`routes`]) and the same golden fixtures
//! (`fixtures/`), so a disagreement is a build or test failure, not a production bug.
//!
//! Conventions (platform 3.4-3.6): every route is under `/v1/`; bodies and query parameters are camelCase; times are
//! RFC 3339 UTC with milliseconds ([`Timestamp`]); every non-2xx JSON answer is the flat error envelope
//! [`errors::ApiError`]. The only `snake_case` keys on the wire are values that are not magpie's API: Claude Code's own
//! transcript lines (`data` of `cc.*` events) and the opaque values magpie carries for it (a tool call's `input`, an
//! ask's `input`/`updatedInput`, the Messages API response in a `title` ack).
//!
//! Clients ignore unknown response fields; servers ignore unknown request fields. Enumerations that magpie may grow
//! (error codes, event kinds, row kinds, change types, command types) have a fallback variant, so an older reader
//! skips what it does not know instead of failing.

pub mod changes;
pub mod client;
mod common;
pub mod conversations;
pub mod errors;
pub mod events;
pub mod files;
pub mod live;
pub mod mime;
pub mod ops;
pub mod routes;
pub mod rows;
pub mod sandbox;
pub mod server;
pub mod shares;
pub mod turns;
pub mod uploads;

pub use common::{Admitted, Empty, Timestamp, flag01};

/// The person surface (`magpie-person`): pigeon's routes, and the hermit, ops and admin routes, which are in-cluster
/// REST under the same rules. Sent as `Aviary-Protocol: magpie-person=1`.
pub const PROTOCOL_PERSON: u32 = 1;

/// The sandbox surface (`magpie-sandbox`): the supervisor's routes through the gateway. Fixed for a sandbox's life,
/// so magpie accepts this number and the previous one (platform 3.4).
pub const PROTOCOL_SANDBOX: u32 = 1;

/// Surface names, as they appear in `Aviary-Protocol` and in `/version`.
pub const SURFACE_PERSON: &str = "magpie-person";
/// See [`SURFACE_PERSON`].
pub const SURFACE_SANDBOX: &str = "magpie-sandbox";

/// The `Aviary-*` header names (platform 3.5), from `aviary-core`.
pub use aviary_core::http::headers;

/// Pair-token callers; which caller may use which route is [`routes::ROUTES`].
pub use aviary_core::auth::Caller;
