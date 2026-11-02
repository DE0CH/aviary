//! Caller credentials (platform 3.7): which service is calling, from its pair token.
//!
//! The callee reads `<CALLEE>_ACCEPT_<CALLER>` for every caller it knows: a comma-separated list, the current token
//! first, two during a rotation. It keeps only the SHA-256 digests and compares the digest of a presented token with
//! every listed digest in constant time (`subtle`: ring's `constant_time::verify_slices_are_equal`, which platform 3.7
//! names, is deprecated in ring 0.17 as internal with "no promises regarding side channels"). The route-to-caller table is code: each router declares its [`Guard`].
//! A missing or unknown token is 401 `unauthenticated` (with `www-authenticate: Bearer`); a valid token on a route its
//! caller may not use is 403 `wrong_caller`. After the guard admits a caller it removes every `Aviary-*` header that
//! caller may not set (platform 3.5), so a handler can trust what is left.
//!
//! The caller side reads `<CALLER>_TOKEN_<CALLEE>` with [`outbound_token`].

use std::fmt;
use std::sync::Arc;

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use ring::digest;
use subtle::ConstantTimeEq as _;

use crate::env;
use crate::http::{ApiError, headers};

/// A service-to-service caller (platform 3.7's pairs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Caller {
    /// pigeon, the app front end.
    Pigeon,
    /// magpie, the store of record.
    Magpie,
    /// hermit, the sandbox lifecycle service.
    Hermit,
    /// the sandbox gateway, calling on behalf of a verified sandbox.
    Gateway,
    /// the MCP tools service.
    Tools,
    /// the external monitor reading hermit's ops snapshot.
    Monitor,
    /// an operator's tooling (runbooks, Claude).
    Admin,
}

impl Caller {
    /// Every caller.
    pub const ALL: [Self; 7] = [
        Self::Pigeon,
        Self::Magpie,
        Self::Hermit,
        Self::Gateway,
        Self::Tools,
        Self::Monitor,
        Self::Admin,
    ];

    /// The caller's name in env names: `PIGEON`, `GATEWAY`, ….
    #[must_use]
    pub fn env_name(self) -> &'static str {
        match self {
            Self::Pigeon => "PIGEON",
            Self::Magpie => "MAGPIE",
            Self::Hermit => "HERMIT",
            Self::Gateway => "GATEWAY",
            Self::Tools => "TOOLS",
            Self::Monitor => "MONITOR",
            Self::Admin => "ADMIN",
        }
    }

    /// The caller's name in logs: `pigeon`, `gateway`, ….
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pigeon => "pigeon",
            Self::Magpie => "magpie",
            Self::Hermit => "hermit",
            Self::Gateway => "gateway",
            Self::Tools => "tools",
            Self::Monitor => "monitor",
            Self::Admin => "admin",
        }
    }
}

impl fmt::Display for Caller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A caller list that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallersError {
    /// A caller with no tokens.
    NoTokens(Caller),
    /// A caller with an empty token.
    EmptyToken(Caller),
    /// The same caller twice.
    Duplicate(Caller),
    /// One token configured for two callers (a misconfigured Secret): it would identify only the first, so the two
    /// could not be told apart.
    SharedToken(Caller, Caller),
}

impl fmt::Display for CallersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTokens(c) => write!(f, "caller {c} has no tokens"),
            Self::EmptyToken(c) => write!(f, "caller {c} has an empty token"),
            Self::Duplicate(c) => write!(f, "caller {c} listed twice"),
            Self::SharedToken(a, b) => write!(f, "callers {a} and {b} share a token"),
        }
    }
}

impl std::error::Error for CallersError {}

type Digest = [u8; 32];

fn sha256(token: &str) -> Digest {
    let d = digest::digest(&digest::SHA256, token.as_bytes());
    let mut out = [0; 32];
    out.copy_from_slice(d.as_ref());
    out
}

/// The callers a service accepts, as token digests.
#[derive(Clone)]
pub struct Callers {
    accepted: Arc<[(Caller, Digest)]>,
}

impl fmt::Debug for Callers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&str> = self.accepted.iter().map(|(c, _)| c.as_str()).collect();
        names.dedup();
        f.debug_struct("Callers").field("callers", &names).finish()
    }
}

