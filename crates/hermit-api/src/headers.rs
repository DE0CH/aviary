//! The `Aviary-*` header names hermit's contract uses (platform 3.5). The gateway's three are re-exported by
//! `gateway-api`.

/// Correlation id (uuid v7), forwarded unchanged and logged as `request`.
pub const REQUEST: &str = "aviary-request";
/// `<surface>=<n>` on every client call (platform 3.4).
pub const PROTOCOL: &str = "aviary-protocol";
/// The signed-in person, set by pigeon (required on `/ops/*` from pigeon).
pub const PERSON: &str = "aviary-person";
/// The operator behind an admin route (recorded in hermit's audit log).
pub const ADMIN: &str = "aviary-admin";
/// The verified sandbox behind a forwarded `/v1/sandbox/**` call: set by the gateway only.
pub const SANDBOX_ID: &str = "aviary-sandbox-id";
pub const SANDBOX_CONVERSATION: &str = "aviary-sandbox-conversation";
/// The owner's trimmed, lowercased email.
pub const SANDBOX_PERSON: &str = "aviary-sandbox-person";
/// The prefix every service-defined header shares; receivers strip client-supplied ones (platform 3.5).
pub const PREFIX: &str = "aviary-";
