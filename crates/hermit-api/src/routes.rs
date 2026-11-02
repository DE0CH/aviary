//! Every hermit route as a constant (hermit 3, 26). Servers register these strings and clients build paths from
//! them with [`fill`], so the two cannot drift. Placeholders use axum's `{name}` syntax.

/// H1 (3.3).
pub const DEPLOYMENT: &str = "/v1/deployment";
/// The catalogue (3.4).
pub const PEOPLE: &str = "/v1/people";
/// H2 (3.4); `{email}` percent-encoded.
pub const PERSON: &str = "/v1/people/{email}";
/// H3 (3.5).
pub const WARM: &str = "/v1/warm";

/// `PUT` get-or-start, `GET` read or long-poll (3.6.2, 3.6.3).
pub const SANDBOX: &str = "/v1/conversations/{cid}/sandbox";
pub const SANDBOX_RELEASE: &str = "/v1/conversations/{cid}/sandbox/release";
pub const SANDBOX_PAUSE: &str = "/v1/conversations/{cid}/sandbox/pause";
pub const SANDBOX_RESUME: &str = "/v1/conversations/{cid}/sandbox/resume";
pub const SANDBOX_SNAPSHOT: &str = "/v1/conversations/{cid}/sandbox/snapshot";
/// The change feed (3.6.5).
pub const SANDBOX_EVENTS: &str = "/v1/sandbox-events";

/// Workspaces (3.8).
pub const WORKSPACE: &str = "/v1/workspaces/{wid}";
pub const WORKSPACE_ENTRIES: &str = "/v1/workspaces/{wid}/entries";
pub const WORKSPACE_FILES: &str = "/v1/workspaces/{wid}/files";
pub const WORKSPACE_CLONE: &str = "/v1/workspaces/{wid}/clone";

/// Token keys and liveness (3.9).
pub const SANDBOX_TOKEN_KEYS: &str = "/v1/sandbox-token-keys";
pub const SANDBOX_TOKEN: &str = "/v1/sandbox-tokens/{sandboxId}";

/// The prefix of the sandbox's own routes (3.7), reached only through the gateway.
pub const SANDBOX_SURFACE_PREFIX: &str = "/v1/sandbox/";
pub const SANDBOX_SELF: &str = "/v1/sandbox/self";
pub const SANDBOX_SECRETS: &str = "/v1/sandbox/secrets";
pub const SANDBOX_SECRET: &str = "/v1/sandbox/secrets/{key}";
pub const SANDBOX_ACTIVITY: &str = "/v1/sandbox/activity";

/// Operator page and snapshot (3.11): browser paths, not under `/v1/`.
pub const OPS_PREFIX: &str = "/ops/";
pub const OPS_PAGE: &str = "/ops/";
pub const OPS_SNAPSHOT: &str = "/ops/snapshot";

/// Admin routes (3.12, 16).
pub const ADMIN_HOSTS: &str = "/v1/admin/hosts";
pub const ADMIN_HOST: &str = "/v1/admin/hosts/{name}";
pub const ADMIN_SANDBOXES_ENABLED: &str = "/v1/admin/sandboxes-enabled";
pub const ADMIN_FRESH_HOST: &str = "/v1/admin/fresh-host";
pub const ADMIN_SANDBOXES: &str = "/v1/admin/sandboxes";
pub const ADMIN_SANDBOX_LOG: &str = "/v1/admin/sandboxes/{id}/log";
pub const ADMIN_SANDBOX_RELEASE: &str = "/v1/admin/sandboxes/{id}/release";
pub const ADMIN_SANDBOX_EXEC: &str = "/v1/admin/sandboxes/{id}/exec";
pub const ADMIN_RECYCLE: &str = "/v1/admin/recycle";
pub const ADMIN_PERSON_PURGE: &str = "/v1/admin/people/{email}/purge";
pub const ADMIN_EVENTS: &str = "/v1/admin/events";