impl Callers {
    /// The callers of service `callee` (`MAGPIE`, `HERMIT`, …), each from `<CALLEE>_ACCEPT_<CALLER>`. Aborts start
    /// when any of them is missing or empty ([`env::required_list`]), or when two callers are given the same token (a
    /// misconfigured Secret), with the same JSON line and exit code ([`env::EXIT_CONFIG`], `msg` `invalid env`).
    ///
    /// # Panics
    /// When `callers` names a caller twice: a programming error, which each service's tests catch.
    #[must_use]
    pub fn from_env(callee: &str, callers: &[Caller]) -> Self {
        let var = |c: Caller| format!("{callee}_ACCEPT_{}", c.env_name());
        let lists = callers.iter().map(|c| (*c, env::required_list(&var(*c))));
        // required_list already refused empty lists and items
        match Self::new(lists) {
            Ok(callers) => callers,
            Err(CallersError::SharedToken(a, b)) => {
                env::abort_start("invalid env", &var(b), &format!("shares a token with {}", var(a)))
            }
            Err(e) => panic!("{callee} callers: {e}"),
        }
    }

    /// Callers from explicit token lists.
    ///
    /// # Errors
    /// When a caller has no token or an empty one, is listed twice, or shares a token with another caller.
    pub fn new<I, T>(lists: I) -> Result<Self, CallersError>
    where
        I: IntoIterator<Item = (Caller, T)>,
        T: IntoIterator,
        T::Item: AsRef<str>,
    {
        let mut accepted = Vec::new();
        let mut seen = Vec::new();
        for (caller, tokens) in lists {
            if seen.contains(&caller) {
                return Err(CallersError::Duplicate(caller));
            }
            seen.push(caller);
            let before = accepted.len();
            for t in tokens {
                let t = t.as_ref().trim();
                if t.is_empty() {
                    return Err(CallersError::EmptyToken(caller));
                }
                let d = sha256(t);
                if let Some((other, _)) = accepted.iter().find(|(c, o)| *c != caller && *o == d) {
                    return Err(CallersError::SharedToken(*other, caller));
                }
                accepted.push((caller, d));
            }
            if accepted.len() == before {
                return Err(CallersError::NoTokens(caller));
            }
        }
        Ok(Self {
            accepted: accepted.into(),
        })
    }

    /// The caller a token belongs to. Compares against every accepted digest, in constant time each, without
    /// stopping at a match.
    #[must_use]
    pub fn identify(&self, token: &str) -> Option<Caller> {
        let presented = sha256(token);
        let mut found = None;
        for (caller, d) in self.accepted.iter() {
            let equal: bool = presented[..].ct_eq(&d[..]).into();
            if equal && found.is_none() {
                found = Some(*caller);
            }
        }
        found
    }

    /// The caller behind an `Authorization: Bearer <token>` header map.
    #[must_use]
    pub fn identify_headers(&self, headers: &HeaderMap) -> Option<Caller> {
        bearer(headers).and_then(|t| self.identify(t))
    }

    /// A guard admitting none yet; add callers with [`Guard::allow`].
    #[must_use]
    pub fn guard(&self) -> Guard {
        Guard {
            callers: self.clone(),
            rules: Arc::from(Vec::new()),
        }
    }
}

/// The token of an `Authorization: Bearer <token>` header (scheme in any case).
#[must_use]
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.trim().split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// The token service `service` (`MAGPIE`) presents to `target` (`HERMIT`): `<SERVICE>_TOKEN_<TARGET>`, required.
#[must_use]
pub fn outbound_token(service: &str, target: &str) -> String {
    env::required(&format!("{service}_TOKEN_{target}"))
}

#[derive(Debug, Clone)]
struct Rule {
    caller: Caller,
    headers: Vec<HeaderName>,
}

/// The callers one router admits, and which `Aviary-*` headers each may set.
///
/// **A guard covers only the routes that exist when it is applied.** It is an axum route layer: a route added to
/// the router after [`Guard::apply`] is not guarded, and nothing denies by default (a handler that does not extract
/// [`Caller`] answers anyone). So build guarded routers with [`Guard::router`], which takes every route at once and
/// returns a router to merge or nest, and add nothing to it afterwards; [`Guard::apply`] is for a router that is
/// already finished. Each service's tests call every `/v1` route without a token and expect 401. Unmatched paths
/// still answer 404 (not 401).
#[derive(Debug, Clone)]
pub struct Guard {
    callers: Callers,
    rules: Arc<[Rule]>,
}

impl Guard {
    /// Admits `caller`, which may set `headers` (beyond `Aviary-Request` and `Aviary-Protocol`, which every caller
    /// sends); its other `Aviary-*` headers are removed.
    #[must_use]
    pub fn allow(self, caller: Caller, headers: &[HeaderName]) -> Self {
        let mut rules: Vec<Rule> = self.rules.iter().filter(|r| r.caller != caller).cloned().collect();
        rules.push(Rule {
            caller,
            headers: headers.to_vec(),
        });
        Self {
            callers: self.callers,
            rules: rules.into(),
        }
    }

