//! hermit's contract (hermit spec 3, 6, 13.13; platform 3, R33-R35): the request, response and event types, the
//! route constants and caller table, the protocol numbers, the sandbox and control token codec, a typed [`Client`]
//! and a [`Server`] trait with HTTP routers for it ([`guarded_router`] enforces the caller table). Golden fixtures in
//! `fixtures/` pin every type's JSON; `vectors/` holds the token formats' known-answer vector.
//!
//! Surfaces: `hermit-core` (pigeon, magpie, the gateway's lookups, tools, monitor, admin) and `hermit-sandbox` (the
//! supervisor through the gateway). `supervisor-api` re-exports the sandbox-surface types; `gateway-api`
//! re-exports the token codec and lookups.

pub mod admin;
pub mod client;
pub mod deployment;
pub mod error;
pub mod exec;
pub mod headers;
pub mod ops;
pub mod people;
pub mod routes;
pub mod sandbox;
pub mod sandbox_surface;
pub mod server;
pub mod time;
pub mod token;
mod wire;
pub mod workspaces;

pub use client::{Client, ClientError, FileDownload, Refusal};
pub use error::{ApiError, ErrorBody};
pub use server::{Ctx, RouteGuard, SandboxIdentity, SelfAnswer, Server, guarded_router, router};
pub use time::Timestamp;

/// A protocol surface and its current number (platform 3.4).
pub type Surface = (&'static str, u32);

/// `hermit-core`: everything but the sandbox's own routes.
pub const CORE: Surface = ("hermit-core", 1);
/// `hermit-sandbox`: `/v1/sandbox/**`, spoken by the supervisor. Fixed for a sandbox's life, so hermit must keep
/// accepting the previous number until every sandbox on an older image is gone.
pub const SANDBOX: Surface = ("hermit-sandbox", 1);

/// The protocol a server accepts: its current number and the previous one.
#[must_use]
pub fn accepts(surface: Surface, n: u32) -> bool {
    n == surface.1 || (surface.1 > 1 && n == surface.1 - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_current_and_previous() {
        assert!(accepts(CORE, 1));
        assert!(!accepts(CORE, 0));
        assert!(!accepts(CORE, 2));
        assert!(accepts(("x", 3), 2));
        assert!(!accepts(("x", 3), 1));
    }
}