/// The callers of hermit (3.2). The pair token decides which one is calling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Caller {
    Pigeon,
    Magpie,
    /// The sandbox gateway, also on the forwarded `/v1/sandbox/**`.
    Gateway,
    Tools,
    /// A read-only monitor (`/ops/snapshot` only).
    Monitor,
    /// The operator (Claude, runbooks); needs `Aviary-Admin`.
    Admin,
}

impl Caller {
    /// The suffix of the callee's accept variable, e.g. `HERMIT_ACCEPT_PIGEON`.
    #[must_use]
    pub fn env_suffix(self) -> &'static str {
        match self {
            Self::Pigeon => "PIGEON",
            Self::Magpie => "MAGPIE",
            Self::Gateway => "GATEWAY",
            Self::Tools => "TOOLS",
            Self::Monitor => "MONITOR",
            Self::Admin => "ADMIN",
        }
    }
}

/// One row of the route-to-caller table: a method, a route constant and who may call it.
#[derive(Clone, Copy, Debug)]
pub struct RouteRule {
    pub method: &'static str,
    pub route: &'static str,
    pub callers: &'static [Caller],
}

use Caller::{Admin, Gateway, Magpie, Monitor, Pigeon, Tools};

const fn rule(method: &'static str, route: &'static str, callers: &'static [Caller]) -> RouteRule {
    RouteRule { method, route, callers }
}

/// The route-to-caller table of hermit 3.2, in code. Anything not listed for a caller is 403 `wrong_caller`.
/// `HEAD` is allowed wherever `GET` is on `/ops/*`.
pub const RULES: &[RouteRule] = &[
    rule("GET", DEPLOYMENT, &[Pigeon, Magpie]),
    rule("GET", PEOPLE, &[Magpie]),
    rule("GET", PERSON, &[Pigeon, Magpie]),
    rule("POST", WARM, &[Pigeon]),
    rule("PUT", SANDBOX, &[Magpie]),
    rule("GET", SANDBOX, &[Magpie]),
    rule("POST", SANDBOX_RELEASE, &[Magpie]),
    rule("POST", SANDBOX_PAUSE, &[Magpie]),
    rule("POST", SANDBOX_RESUME, &[Magpie]),
    rule("POST", SANDBOX_SNAPSHOT, &[Magpie]),
    rule("GET", SANDBOX_EVENTS, &[Magpie]),
    rule("PUT", WORKSPACE, &[Magpie]),
    rule("GET", WORKSPACE, &[Magpie]),
    rule("DELETE", WORKSPACE, &[Magpie]),
    rule("GET", WORKSPACE_ENTRIES, &[Magpie]),
    rule("GET", WORKSPACE_FILES, &[Magpie]),
    rule("POST", WORKSPACE_CLONE, &[Magpie]),
    rule("GET", SANDBOX_TOKEN_KEYS, &[Gateway, Tools]),
    rule("GET", SANDBOX_TOKEN, &[Gateway, Tools]),
    rule("GET", SANDBOX_SELF, &[Gateway]),
    rule("GET", SANDBOX_SECRETS, &[Gateway]),
    rule("GET", SANDBOX_SECRET, &[Gateway]),
    rule("POST", SANDBOX_ACTIVITY, &[Gateway]),
    rule("GET", OPS_PAGE, &[Pigeon, Admin]),
    rule("GET", OPS_SNAPSHOT, &[Pigeon, Admin, Monitor]),
    rule("GET", ADMIN_HOSTS, &[Admin]),
    rule("PATCH", ADMIN_HOST, &[Admin]),
    rule("PUT", ADMIN_SANDBOXES_ENABLED, &[Admin]),
    rule("POST", ADMIN_FRESH_HOST, &[Admin]),
    rule("GET", ADMIN_SANDBOXES, &[Admin]),
    rule("GET", ADMIN_SANDBOX_LOG, &[Admin]),
    rule("POST", ADMIN_SANDBOX_RELEASE, &[Admin]),
    rule("POST", ADMIN_SANDBOX_EXEC, &[Admin]),
    rule("POST", ADMIN_RECYCLE, &[Admin]),
    rule("POST", ADMIN_PERSON_PURGE, &[Admin]),
    rule("GET", ADMIN_EVENTS, &[Admin]),
];