    /// A router of exactly `routes` (path, method router), every one guarded; an empty router when `routes` is
    /// empty. Merge or nest the result; never add a route to it afterwards (see [`Guard`]).
    pub fn router<S, P>(self, routes: impl IntoIterator<Item = (P, MethodRouter<S>)>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
        P: AsRef<str>,
    {
        let all = routes
            .into_iter()
            .fold(Router::new(), |acc, (path, method)| acc.route(path.as_ref(), method));
        self.apply(all)
    }

    /// The finished `router` with this guard on every route it has now. A route added to the result later is not
    /// guarded (see [`Guard`]); prefer [`Guard::router`].
    /// A router with no routes comes back as it is (axum refuses a route layer on one; there is nothing to guard).
    pub fn apply<S: Clone + Send + Sync + 'static>(self, router: Router<S>) -> Router<S> {
        if !router.has_routes() {
            return router;
        }
        router.route_layer(axum::middleware::from_fn_with_state(self, check))
    }

    /// The decision for a request's headers: the admitted caller (with the headers it may not set removed), or the
    /// refusal.
    ///
    /// # Errors
    /// 401 `unauthenticated` without a known token; 403 `wrong_caller` for a caller this guard does not admit.
    pub fn admit(&self, headers: &mut HeaderMap) -> Result<Caller, ApiError> {
        let caller = self
            .callers
            .identify_headers(headers)
            .ok_or_else(ApiError::unauthenticated)?;
        let rule = self
            .rules
            .iter()
            .find(|r| r.caller == caller)
            .ok_or_else(|| ApiError::wrong_caller(caller))?;
        headers::strip_aviary(headers, &rule.headers);
        Ok(caller)
    }
}

