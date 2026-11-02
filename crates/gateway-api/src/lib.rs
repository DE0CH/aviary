//! The sandbox gateway contract (platform 3.11, hermit 6.3; R32, R33, R18), for any gateway implementation and for
//! the sandbox side that talks through one.
//!
//! - [`credential`]: the one addressing mode, an HTTP proxy with `sbx:<sandbox token>` as its credential, and the
//!   boot environment that carries it (the token travels only there, DESIGN 12 D1).
//! - [`FixedName`]: `magpie`, `hermit`, `model`, `tools`, their paths, upstreams and body limits.
//! - [`headers`]: what the gateway strips and adds.
//! - [`liveness`]: the revocation view of R33 (served stale without limit while hermit fails).
//! - [`models`]: the models route of R18 and its client.
//! - Re-exported from `hermit-api`: the `hst1.` token codec and key set, the token-keys and liveness types and routes.

pub mod credential;
pub mod headers;
pub mod liveness;
pub mod models;

use std::time::Duration;

pub use hermit_api::routes::{SANDBOX_TOKEN, SANDBOX_TOKEN_KEYS};
pub use hermit_api::server::SandboxIdentity;
pub use hermit_api::token::{Claims, KeySet, TokenKey, TokenKeys, TokenLiveness, VerifyError, peek_kid};

/// `gateway`: the contract's protocol number (platform 3.4).
pub const GATEWAY: hermit_api::Surface = ("gateway", 1);

/// Error codes of the gateway's own refusals (JSON on fixed names; internet refusals are plain text).
pub mod codes {
    /// 403: the token is not valid, its sandbox is gone or its person not allowed, or the path is not routed.
    pub const NOT_ALLOWED: &str = "not_allowed";
    /// 502: the upstream could not be reached.
    pub const UPSTREAM_UNREACHABLE: &str = "upstream_unreachable";
    /// 503 with `retry-after: 1`: a draining replica refusing a new request.
    pub const UNAVAILABLE: &str = "unavailable";
    /// 413: a fixed-name request body over its limit.
    pub const TOO_LARGE: &str = "too_large";
}

/// Limits and timers of 3.11 and hermit 6.3.
pub mod limits {
    use super::Duration;

    /// Request body limit on the `magpie`, `hermit` and `model` names (64 MiB + 1 is 413).
    pub const FIXED_BODY_BYTES: u64 = 64 * 1024 * 1024;
    /// tools' own cap.
    pub const TOOLS_BODY_BYTES: u64 = 2 * 1000 * 1000;
    /// Long-polls held open at least this long.
    pub const LONG_POLL: Duration = Duration::from_secs(30);
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
    /// The `tools` route's request timeout (at least).
    pub const TOOLS_TIMEOUT: Duration = Duration::from_secs(180);
    /// First byte relayed within this of the upstream's.
    pub const FIRST_BYTE: Duration = Duration::from_millis(200);
    /// Added latency on `magpie` and `hermit`, p99.
    pub const HOT_PATH_P99: Duration = Duration::from_millis(5);
    /// Drain bounds: model streams and CONNECT tunnels, fixed-name requests, tools calls.
    pub const DRAIN_MODEL: Duration = Duration::from_secs(600);
    pub const DRAIN_FIXED: Duration = Duration::from_secs(60);
    pub const DRAIN_TOOLS: Duration = Duration::from_secs(180);
    /// Key set: refreshed every 5 min, and at most every 10 s on an unknown kid; the last set is kept on failure.
    pub const KEYS_REFRESH: Duration = Duration::from_secs(300);
    pub const KEYS_UNKNOWN_KID_MIN: Duration = Duration::from_secs(10);
    /// Liveness lookups: 1 s timeout, answers cached 30 s.
    pub const LIVENESS_TIMEOUT: Duration = Duration::from_secs(1);
    pub const LIVENESS_TTL: Duration = Duration::from_secs(30);
}

/// The fixed names (3.11.1): recognised by exact host match (no port, or port 80), never resolved by DNS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FixedName {
    Magpie,
    Hermit,
    Model,
    Tools,
}