/// Whether `caller` may call `method route` (`route` is the constant, as matched by the router).
#[must_use]
pub fn allowed(caller: Caller, method: &str, route: &str) -> bool {
    let method = if method == "HEAD" && route.starts_with(OPS_PREFIX) {
        "GET"
    } else {
        method
    };
    RULES
        .iter()
        .any(|r| r.method == method && r.route == route && r.callers.contains(&caller))
}

/// Builds a path from a route constant, replacing each `{placeholder}` in order with the percent-encoded value.
///
/// # Panics
/// Never at run time for the constants of this crate with the right number of values: a mismatch is a programming
/// error caught by the crate's tests (debug assertion).
#[must_use]
pub fn fill(route: &str, values: &[&str]) -> String {
    let mut out = String::with_capacity(route.len() + 40);
    let mut rest = route;
    let mut values = values.iter();
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}').map_or(rest.len(), |c| open + c + 1);
        let value = values.next();
        debug_assert!(value.is_some(), "too few values for {route}");
        out.push_str(&encode_component(value.copied().unwrap_or_default()));
        rest = &rest[close.min(rest.len())..];
    }
    out.push_str(rest);
    debug_assert!(values.next().is_none(), "too many values for {route}");
    out
}

/// Percent-encodes everything but RFC 3986 unreserved characters (for a path segment or a query value).
#[must_use]
pub fn encode_component(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Builds `?a=1&b=x` from present pairs (`None` values are left out); empty when nothing is present.
#[must_use]
pub fn query(pairs: &[(&str, Option<String>)]) -> String {
    let mut out = String::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            out.push(if out.is_empty() { '?' } else { '&' });
            out.push_str(k);
            out.push('=');
            out.push_str(&encode_component(v));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_encodes_values() {
        assert_eq!(
            fill(PERSON, &["Staff+1@example.org"]),
            "/v1/people/Staff%2B1%40example.org"
        );
        assert_eq!(
            fill(SANDBOX_RELEASE, &["0190f6c2-0000-7000-8000-000000000001"]),
            "/v1/conversations/0190f6c2-0000-7000-8000-000000000001/sandbox/release"
        );
        assert_eq!(fill(DEPLOYMENT, &[]), DEPLOYMENT);
    }

    #[test]
    fn query_skips_absent() {
        assert_eq!(
            query(&[("waitMs", Some("25000".into())), ("afterVersion", None)]),
            "?waitMs=25000"
        );
        assert_eq!(query(&[("a", None)]), "");
    }

    #[test]
    fn caller_table() {
        assert!(allowed(Caller::Magpie, "PUT", SANDBOX));
        assert!(!allowed(Caller::Pigeon, "PUT", SANDBOX));
        assert!(allowed(Caller::Monitor, "HEAD", OPS_SNAPSHOT));
        assert!(!allowed(Caller::Monitor, "GET", OPS_PAGE));
        assert!(allowed(Caller::Gateway, "POST", SANDBOX_ACTIVITY));
        assert!(!allowed(Caller::Tools, "GET", SANDBOX_SELF));
        assert!(!allowed(Caller::Admin, "PUT", SANDBOX));
    }

    #[test]
    fn every_route_has_a_rule() {
        for r in RULES {
            assert!(
                r.route.starts_with("/v1/") || r.route.starts_with(OPS_PREFIX),
                "{}",
                r.route
            );
            assert_ne!(r.callers, &[] as &[Caller]);
        }
    }
}
