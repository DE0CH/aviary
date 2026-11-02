//! hermit 3.2 in code: the route-to-caller table and the router agree in both directions, `guarded_router`
//! enforces the table, the identity headers and the protocol number, the admin exec route takes supervisor 15.4's
//! body, and no `Debug` form writes a credential.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::unused_async_trait_impl,
    clippy::many_single_char_names
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use aviary_core::auth::{Caller, Callers};
use axum::http::{HeaderMap, HeaderValue, Method};
use hermit_api::server::{Answer, Ctx, RouteGuard, Server, guarded_router, surface_of};
use hermit_api::{Client, routes};
use serde_json::{Value, json};

/// A server that implements nothing.
struct Nothing;
impl Server for Nothing {}

/// A server that implements only the echo page.
struct Echo;

impl Server for Echo {
    async fn ops_page(&self, ctx: Ctx) -> Answer<String> {
        let mut aviary: Vec<String> = ctx
            .headers
            .keys()
            .filter(|n| n.as_str().starts_with("aviary-"))
            .map(|n| n.as_str().to_owned())
            .collect();
        aviary.sort();
        Ok(format!("{:?} {}", ctx.caller, aviary.join(",")))
    }
}

fn token(c: Caller) -> String {
    format!("{}-pair-token-000000000000000000000000000000", c.as_str())
}

fn callers() -> Callers {
    Callers::new(Caller::ALL.iter().map(|c| (*c, vec![token(*c)]))).unwrap()
}

/// Every route constant of `routes.rs` (`pub const NAME: &str = "/…";`, prefixes left out).
fn route_constants() -> Vec<String> {
    let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes.rs")).unwrap();
    src.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("pub const ")?;
            let (name, value) = rest.split_once(": &str = \"")?;
            (!name.ends_with("_PREFIX")).then(|| value.trim_end_matches("\";").to_owned())
        })
        .collect()
}