impl FixedName {
    pub const ALL: [Self; 4] = [Self::Magpie, Self::Hermit, Self::Model, Self::Tools];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Magpie => "magpie",
            Self::Hermit => "hermit",
            Self::Model => "model",
            Self::Tools => "tools",
        }
    }

    /// The name a request's host (`Host` or the absolute-form authority) names: exact, ASCII case-insensitive, with
    /// no port or port 80.
    #[must_use]
    pub fn from_host(host: &str) -> Option<Self> {
        let name = match host.rsplit_once(':') {
            Some((name, "80")) => name,
            Some(_) => return None,
            None => host,
        };
        Self::ALL.into_iter().find(|n| n.as_str().eq_ignore_ascii_case(name))
    }

    /// Whether `method path` is routed on this name (3.11.3). The query is ignored; a path with dot segments, empty
    /// segments or percent-encoded `.`, `/` or `\` is never routed (it could leave `/v1/sandbox/`).
    #[must_use]
    pub fn allows(self, method: &str, path_and_query: &str) -> bool {
        let path = path_and_query.split_once('?').map_or(path_and_query, |(p, _)| p);
        if !clean_path(path) {
            return false;
        }
        match self {
            Self::Magpie | Self::Hermit => path.starts_with(hermit_api::routes::SANDBOX_SURFACE_PREFIX),
            Self::Model => matches!(
                (method, path),
                ("POST", models::MESSAGES | models::COUNT_TOKENS) | ("GET", models::MODELS)
            ),
            Self::Tools => path == models::TOOLS_MCP,
        }
    }

    /// The upstream base in-cluster (`model`'s is the implementation's own).
    #[must_use]
    pub fn upstream(self) -> Option<&'static str> {
        match self {
            Self::Magpie => Some("http://magpie"),
            Self::Hermit => Some("http://hermit"),
            Self::Tools => Some("http://tools"),
            Self::Model => None,
        }
    }

    /// The per-request body limit.
    #[must_use]
    pub fn body_limit(self) -> u64 {
        match self {
            Self::Tools => limits::TOOLS_BODY_BYTES,
            _ => limits::FIXED_BODY_BYTES,
        }
    }
}

fn clean_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") || path.contains('\\') {
        return false;
    }
    path[1..].split('/').all(|seg| seg != "." && seg != "..") && !path.contains("//")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(FixedName::from_host("magpie"), Some(FixedName::Magpie));
        assert_eq!(FixedName::from_host("HERMIT:80"), Some(FixedName::Hermit));
        assert_eq!(FixedName::from_host("model:8080"), None);
        assert_eq!(FixedName::from_host("tools.example.org"), None);
        assert_eq!(FixedName::from_host("magpie.svc"), None);
    }

    #[test]
    fn routes() {
        use FixedName::*;
        assert!(Magpie.allows("PUT", "/v1/sandbox/blobs/ab?x=1"));
        assert!(!Magpie.allows("GET", "/v1/conversations"));
        assert!(Hermit.allows("GET", "/v1/sandbox/self"));
        assert!(!Hermit.allows("GET", "/v1/people"));
        assert!(!Hermit.allows("GET", "/v1/sandbox/../people"));
        assert!(!Hermit.allows("GET", "/v1/sandbox/%2e%2e/people"));
        assert!(!Hermit.allows("GET", "/v1/sandbox/x%2Fy"));
        assert!(!Hermit.allows("GET", "/v1/sandbox//self"));
        assert!(Model.allows("POST", "/v1/messages"));
        assert!(Model.allows("POST", "/v1/messages/count_tokens"));
        assert!(Model.allows("GET", "/v1/aviary/models"));
        assert!(!Model.allows("GET", "/v1/messages"));
        assert!(!Model.allows("GET", "/v1/models"));
        assert!(Tools.allows("POST", "/mcp"));
        assert!(!Tools.allows("POST", "/mcp/x"));
        assert_eq!(Tools.body_limit(), 2_000_000);
        assert_eq!(Magpie.body_limit(), 64 << 20);
    }
}
