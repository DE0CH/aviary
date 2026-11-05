//! Wire constants of the shipped Paseo clients (pigeon spec 1.2): paths, query and fragment names, the signed-out
//! meta and a message type that the app builds in `testing/paseo-contract/clients.txt` were written against. They are
//! the same in every deployment and carry no deployment behaviour; they are the only place in pigeon where the two
//! client-era names appear (a CI source rule, platform 4.5, fails them anywhere else in the crate).
//!
//! Protocol 1 is the set below. The rename to protocol 2 (`/api/aviary/*`, `aviary_view_as`, …) is a follow-up
//! (spec 1.2) that adds a second set here beside this one.

/// The prefix of pigeon's own REST routes for protocol-1 clients.
pub const API: &str = "/api/jarvis";

/// `GET /api/jarvis/signin?next=` (5.3.1).
pub const SIGNIN: &str = "/api/jarvis/signin";
/// `GET /api/jarvis/app-signin?state=` (5.3.3).
pub const APP_SIGNIN: &str = "/api/jarvis/app-signin";
/// `GET /api/jarvis/me` (5.4).
pub const ME: &str = "/api/jarvis/me";
/// `POST /api/jarvis/view-as` (6.1).
pub const VIEW_AS: &str = "/api/jarvis/view-as";
/// `POST /api/jarvis/warm` (5.6).
pub const WARM: &str = "/api/jarvis/warm";
/// `/api/jarvis/sessions/{id}/…` (5.8, 5.10).
pub const SESSIONS: &str = "/api/jarvis/sessions/";
/// `/api/jarvis/shares/{shareId}…` (5.10).
pub const SHARES: &str = "/api/jarvis/shares/";

/// The query parameter that carries a view-as token on REST calls, file links and the socket URL (6.2).
pub const VIEW_AS_QUERY: &str = "jarvis_view_as";
/// The fragment the view-as start redirects to (`/#jarvis-view-as=<token>`, 6.1).
pub const VIEW_AS_FRAGMENT: &str = "jarvis-view-as=";

/// The app's `sessionStorage` key for a view-as tab's token (the app's `yyc/view-as.ts`), read by the raw view.
pub const VIEW_AS_STORAGE: &str = "yyc.view-as";

/// The meta the app shell carries when nobody is signed in (5.2.2).
pub const SIGNED_OUT_META: &str = r#"<meta name="yyc-signed-out" content="1">"#;

/// The socket message with a conversation's sandbox state (7.6.5).
pub const SANDBOX_STATUS: &str = "yyc.sandbox_status";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_paths_sit_under_the_prefix() {
        for p in [SIGNIN, APP_SIGNIN, ME, VIEW_AS, WARM, SESSIONS, SHARES] {
            assert!(p.starts_with(API), "{p}");
            assert!(p.starts_with("/api/"), "{p}");
        }
    }
}