fn concrete(route: &str) -> String {
    let mut out = String::new();
    let mut rest = route;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = open + rest[open..].find('}').unwrap();
        out.push_str(match &rest[open + 1..close] {
            "email" => "staff%40example.org",
            "name" => "host-1",
            "key" => "EXAMPLE_TOKEN",
            _ => "0190f6c2-4a0d-7b2c-8e1f-0a1b2c3d4e5f",
        });
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

async fn serve(router: axum::Router) -> String {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

const METHODS: [&str; 5] = ["GET", "PUT", "POST", "PATCH", "DELETE"];

async fn send(http: &reqwest::Client, base: &str, method: &str, route: &str, caller: Option<Caller>) -> (u16, String) {
    let surface = surface_of(route);
    let mut rb = http
        .request(method.parse().unwrap(), format!("{base}{}", concrete(route)))
        .header("aviary-protocol", format!("{}={}", surface.0, surface.1));
    if let Some(c) = caller {
        rb = rb.bearer_auth(token(c));
    }
    if method != "GET" && method != "DELETE" {
        rb = rb.header("content-type", "application/json").body("{}");
    }
    let res = rb.send().await.unwrap();
    let status = res.status().as_u16();
    let code = res
        .json::<Value>()
        .await
        .ok()
        .and_then(|v| v["code"].as_str().map(str::to_owned))
        .unwrap_or_default();
    (status, code)
}

/// The handler ran (the default 501, or a 400 from decoding the empty body), rather than a routing or guard answer.
fn reached((status, code): &(u16, String)) -> bool {
    !matches!(status, 401 | 403 | 404 | 405) && code != "unsupported_protocol"
}

#[tokio::test]
async fn the_rules_and_the_router_name_the_same_routes() {
    // every (method, route) in RULES once
    let mut rules = BTreeSet::new();
    for r in routes::RULES {
        assert!(
            rules.insert((r.method, r.route)),
            "{} {} twice in RULES",
            r.method,
            r.route
        );
        assert!(!r.callers.is_empty(), "{} {}", r.method, r.route);
    }
    let constants = route_constants();
    assert_eq!(constants.len(), 33, "{constants:?}");
    for (_, route) in &rules {
        assert!(constants.iter().any(|c| c == route), "{route} is not a route constant");
    }

    // unguarded: a (method, route constant) reaches a handler exactly when RULES lists it, so every registered
    // route has a rule and every rule has a route
    let base = serve(hermit_api::router(Arc::new(Nothing))).await;
    let http = reqwest::Client::new();
    for route in &constants {
        for method in METHODS {
            let got = send(&http, &base, method, route, None).await;
            let listed = rules.contains(&(method, route.as_str()));
            assert_eq!(reached(&got), listed, "{method} {route}: {got:?}, in RULES: {listed}");
        }
    }
}

#[tokio::test]
async fn the_guard_enforces_the_table() {
    let base = serve(guarded_router(Arc::new(Nothing), RouteGuard::new(callers()))).await;
    let http = reqwest::Client::new();
    for rule in routes::RULES {
        for caller in Caller::ALL {
            let got = send(&http, &base, rule.method, rule.route, Some(caller)).await;
            if rule.callers.contains(&caller) {
                assert!(reached(&got), "{caller} {} {}: {got:?}", rule.method, rule.route);
            } else {
                assert_eq!(
                    got,
                    (403, "wrong_caller".into()),
                    "{caller} {} {}",
                    rule.method,
                    rule.route
                );
            }
        }
        let got = send(&http, &base, rule.method, rule.route, None).await;
        assert_eq!(got, (401, "unauthenticated".into()), "{} {}", rule.method, rule.route);
    }
    // HEAD only on /ops/*
    let head = |path: &str, caller: Caller| {
        http.head(format!("{base}{path}"))
            .bearer_auth(token(caller))
            .header("aviary-protocol", "hermit-core=1")
            .send()
    };
    assert_eq!(head(routes::OPS_SNAPSHOT, Caller::Monitor).await.unwrap().status(), 501);
    assert_eq!(head(routes::DEPLOYMENT, Caller::Pigeon).await.unwrap().status(), 405);
    // unknown paths stay 404 without a token
    let res = http.get(format!("{base}/v1/nothing")).send().await.unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn the_guard_checks_the_protocol_and_strips_foreign_headers() {
    let base = serve(guarded_router(Arc::new(Echo), RouteGuard::new(callers()))).await;
    let http = reqwest::Client::new();
    let ops = |caller: Caller, protocol: Option<&str>| {
        let mut rb = http
            .get(format!("{base}{}", routes::OPS_PAGE))
            .bearer_auth(token(caller))
            .header("aviary-person", "staff@example.org")
            .header("aviary-admin", "spoof@example.org")
            .header("aviary-sandbox-id", "spoof")
            .header("aviary-request", "0190f6c2-0000-7000-8000-00000000000a");
        if let Some(p) = protocol {
            rb = rb.header("aviary-protocol", p);
        }
        rb.send()
    };
    let res = ops(Caller::Pigeon, Some("hermit-core=1")).await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.text().await.unwrap(),
        "Some(Pigeon) aviary-person,aviary-protocol,aviary-request"
    );
    let res = ops(Caller::Admin, Some("hermit-core=1")).await.unwrap();
    assert_eq!(
        res.text().await.unwrap(),
        "Some(Admin) aviary-admin,aviary-protocol,aviary-request"
    );
    for bad in [
        None,
        Some("hermit-core=2"),
        Some("hermit-core=0"),
        Some("hermit-sandbox=1"),
        Some("x"),
    ] {
        let res = ops(Caller::Pigeon, bad).await.unwrap();
        assert_eq!(res.status(), 400, "{bad:?}");
        let v: Value = res.json().await.unwrap();
        assert_eq!(v["code"], "unsupported_protocol", "{bad:?}");
        assert_eq!(v["surface"], "hermit-core");
    }
    // the sandbox surface needs its own number
    let res = http
        .get(format!("{base}{}", routes::SANDBOX_SELF))
        .bearer_auth(token(Caller::Gateway))
        .header("aviary-protocol", "hermit-core=1")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    // the typed client passes the guard with its own headers
    let c = Client::new(reqwest::Client::new(), base)
        .bearer(token(Caller::Pigeon))
        .as_person("staff@example.org");
    assert_eq!(
        c.ops_page().await.unwrap(),
        "Some(Pigeon) aviary-person,aviary-protocol"
    );
}

#[test]
fn admit_without_http() {
    let g = RouteGuard::new(callers());
    let mut h = HeaderMap::new();
    h.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", token(Caller::Magpie))).unwrap(),
    );
    h.insert("aviary-protocol", HeaderValue::from_static("hermit-core=1"));
    h.insert("aviary-person", HeaderValue::from_static("spoof@example.org"));
    assert_eq!(g.admit(&Method::PUT, routes::SANDBOX, &mut h).unwrap(), Caller::Magpie);
    assert!(!h.contains_key("aviary-person"));
    assert_eq!(
        g.admit(&Method::POST, routes::WARM, &mut h).unwrap_err().code(),
        "wrong_caller"
    );
}

#[tokio::test]
async fn the_exec_route_takes_a_full_size_stdin() {
    let base = serve(hermit_api::router(Arc::new(Nothing))).await;
    let http = reqwest::Client::new();
    let url = format!(
        "{base}{}",
        routes::fill(routes::ADMIN_SANDBOX_EXEC, &["0190f6c2-4a0d-7b2c-8e1f-0a1b2c3d4e5f"])
    );
    // 1 MiB of stdin is 1 398 104 characters of base64: over hermit's 1 MiB JSON limit, inside the exec route's
    let stdin = "A".repeat(hermit_api::exec::EXEC_MAX_STDIN.div_ceil(3) * 4);
    let body = serde_json::to_vec(&json!({"argv": ["cat"], "stdin": stdin})).unwrap();
    assert!(body.len() > hermit_api::server::MAX_JSON_BODY);
    let res = http
        .post(&url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 501, "the body parsed and reached the handler");
    let res = http
        .post(&url)
        .header("content-type", "application/json")
        .body(vec![b' '; hermit_api::server::MAX_EXEC_BODY + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 413);
    // other routes keep 1 MiB
    let res = http
        .post(format!("{base}{}", routes::ADMIN_RECYCLE))
        .header("content-type", "application/json")
        .body(vec![b' '; hermit_api::server::MAX_JSON_BODY + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 413);
}

#[test]
fn debug_forms_never_show_credentials() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pair = "pair-token-secret-0000000000000000000000000";
    let c = Client::new(reqwest::Client::new(), "http://hermit")
        .bearer(pair)
        .as_person("staff@example.org")
        .request_id("0190f6c2-0000-7000-8000-00000000000a");
    let d = format!("{c:?}");
    assert!(!d.contains(pair) && !d.contains("staff@example.org"), "{d}");
    assert!(
        d.contains("http://hermit") && d.contains("0190f6c2-0000-7000-8000-00000000000a"),
        "{d}"
    );

    let mut h = HeaderMap::new();
    h.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {pair}")).unwrap(),
    );
    h.insert(
        "proxy-authorization",
        HeaderValue::from_static("Basic c2J4OmhzdDEuMS5BQS5CQg"),
    );
    h.insert("aviary-sandbox-person", HeaderValue::from_static("staff@example.org"));
    h.insert("aviary-protocol", HeaderValue::from_static("hermit-sandbox=1"));
    h.insert("accept", HeaderValue::from_static("application/json"));
    let ctx = Ctx::new(Method::GET, routes::SANDBOX_SELF, h).with_caller(Some(Caller::Gateway));
    let d = format!("{ctx:?}");
    for secret in [pair, "c2J4OmhzdDEuMS5BQS5CQg", "staff@example.org"] {
        assert!(!d.contains(secret), "{secret} in {d}");
    }
    assert!(
        d.contains("hermit-sandbox=1") && d.contains("application/json") && d.contains("Gateway"),
        "{d}"
    );

    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sandbox-self.json");
    let s: hermit_api::sandbox_surface::SandboxSelf =
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    assert!(s.env.values().any(|v| v.contains("hst1.")));
    let d = format!("{s:?}");
    assert!(!d.contains("hst1."), "{d}");
    assert!(d.contains("PIP_INDEX_URL"), "{d}");
}