async fn check(State(guard): State<Guard>, mut req: Request, next: Next) -> Response {
    match guard.admit(req.headers_mut()) {
        Ok(caller) => {
            req.extensions_mut().insert(caller);
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<Self>()
                .copied()
                .ok_or_else(ApiError::unauthenticated),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::headers::{AVIARY_ADMIN, AVIARY_PERSON, AVIARY_REQUEST, AVIARY_VIEWER};
    use axum::routing::get;

    const PIGEON_A: &str = "pigeon-current-0000000000000000000000000000";
    const PIGEON_B: &str = "pigeon-next-111111111111111111111111111111";
    const ADMIN: &str = "admin-2222222222222222222222222222222222222";
    const GATEWAY: &str = "gateway-33333333333333333333333333333333333";

    fn callers() -> Callers {
        Callers::new([
            (Caller::Pigeon, vec![PIGEON_A, PIGEON_B]),
            (Caller::Admin, vec![ADMIN]),
            (Caller::Gateway, vec![GATEWAY]),
        ])
        .unwrap()
    }

    #[test]
    fn identify_with_rotation_lists() {
        let c = callers();
        assert_eq!(c.identify(PIGEON_A), Some(Caller::Pigeon));
        assert_eq!(c.identify(PIGEON_B), Some(Caller::Pigeon));
        assert_eq!(c.identify(ADMIN), Some(Caller::Admin));
        assert_eq!(c.identify("pigeon-current-000000000000000000000000000"), None);
        assert_eq!(c.identify(""), None);
    }

    #[test]
    fn bad_lists_are_refused() {
        assert_eq!(
            Callers::new([(Caller::Pigeon, Vec::<&str>::new())]).unwrap_err(),
            CallersError::NoTokens(Caller::Pigeon)
        );
        assert_eq!(
            Callers::new([(Caller::Pigeon, vec![" "])]).unwrap_err(),
            CallersError::EmptyToken(Caller::Pigeon)
        );
        assert_eq!(
            Callers::new([(Caller::Pigeon, vec!["a"]), (Caller::Pigeon, vec!["b"])]).unwrap_err(),
            CallersError::Duplicate(Caller::Pigeon)
        );
    }

    #[test]
    fn a_token_shared_by_two_callers_is_refused() {
        let e = Callers::new([
            (Caller::Pigeon, vec!["t1", "shared"]),
            (Caller::Admin, vec![" shared "]),
        ])
        .unwrap_err();
        assert_eq!(e, CallersError::SharedToken(Caller::Pigeon, Caller::Admin));
        assert_eq!(e.to_string(), "callers pigeon and admin share a token");
        // one caller listing a token twice (a rotation to the same value) is fine
        let c = Callers::new([(Caller::Pigeon, vec!["t1", "t1"]), (Caller::Admin, vec!["t2"])]).unwrap();
        assert_eq!(c.identify("t1"), Some(Caller::Pigeon));
    }

    #[test]
    fn debug_never_shows_digests() {
        let s = format!("{:?}", callers());
        assert_eq!(s, r#"Callers { callers: ["pigeon", "admin", "gateway"] }"#);
    }

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(header::AUTHORIZATION, "bearer  abc ".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer ".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }

    async fn echo(caller: Caller, headers: HeaderMap) -> String {
        let mut aviary: Vec<String> = headers
            .iter()
            .filter(|(n, _)| n.as_str().starts_with("aviary-"))
            .map(|(n, v)| format!("{n}={}", v.to_str().unwrap()))
            .collect();
        aviary.sort();
        format!("{caller} {}", aviary.join(" "))
    }

    async fn serve() -> String {
        let guard = callers()
            .guard()
            .allow(Caller::Pigeon, &[AVIARY_PERSON, AVIARY_VIEWER])
            .allow(Caller::Admin, &[AVIARY_ADMIN]);
        let router = guard.router([("/v1/x", get(echo)), ("/v1/y", get(echo))]);
        let router = crate::http::standard(router, 1 << 20);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn guard_admits_refuses_and_strips() {
        let base = serve().await;
        let c = reqwest::Client::builder()
            .tls_backend_preconfigured(crate::http::tls_config())
            .build()
            .unwrap();
        let url = format!("{base}/v1/x");

        // no token
        let r = c.get(&url).send().await.unwrap();
        assert_eq!(r.status(), 401);
        assert_eq!(r.headers()["www-authenticate"], "Bearer");
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["code"], "unauthenticated");

        // unknown token
        let r = c.get(&url).bearer_auth("nope").send().await.unwrap();
        assert_eq!(r.status(), 401);

        // a valid token of a caller this router does not admit
        let r = c.get(&url).bearer_auth(GATEWAY).send().await.unwrap();
        assert_eq!(r.status(), 403);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["code"], "wrong_caller");
        assert_eq!(body["caller"], "gateway");

        // pigeon, either rotation token; keeps its own headers, loses the admin's and the gateway's
        for token in [PIGEON_A, PIGEON_B] {
            let r = c
                .get(&url)
                .bearer_auth(token)
                .header("aviary-person", "a@b.c")
                .header("aviary-viewer", "v@b.c")
                .header("aviary-admin", "spoof@b.c")
                .header("aviary-sandbox-id", "spoof")
                .header("aviary-protocol", "magpie-person=1")
                .header(AVIARY_REQUEST, "0190f3b2-7c1a-7d2e-9f00-0123456789ab")
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            assert_eq!(
                r.text().await.unwrap(),
                "pigeon aviary-person=a@b.c aviary-protocol=magpie-person=1 \
                 aviary-request=0190f3b2-7c1a-7d2e-9f00-0123456789ab aviary-viewer=v@b.c"
            );
        }

        // the admin keeps Aviary-Admin, loses Aviary-Person
        let r = c
            .get(&url)
            .bearer_auth(ADMIN)
            .header("aviary-admin", "op@b.c")
            .header("aviary-person", "spoof@b.c")
            .send()
            .await
            .unwrap();
        let text = r.text().await.unwrap();
        assert!(text.starts_with("admin aviary-admin=op@b.c aviary-request="), "{text}");
        assert!(!text.contains("aviary-person"), "{text}");

        // every route of the guarded router refuses an unknown caller
        let r = c.get(format!("{base}/v1/y")).send().await.unwrap();
        assert_eq!(r.status(), 401);

        // unmatched paths are 404, not 401 (the guard is a route layer)
        let r = c.get(format!("{base}/nope")).send().await.unwrap();
        assert_eq!(r.status(), 404);
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["code"], "not_found");
    }

    #[test]
    fn a_guard_with_no_routes_gives_an_empty_router() {
        // axum panics on a route layer over a router with no routes; a caller with no routes yet must not
        let routes: Vec<(&str, MethodRouter)> = Vec::new();
        let r: Router = callers().guard().allow(Caller::Pigeon, &[]).router(routes);
        assert!(!r.has_routes());
        let r: Router = callers().guard().apply(Router::new());
        assert!(!r.has_routes());
    }
}
