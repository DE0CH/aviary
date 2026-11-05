//! pigeon spec 18.2: the HTTP tests ported from the old router's `router.test.mjs` (A20.F0), against a real pigeon
//! in front of fake-hermit, fake-magpie, a model stub and two fake Access teams.
#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::many_single_char_names)] // tests

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use aviary_fakes::{FaultKind, Reply};
use common::*;
use pigeon::paseo::compat;
use serde_json::json;

fn me() -> &'static str {
    compat::ME
}

// ---- Access (spec 4.2-4.3) ----

#[tokio::test]
async fn access_tokens_are_verified() {
    let h = Harness::start().await;
    let t1 = &h.teams[0];
    // missing
    assert_eq!(
        code(h.get(me()).send().await.unwrap()).await,
        (401, "NOT_SIGNED_IN".into())
    );
    // good, by assertion header, by cookie, by the iOS header
    assert_eq!(h.get_as(me(), STAFF).await.status(), 200);
    let r = h
        .get(me())
        .header("cookie", format!("CF_Authorization={}", t1.token(STAFF)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let r = h
        .get(me())
        .header("cf-access-token", t1.token(STAFF))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "the cf-access-token header works on its own");
    // wrong audience
    let wrong_aud = t1
        .issuer
        .mint(&fake_access::MintRequest {
            email: STAFF.into(),
            name: None,
            aud: "another-app".into(),
            exp_seconds: 3600,
            nbf_seconds: None,
            kid: None,
        })
        .unwrap();
    assert_eq!(
        code(h.as_token(h.get(me()), &wrong_aud).send().await.unwrap()).await.0,
        401
    );
    // expired, and not yet valid (beyond the 5 s leeway)
    let expired = t1.mint(STAFF, -60, None, None);
    assert_eq!(h.as_token(h.get(me()), &expired).send().await.unwrap().status(), 401);
    let early = t1.mint(STAFF, 3600, Some(120), None);
    assert_eq!(h.as_token(h.get(me()), &early).send().await.unwrap().status(), 401);
    // within the leeway
    let skewed = t1.mint(STAFF, -2, None, None);
    assert_eq!(h.as_token(h.get(me()), &skewed).send().await.unwrap().status(), 200);
    // wrong key: a valid header and claims with another token's signature
    let other = t1.token(STAFF2);
    let good = t1.token(STAFF);
    let (gh, _) = good.rsplit_once('.').unwrap();
    let (_, osig) = other.rsplit_once('.').unwrap();
    let forged = format!("{gh}.{osig}");
    assert_eq!(h.as_token(h.get(me()), &forged).send().await.unwrap().status(), 401);
    // a service token: no email
    let service = sign_jwt(&t1.pems[0], &{
        let mut c = t1.claims(None);
        c["common_name"] = json!("svc.example");
        c
    });
    assert_eq!(h.as_token(h.get(me()), &service).send().await.unwrap().status(), 401);
    // an issuer the deployment does not name
    let mut foreign = t1.claims(Some(STAFF));
    foreign["iss"] = json!("https://three.example.org");
    let foreign = sign_jwt(&t1.pems[0], &foreign);
    assert_eq!(h.as_token(h.get(me()), &foreign).send().await.unwrap().status(), 401);
    // not RS256
    let none = format!(
        "{}.{}.",
        b64(r#"{"alg":"none","kid":"t1-a"}"#),
        b64(&t1.claims(Some(STAFF)).to_string())
    );
    assert_eq!(h.as_token(h.get(me()), &none).send().await.unwrap().status(), 401);
    // garbage
    assert_eq!(h.as_token(h.get(me()), "a.b.c").send().await.unwrap().status(), 401);
}

fn b64(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

#[tokio::test]
async fn two_teams_each_verified_against_their_own_keys() {
    let h = Harness::start().await;
    let (t1, t2) = (&h.teams[0], &h.teams[1]);
    assert_eq!(
        h.as_token(h.get(me()), &t2.token(STAFF)).send().await.unwrap().status(),
        200
    );
    // team two's key cannot sign for team one
    let cross = sign_jwt(&t2.pems[0], &t1.claims(Some(STAFF)));
    assert_eq!(h.as_token(h.get(me()), &cross).send().await.unwrap().status(), 401);
    // nor team one's audience pass for team two
    let mut c = t2.claims(Some(STAFF));
    c["aud"] = json!([AUD1]);
    let wrong = sign_jwt(&t2.pems[0], &c);
    assert_eq!(h.as_token(h.get(me()), &wrong).send().await.unwrap().status(), 401);
}

#[tokio::test]
async fn unknown_kid_refetches_with_a_cooldown_and_rotation_works() {
    let h = Harness::start().await;
    let t1 = &h.teams[0];
    let before = t1.jwks_hits.load(Ordering::SeqCst);
    // the second key is loaded but not published: its kid is unknown
    let unpublished = t1.mint(STAFF, 3600, None, Some("t1-b"));
    assert_eq!(
        h.as_token(h.get(me()), &unpublished).send().await.unwrap().status(),
        401
    );
    let after_first = t1.jwks_hits.load(Ordering::SeqCst);
    assert_eq!(after_first, before + 1, "an unknown kid refetches once");
    for _ in 0..5 {
        let t = t1.mint(STAFF2, 3600, None, Some("t1-b"));
        assert_eq!(h.as_token(h.get(me()), &t).send().await.unwrap().status(), 401);
    }
    assert_eq!(
        t1.jwks_hits.load(Ordering::SeqCst),
        after_first,
        "no refetch inside the cooldown"
    );
    // rotation: the new key is published; after the cooldown its first token refetches and verifies
    t1.issuer.set_published("t1-b", true).unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let rotated = t1.mint(ADMIN, 3600, None, Some("t1-b"));
    assert_eq!(h.as_token(h.get(me()), &rotated).send().await.unwrap().status(), 200);
    // the old key retired: its tokens keep working while the cached set holds it (cache 1 h), new kid still fine
    t1.issuer.set_published("t1-a", false).unwrap();
    assert_eq!(h.get_as(me(), STAFF).await.status(), 200);
}

#[tokio::test]
async fn a_stale_key_set_is_used_while_one_refresh_runs() {
    let h = Harness::start_with(pigeon::Tuning {
        jwks_ttl: Duration::from_millis(300),
        token_cache: Duration::from_millis(1),
        ..tuning()
    })
    .await;
    let t1 = &h.teams[0];
    tokio::time::sleep(Duration::from_millis(400)).await;
    let before = t1.jwks_hits.load(Ordering::SeqCst);
    let mut all = Vec::new();
    for i in 0..10 {
        let r = h.as_token(h.get(me()), &t1.token(&format!("p{i}@example.org")));
        all.push(tokio::spawn(r.send()));
    }
    for r in all {
        assert_eq!(r.await.unwrap().unwrap().status(), 200, "the stale set still verifies");
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        t1.jwks_hits.load(Ordering::SeqCst),
        before + 1,
        "one background refresh for the burst"
    );
}

#[tokio::test]
async fn a_failing_refresh_of_a_stale_set_is_tried_once_per_cooldown() {
    let h = Harness::start_with(pigeon::Tuning {
        jwks_ttl: Duration::from_millis(300),
        token_cache: Duration::from_millis(1),
        ..tuning()
    })
    .await;
    let t1 = &h.teams[0];
    tokio::time::sleep(Duration::from_millis(400)).await;
    t1.certs_up.store(false, Ordering::SeqCst);
    let before = t1.jwks_hits.load(Ordering::SeqCst);
    for round in 0..2 {
        let mut all = Vec::new();
        for i in 0..40 {
            // valid tokens verify on the cached set; forged ones (known kid, garbage signature) are refused
            let token = if i % 2 == 0 {
                t1.token(&format!("p{round}-{i}@example.org"))
            } else {
                let mut t = t1.token(&format!("f{round}-{i}@example.org"));
                t.truncate(t.len() - 8);
                t.push_str("AAAAAAAA");
                t
            };
            all.push(tokio::spawn(h.as_token(h.get(me()), &token).send()));
        }
        for (i, r) in all.into_iter().enumerate() {
            let want = if i % 2 == 0 { 200 } else { 401 };
            assert_eq!(r.await.unwrap().unwrap().status(), want);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        t1.jwks_hits.load(Ordering::SeqCst),
        before + 1,
        "one attempt inside the cooldown, however many requests miss the fresh set"
    );
    // after the cooldown one more attempt; once the certs answer the set is fresh again
    tokio::time::sleep(Duration::from_millis(600)).await;
    t1.certs_up.store(true, Ordering::SeqCst);
    assert_eq!(h.get_as(me(), STAFF).await.status(), 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(t1.jwks_hits.load(Ordering::SeqCst), before + 2);
}

#[tokio::test]
async fn parallel_requests_with_a_new_kid_wait_for_one_fetch() {
    let h = Harness::start().await;
    let t1 = &h.teams[0];
    // rotation: the new key is published, and the certs answer slowly
    t1.issuer.set_published("t1-b", true).unwrap();
    t1.certs_delay_ms.store(300, Ordering::SeqCst);
    let before = t1.jwks_hits.load(Ordering::SeqCst);
    let token = t1.mint(STAFF, 3600, None, Some("t1-b"));
    let mut all = Vec::new();
    for path in [me(), "/api/providers/claude/models", me(), me(), me(), me(), me(), me()] {
        all.push(tokio::spawn(h.as_token(h.get(path), &token).send()));
    }
    for r in all {
        assert_eq!(
            r.await.unwrap().unwrap().status(),
            200,
            "every request sees the new key"
        );
    }
    assert_eq!(t1.jwks_hits.load(Ordering::SeqCst), before + 1, "one fetch");
}

#[tokio::test]
async fn a_team_without_keys_is_tried_once_per_cooldown_and_blocks_no_other_team() {
    let h = Harness::start_opts(Opts {
        teams_in_facts: 1,
        ..Opts::default()
    })
    .await;
    let (t1, t2) = (&h.teams[0], &h.teams[1]);
    assert_eq!(t2.jwks_hits.load(Ordering::SeqCst), 0);
    // hermit adds the second team, whose certs are down and slow: pigeon's facts refresh tries to load its keys
    t2.certs_up.store(false, Ordering::SeqCst);
    t2.certs_delay_ms.store(400, Ordering::SeqCst);
    h.set_facts(|d| {
        d.sign_in.access.push(hermit_api::deployment::AccessApp {
            team_domain: TEAM2.into(),
            audience: AUD2.into(),
        });
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        t2.jwks_hits.load(Ordering::SeqCst) >= 1,
        "loaded after the facts refresh"
    );
    // while its fetch hangs, the first team's new kid is fetched and verified at once
    t1.issuer.set_published("t1-b", true).unwrap();
    let started = std::time::Instant::now();
    let r = h
        .as_token(h.get(me()), &t1.mint(STAFF, 3600, None, Some("t1-b")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    // many tokens of the keyless team: refused, at most one attempt per cooldown
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = t2.jwks_hits.load(Ordering::SeqCst);
    let mut all = Vec::new();
    for i in 0..20 {
        let token = t2.token(&format!("q{i}@example.org"));
        all.push(tokio::spawn(h.as_token(h.get(me()), &token).send()));
    }
    for r in all {
        assert_eq!(r.await.unwrap().unwrap().status(), 401);
    }
    assert!(
        t2.jwks_hits.load(Ordering::SeqCst) <= before + 1,
        "{} attempts for 20 requests",
        t2.jwks_hits.load(Ordering::SeqCst) - before
    );
    // the certs come back: the next facts refresh loads the keys and the team signs in
    t2.certs_delay_ms.store(0, Ordering::SeqCst);
    t2.certs_up.store(true, Ordering::SeqCst);
    let started = std::time::Instant::now();
    loop {
        let r = h.as_token(h.get(me()), &t2.token(STAFF)).send().await.unwrap();
        if r.status() == 200 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the second team never loaded"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---- People (spec 4.4) ----

#[tokio::test]
async fn people_not_allowed_reread_and_stale_on_error() {
    let h = Harness::start().await;
    assert_eq!(code(h.get_as(me(), OUTSIDER).await).await, (403, "NOT_ALLOWED".into()));
    assert_eq!(h.get_as(me(), STAFF2).await.status(), 200);
    // removed from the catalogue: still allowed while the answer is fresh, refused after it
    h.hermit
        .fake
        .configure(|c| c.users = vec![ADMIN.into(), ADMIN2.into(), STAFF.into()]);
    assert_eq!(
        h.get_as(me(), STAFF2).await.status(),
        200,
        "within the 15 s (test: 400 ms) cache"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(code(h.get_as(me(), STAFF2).await).await, (403, "NOT_ALLOWED".into()));
    // hermit down: the last answer stands for 300 s (test: 1.5 s) past its expiry, then 503
    assert_eq!(h.get_as(me(), STAFF).await.status(), 200);
    let fault = h.hermit.fake.faults().on(
        hermit_api::routes::PERSON,
        FaultKind::Unavailable { retry_after_secs: 1 },
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.get_as(me(), STAFF).await.status(), 200, "stale on error");
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let r = h.get_as(me(), STAFF).await;
    assert_eq!(r.headers()["retry-after"], "5");
    assert_eq!(code(r).await, (503, "UNAVAILABLE".into()));
    // a person never seen while hermit is down: 503 at once
    assert_eq!(code(h.get_as(me(), "new@example.org").await).await.0, 503);
    h.hermit.fake.faults().remove(fault);
    assert_eq!(h.get_as(me(), STAFF).await.status(), 200);
}

// ---- Shell and files (spec 5.2) ----

#[tokio::test]
async fn files_are_public_and_cached_by_kind() {
    let h = Harness::start().await;
    let r = h.get(HASHED).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "public, max-age=31536000, immutable");
    assert_eq!(r.headers()["content-type"], "text/javascript; charset=utf-8");
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    assert_eq!(r.headers()["vary"], "accept-encoding");
    assert_eq!(r.text().await.unwrap(), "console.log('entry')");
    let r = h
        .get(HASHED)
        .header("accept-encoding", "gzip, br")
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["content-encoding"], "br");
    let etag = r.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"BROTLI");
    let r = h.get(HASHED).header("accept-encoding", "gzip").send().await.unwrap();
    assert_eq!(r.headers()["content-encoding"], "gzip");
    let r = h
        .get(HASHED)
        .header("accept-encoding", "br")
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 304);
    let r = h.get("/index.html").send().await.unwrap();
    assert_eq!(r.headers()["cache-control"], "no-store");
    let r = h.get("/sw.js").send().await.unwrap();
    assert_eq!(r.status(), 200, "the kill-switch worker is served signed out");
    assert_eq!(r.headers()["cache-control"], "no-cache");
    let r = h.get("/logo.png").send().await.unwrap();
    assert_eq!(r.status(), 200, "the logo the Access login page links to is public");
    assert_eq!(r.headers()["cache-control"], "public, max-age=300");
    let r = h
        .get("/_expo/static/js/web/missing-0123456789abcdef.js")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.text().await.unwrap(), "Not found");
    assert_eq!(h.post("/logo.png").send().await.unwrap().status(), 405);
    assert_eq!(h.post("/new").send().await.unwrap().status(), 405);
}

fn named_index() -> String {
    INDEX
        .replace("<title>App</title>", &format!("<title>{PRODUCT}</title>"))
        .replace("content=\"App\"", &format!("content=\"{PRODUCT}\""))
}

fn signed_in_index() -> String {
    named_index().replace("</body>", &format!("{}</body>", pigeon::raw::toggle_script()))
}

#[tokio::test]
async fn the_shell_answers_every_page_path_signed_in_and_out() {
    let h = Harness::start().await;
    let cookie = format!("CF_Authorization={}", h.token(STAFF));
    let wks = format!("/h/srv_conversations/workspace/wks_{}", uuid::Uuid::new_v4());
    let paths = [
        "/",
        "/new",
        "/h/srv_conversations",
        wks.as_str(),
        "/s/AAAAAAAAAAAAAAAAAAAAAA",
        "/sessions",
        "/settings/recently-deleted",
        "/no/such/page",
        "/healthz",
    ];
    let signed_out = named_index().replace("</head>", &format!("{}</head>", compat::SIGNED_OUT_META));
    for p in paths {
        for q in ["", "?a=1"] {
            let r = h.get(&format!("{p}{q}")).send().await.unwrap();
            assert_eq!(r.status(), 200, "{p}{q}");
            assert_eq!(r.headers()["cache-control"], "no-store");
            assert_eq!(r.headers()["vary"], "cookie");
            assert_eq!(r.headers()["content-type"], "text/html; charset=utf-8");
            assert_eq!(r.text().await.unwrap(), signed_out, "{p}{q} signed out");
            let r = h
                .get(&format!("{p}{q}"))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            assert_eq!(r.text().await.unwrap(), signed_in_index(), "{p}{q} signed in");
        }
    }
    // the assertion header is not read on page paths (the edge does not send it there)
    let r = h.as_token(h.get("/new"), &h.token(STAFF)).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), signed_out);
    // a verified cookie of someone not allowed: the 403 page
    let r = h
        .get("/new")
        .header("cookie", format!("CF_Authorization={}", h.token(OUTSIDER)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let t = r.text().await.unwrap();
    assert!(
        t.contains(OUTSIDER) && t.contains("沒有權限") && t.contains("Not allowed"),
        "{t}"
    );
}

#[tokio::test]
async fn the_early_message_box_reaches_the_browser_unchanged() {
    let h = Harness::start().await;
    let t = h.get("/").send().await.unwrap().text().await.unwrap();
    let textarea = t.find("<textarea id=\"early-box\">").unwrap();
    let inline = t.find("<script>window.__early=1</script>").unwrap();
    let first_src = t.find("<script src=").unwrap();
    assert!(inline < first_src && textarea < first_src);
    let meta = t.find(compat::SIGNED_OUT_META).unwrap();
    assert!(
        meta < t.find("</head>").unwrap() && meta > inline,
        "the meta goes in just before </head>"
    );
    // the product name follows the facts without a restart
    h.set_facts(|d| d.product.name = "Renamed <AI>".into()).await;
    let t = h.get("/").send().await.unwrap().text().await.unwrap();
    assert!(t.contains("<title>Renamed &lt;AI&gt;</title>"), "{t}");
    assert!(t.contains("content=\"Renamed &lt;AI&gt;\""), "{t}");
}

#[tokio::test]
async fn every_answer_names_the_build() {
    let h = Harness::start().await;
    let want = format!("pigeon/{}", aviary_core::version::sha12());
    for r in [
        h.get("/").send().await.unwrap(),
        h.get("/missing.js").send().await.unwrap(),
        h.get(me()).send().await.unwrap(),
        h.get("/ops/").send().await.unwrap(),
        h.get("/a%00b").send().await.unwrap(),
    ] {
        assert_eq!(r.headers()["server"], want.as_str());
    }
}

// ---- Routing (spec 5.1) ----

#[tokio::test]
async fn routing_table_order_and_normalisation() {
    let h = Harness::start().await;
    for p in ["/api", "/api/", "/paseo-ws"] {
        let r = h.get(p).send().await.unwrap();
        assert_eq!(r.status(), 302, "{p}");
        assert_eq!(r.headers()["location"], "/");
    }
    // unknown /api: identity first, then 404 JSON, never the shell
    assert_eq!(code(h.get("/api/unknown").send().await.unwrap()).await.0, 401);
    let r = h.get_as("/api/unknown", STAFF).await;
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(code(r).await, (404, "NOT_AVAILABLE".into()));
    assert_eq!(
        code(h.get_as(&format!("{}/ws-old", compat::API), STAFF).await).await.0,
        404
    );
    // a path that changes class on normalisation, NUL, backslash
    for p in ["/ops/../api/x", "/x/%2e%2e/api/x", "/a%00b", "/a%5Cb"] {
        let r = h.get_as(p, ADMIN).await;
        assert_eq!(r.status(), 404, "{p}");
    }
    // dot segments that stay inside the class are resolved
    let r = h
        .get_as(&format!("/api/x/..{}", compat::ME.trim_start_matches("/api")), STAFF)
        .await;
    assert_eq!(r.status(), 200);
    // OPTIONS on /api: 405, no CORS headers
    let r = h
        .http
        .request(reqwest::Method::OPTIONS, format!("{}{}", h.url, me()))
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 405);
    assert!(r.headers().keys().all(|k| !k.as_str().starts_with("access-control-")));
    // an upgrade anywhere but /paseo-ws
    let r = h
        .get("/api/x")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    // the socket needs a person
    let r = h
        .get("/paseo-ws")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // client-supplied Aviary-* headers never reach an upstream
    let cid = uuid::Uuid::new_v4();
    h.magpie.fake.script().always(
        "files_raw",
        Reply::bytes(200, "x").with_header("content-type", "text/plain"),
    );
    let r = h
        .as_token(
            h.get(&format!("{}{cid}/files/raw?path=a.txt", compat::SESSIONS)),
            &h.token(STAFF),
        )
        .header("aviary-person", "victim@example.org")
        .header("aviary-viewer", ADMIN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let call = h.magpie.fake.script().calls_to("files_raw").pop().unwrap();
    assert_eq!(call.header("aviary-person"), Some(STAFF));
    assert_eq!(call.header("aviary-viewer"), None);
    assert!(call.header("aviary-request").is_some());
}

// ---- Sign-in and me (spec 5.3-5.4) ----

#[tokio::test]
async fn sign_in_redirects_same_origin_only() {
    let h = Harness::start().await;
    let go = |next: &str| format!("{}?next={next}", compat::SIGNIN);
    let me_path = compat::ME.to_owned();
    for (next, to) in [
        ("/h/srv_conversations", "/h/srv_conversations"),
        ("%2Fs%2Fabc%3Fx%3D1", "/s/abc?x=1"),
        ("//evil.example", "/"),
        ("/%5Cevil.example", "/"),
        ("https://evil.example", "/"),
        (me_path.as_str(), "/"),
        ("/ops", "/"),
        ("", "/"),
    ] {
        let r = h.get_as(&go(next), STAFF).await;
        assert_eq!(r.status(), 302, "{next}");
        assert_eq!(r.headers()["location"], to, "{next}");
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert_eq!(r.headers()["referrer-policy"], "no-referrer");
    }
    assert_eq!(h.get(&go("/x")).send().await.unwrap().status(), 401);
    assert_eq!(h.get_as(&go("/x"), OUTSIDER).await.status(), 403);
}

#[tokio::test]
async fn app_sign_in_hands_the_token_to_the_app_scheme() {
    let h = Harness::start().await;
    let token = h.token(STAFF);
    let state = "abcdefghijklmnopqrst";
    let r = h
        .get(&format!("{}?state={state}", compat::APP_SIGNIN))
        .header("cookie", format!("CF_Authorization={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 302);
    assert_eq!(
        r.headers()["location"],
        format!("dev.example.app://signed-in?state={state}#token={token}").as_str(),
        "a JWT is base64url and dots: nothing to encode"
    );
    assert_eq!(r.headers()["referrer-policy"], "no-referrer");
    // the verified token is handed over, never a different cookie beside it (stale, or another team's)
    let other = h.teams[1].token(STAFF);
    let r = h
        .get(&format!("{}?state={state}", compat::APP_SIGNIN))
        .header("cf-access-jwt-assertion", &token)
        .header("cookie", format!("CF_Authorization={other}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 302);
    assert_eq!(
        r.headers()["location"],
        format!("dev.example.app://signed-in?state={state}#token={token}").as_str()
    );
    let r = h.get_as(&format!("{}?state=short", compat::APP_SIGNIN), STAFF).await;
    assert_eq!(r.status(), 400);
    let r = h
        .as_token(h.post(&format!("{}?state={state}", compat::APP_SIGNIN)), &token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 405);
    // a view-as token is ignored here
    let vt = h.view_token(ADMIN, STAFF, now() + 600);
    let r = h
        .get_as(
            &format!("{}?state={state}&{}={vt}", compat::APP_SIGNIN, compat::VIEW_AS_QUERY),
            ADMIN,
        )
        .await;
    assert_eq!(r.status(), 302);
    // no app scheme: iOS sign-in is off
    h.set_facts(|d| d.sign_in.app_scheme = None).await;
    let r = h.get_as(&format!("{}?state={state}", compat::APP_SIGNIN), STAFF).await;
    assert_eq!(r.status(), 404);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/html"));
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert!(
        r.text().await.unwrap().contains("這項功能在這裏未有開放。"),
        "the 404 page"
    );
}

#[tokio::test]
async fn me_gives_the_first_name() {
    let h = Harness::start().await;
    let r = h.get_as(me(), STAFF).await;
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(body_json(r).await, json!({"firstName": "Tai", "sharing": true}));
    // no name claim: Access's get-identity answers (fake-access answers from the token's own name)
    let t1 = &h.teams[0];
    let bare = t1
        .issuer
        .mint(&fake_access::MintRequest {
            email: STAFF2.into(),
            name: None,
            aud: AUD1.into(),
            exp_seconds: 3600,
            nbf_seconds: None,
            kid: None,
        })
        .unwrap();
    let v = body_json(h.as_token(h.get(me()), &bare).send().await.unwrap()).await;
    assert_eq!(v["sharing"], true);
    assert_eq!(v["firstName"], serde_json::Value::Null);
}

// ---- Models and warm (spec 5.5-5.6) ----

#[tokio::test]
async fn the_model_list_is_cached_and_kept_on_error() {
    let h = Harness::start().await;
    let path = "/api/providers/claude/models";
    let want = json!({"success": true, "data": {"models": {
        "OPTIONS": [{"value": "vendor/model-a", "label": "Model A"}, {"value": "vendor/model-b", "label": "vendor/model-b"}],
        "DEFAULT": "vendor/model-a"}}});
    assert_eq!(body_json(h.get_as(path, STAFF).await).await, want);
    assert_eq!(body_json(h.get_as(path, STAFF).await).await, want);
    assert_eq!(h.models_hits.load(Ordering::SeqCst), 1, "cached");
    h.models_up.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        body_json(h.get_as(path, STAFF).await).await,
        want,
        "the last list stands while model fails"
    );
}

#[tokio::test]
async fn a_stale_model_list_is_answered_at_once_while_one_refresh_hangs() {
    let h = Harness::start().await;
    let path = "/api/providers/claude/models";
    let want = body_json(h.get_as(path, STAFF).await).await;
    assert_eq!(want["success"], true);
    assert_eq!(h.models_hits.load(Ordering::SeqCst), 1);
    h.models_hang.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    for _ in 0..2 {
        let started = Instant::now();
        let burst = (0..20).map(|_| async { body_json(h.get_as(path, STAFF).await).await });
        for got in futures_util::future::join_all(burst).await {
            assert_eq!(got, want, "the stale list");
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "nobody waits for the hanging refresh: {:?}",
            started.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert_eq!(h.models_hits.load(Ordering::SeqCst), 2, "one refresh for both bursts");
}

#[tokio::test]
async fn the_model_list_fails_clearly_with_nothing_cached() {
    let h = Harness::start().await;
    h.models_up.store(false, Ordering::SeqCst);
    let r = h.get_as("/api/providers/claude/models", STAFF).await;
    assert_eq!(code(r).await, (502, "MODEL_LIST_UNAVAILABLE".into()));
}

#[tokio::test]
async fn warm_hints_are_coalesced() {
    let h = Harness::start().await;
    let cid = uuid::Uuid::new_v4();
    let t = h.token(STAFF);
    for _ in 0..3 {
        let r = h
            .as_token(h.post(compat::WARM), &t)
            .json(&json!({"sessionId": cid}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 202);
        assert_eq!(body_json(r).await, json!({"ok": true}));
    }
    let r = h.as_token(h.post(compat::WARM), &t).body("").send().await.unwrap();
    assert_eq!(r.status(), 202, "a new chat (no body) is its own key");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let hints = h.hermit.fake.hints();
    assert_eq!(hints.len(), 2, "{hints:?}");
    assert!(
        hints
            .iter()
            .any(|w| w.person == STAFF && w.conversation_id == Some(cid))
    );
    assert!(hints.iter().any(|w| w.conversation_id.is_none()));
    tokio::time::sleep(Duration::from_millis(600)).await;
    h.as_token(h.post(compat::WARM), &t)
        .json(&json!({"sessionId": cid}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.hermit.fake.hints().len(), 3, "a new hint after the window");
    // a present but invalid sessionId is no hint at all (not a new-chat hint)
    let other = h.token(STAFF2);
    for body in [json!({"sessionId": "not-a-uuid"}), json!({"sessionId": 7})] {
        let r = h
            .as_token(h.post(compat::WARM), &other)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 202);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        h.hermit.fake.hints().iter().all(|w| w.person != STAFF2),
        "{:?}",
        h.hermit.fake.hints()
    );
}

// ---- Same origin (spec 5.13) ----

#[tokio::test]
async fn cross_origin_writes_are_refused_and_native_ones_allowed() {
    let h = Harness::start().await;
    let t = h.token(STAFF);
    let r = h
        .as_token(h.post(compat::WARM), &t)
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(code(r).await, (403, "CROSS_ORIGIN".into()));
    let r = h
        .as_token(h.post(compat::WARM), &t)
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let host = h.url.trim_start_matches("http://").to_owned();
    let r = h
        .as_token(h.post(compat::WARM), &t)
        .header("origin", format!("http://{host}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202, "same origin");
    let r = h.as_token(h.post(compat::WARM), &t).send().await.unwrap();
    assert_eq!(r.status(), 202, "native: no Origin");
}

// ---- View as (spec 6) ----

async fn start_view(h: &Harness, admin_token: &str, user: &str) -> reqwest::Response {
    let host = h.url.trim_start_matches("http://").to_owned();
    h.as_token(h.post(compat::VIEW_AS), admin_token)
        .header("origin", format!("http://{host}"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("user={}", user.replace('@', "%40")))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn view_as_start_is_admin_only_same_origin_recorded_first() {
    let h = Harness::start().await;
    let admin = h.token(ADMIN);
    // staff cannot start a view
    let r = start_view(&h, &h.token(STAFF), STAFF2).await;
    assert_eq!(r.status(), 403);
    assert!(r.text().await.unwrap().contains("Admins only"));
    // the target must be allowed
    let r = start_view(&h, &admin, OUTSIDER).await;
    assert_eq!(r.status(), 400);
    assert!(r.text().await.unwrap().contains("not a user of this instance"));
    let r = start_view(&h, &admin, "@example.org").await;
    assert_eq!(r.status(), 400);
    // not same-origin: an Origin elsewhere, or a navigation from nowhere (`none`)
    let r = h
        .as_token(h.post(compat::VIEW_AS), &admin)
        .header("origin", "https://evil.example")
        .body(format!("user={STAFF}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = h
        .as_token(h.post(compat::VIEW_AS), &admin)
        .header("sec-fetch-site", "none")
        .body(format!("user={STAFF}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = h.as_token(h.get(compat::VIEW_AS), &admin).send().await.unwrap();
    assert_eq!(r.status(), 405);
    // magpie cannot record (unscripted: 501): no token
    let r = start_view(&h, &admin, STAFF).await;
    assert_eq!(r.status(), 502);
    assert!(r.headers().get("location").is_none());
    assert!(r.text().await.unwrap().contains("could not be recorded"));
    // recorded: 303 to the app with the token in the fragment
    h.magpie.fake.script().always("view_as", Reply::empty(201));
    let r = start_view(&h, &admin, &STAFF.to_uppercase()).await;
    assert_eq!(r.status(), 303);
    assert_eq!(r.headers()["referrer-policy"], "no-referrer");
    let loc = r.headers()["location"].to_str().unwrap().to_owned();
    let token = loc.strip_prefix(&format!("/#{}", compat::VIEW_AS_FRAGMENT)).unwrap();
    let rec = h.magpie.fake.script().calls_to("view_as").pop().unwrap();
    let body = rec.body.unwrap();
    assert_eq!(body["admin"], ADMIN);
    assert_eq!(body["person"], STAFF, "trimmed and lowercased");
    let claims = pigeon::viewas::verify(&pigeon::KeyList(vec![VIEW_KEY]), token, now()).unwrap();
    assert_eq!(claims.s, body["session"].as_str().unwrap());
    assert!(claims.e > now() + 43_000);
    // the token reaches the person's data, as the person, with the viewer headers
    let r = h
        .get_as(&format!("{}?{}={token}", me(), compat::VIEW_AS_QUERY), ADMIN)
        .await;
    let v = body_json(r).await;
    assert_eq!(v["viewAs"]["user"], STAFF);
    let cid = uuid::Uuid::new_v4();
    h.magpie.fake.script().always(
        "files_raw",
        Reply::bytes(200, "x").with_header("content-type", "text/plain"),
    );
    let r = h
        .get_as(
            &format!(
                "{}{cid}/files/raw?path=a.txt&{}={token}",
                compat::SESSIONS,
                compat::VIEW_AS_QUERY
            ),
            ADMIN,
        )
        .await;
    assert_eq!(r.status(), 200);
    let call = h.magpie.fake.script().calls_to("files_raw").pop().unwrap();
    assert_eq!(call.header("aviary-person"), Some(STAFF));
    assert_eq!(call.header("aviary-viewer"), Some(ADMIN));
    assert_eq!(call.header("aviary-view-session"), Some(claims.s.as_str()));
    let q = call.query.unwrap();
    assert!(
        q.get(compat::VIEW_AS_QUERY).is_none(),
        "the view-as parameter is never forwarded"
    );
}

#[tokio::test]
async fn view_as_tokens_are_refused_unless_valid_for_this_admin() {
    let h = Harness::start().await;
    let refused = |r: reqwest::Response| async move { code(r).await };
    let path = |t: &str| format!("{}?{}={t}", me(), compat::VIEW_AS_QUERY);
    let good = h.view_token(ADMIN, STAFF, now() + 600);
    assert_eq!(h.get_as(&path(&good), ADMIN).await.status(), 200);
    // staff using an admin's token
    assert_eq!(
        refused(h.get_as(&path(&good), STAFF2).await).await,
        (403, "VIEW_AS_REFUSED".into())
    );
    // another admin
    assert_eq!(refused(h.get_as(&path(&good), ADMIN2).await).await.1, "VIEW_AS_REFUSED");
    // forged, expired
    let forged = pigeon::viewas::sign(
        &pigeon::KeyList(vec![[9; 32]]),
        &pigeon::viewas::Claims {
            a: ADMIN.into(),
            u: STAFF.into(),
            s: "s".into(),
            e: now() + 600,
        },
    );
    assert_eq!(
        refused(h.get_as(&path(&forged), ADMIN).await).await.1,
        "VIEW_AS_REFUSED"
    );
    let expired = h.view_token(ADMIN, STAFF, now() - 1);
    assert_eq!(
        refused(h.get_as(&path(&expired), ADMIN).await).await.1,
        "VIEW_AS_REFUSED"
    );
    // never the caller's own data on a refusal: an upstream is not called
    h.magpie
        .fake
        .script()
        .always("list_shares", Reply::ok(json!({"shares": []})));
    let cid = uuid::Uuid::new_v4();
    let r = h
        .get_as(
            &format!("{}{cid}/shares?{}={forged}", compat::SESSIONS, compat::VIEW_AS_QUERY),
            ADMIN,
        )
        .await;
    assert_eq!(r.status(), 403);
    assert!(h.magpie.fake.script().calls_to("list_shares").is_empty());
    // the person removed, then the admin demoted
    h.hermit
        .fake
        .configure(|c| c.users = vec![ADMIN.into(), ADMIN2.into(), STAFF2.into()]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(refused(h.get_as(&path(&good), ADMIN).await).await.1, "VIEW_AS_REFUSED");
    let good2 = h.view_token(ADMIN, STAFF2, now() + 600);
    assert_eq!(h.get_as(&path(&good2), ADMIN).await.status(), 200);
    h.hermit.fake.configure(|c| c.admins = vec![ADMIN2.into()]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(refused(h.get_as(&path(&good2), ADMIN).await).await.1, "VIEW_AS_REFUSED");
}

#[tokio::test]
async fn view_as_is_read_only_on_rest_except_warm_and_uploads() {
    let h = Harness::start().await;
    let vt = h.view_token(ADMIN, STAFF, now() + 600);
    let admin = h.token(ADMIN);
    let q = format!("{}={vt}", compat::VIEW_AS_QUERY);
    let cid = uuid::Uuid::new_v4();
    let share = "AAAAAAAAAAAAAAAAAAAAAA";
    for (method, path) in [
        (reqwest::Method::POST, format!("{}{cid}/shares?{q}", compat::SESSIONS)),
        (reqwest::Method::DELETE, format!("{}{share}?{q}", compat::SHARES)),
        (reqwest::Method::POST, format!("{}{share}/continue?{q}", compat::SHARES)),
        (reqwest::Method::POST, format!("{}?{q}", compat::VIEW_AS)),
    ] {
        let r = h
            .as_token(h.http.request(method.clone(), format!("{}{path}", h.url)), &admin)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403, "{method} {path}");
    }
    let r = h
        .as_token(
            h.http.post(format!("{}{}{cid}/shares?{q}", h.url, compat::SESSIONS)),
            &admin,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(code(r).await, (403, "VIEW_AS_READ_ONLY".into()));
    assert!(h.magpie.fake.script().calls_to("create_share").is_empty());
    let r = h
        .as_token(h.post(&format!("{}?{q}", compat::WARM)), &admin)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    // the ops page is refused in a view-as tab
    let r = h.as_token(h.get(&format!("/ops/?{q}")), &admin).send().await.unwrap();
    assert_eq!(r.status(), 403);
    assert!(h.hermit.fake.script().calls_to("ops_page").is_empty());
}

// ---- The raw view (Deyao, 2026-11-05) ----

#[tokio::test]
async fn the_raw_view_is_admin_only_and_reads_the_transcript_as_cells_only_while_viewing_as() {
    let h = Harness::start().await;
    let cid = uuid::Uuid::new_v4();
    let page = format!("/raw/{cid}");
    // the page: an admin's cookie only
    assert_eq!(h.get(&page).send().await.unwrap().status(), 401);
    let staff = format!("CF_Authorization={}", h.token(STAFF));
    let r = h.get(&page).header("cookie", &staff).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let admin_cookie = format!("CF_Authorization={}", h.token(ADMIN));
    let r = h.get(&page).header("cookie", &admin_cookie).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.text().await.unwrap(), pigeon::raw::page_html());
    let r = h
        .get("/raw/not-a-uuid")
        .header("cookie", &admin_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    // the data: a view-as identity only; magpie is not called otherwise
    h.magpie.fake.script().always(
        "transcript",
        Reply::ok(json!({
            "path": "projects/-workspace/s.jsonl",
            "lines": [
                {"ord": 4, "line": r#"{"type":"user","timestamp":"2026-11-05T03:12:09.120Z","message":{"content":"hello"}}"#},
                {"ord": 5, "line": r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#}
            ],
            "next": 5, "more": false, "running": true
        })),
    );
    let data = format!("{}{cid}/transcript", compat::SESSIONS);
    for who in [ADMIN, STAFF] {
        let r = h.get_as(&data, who).await;
        assert_eq!(code(r).await, (404, "NOT_AVAILABLE".into()), "{who} not viewing as");
    }
    assert!(h.magpie.fake.script().calls_to("transcript").is_empty());
    let vt = h.view_token(ADMIN, STAFF, now() + 600);
    let r = h
        .get_as(&format!("{data}?after=3&{}={vt}", compat::VIEW_AS_QUERY), STAFF2)
        .await;
    assert_eq!(code(r).await.1, "VIEW_AS_REFUSED");
    let r = h
        .get_as(&format!("{data}?after=3&{}={vt}", compat::VIEW_AS_QUERY), ADMIN)
        .await;
    assert_eq!(r.status(), 200);
    let v = body_json(r).await;
    assert_eq!(v["next"], 5);
    assert_eq!(v["running"], true);
    assert_eq!(v["cells"][0]["kind"], "user");
    assert_eq!(v["cells"][0]["text"], "hello");
    assert_eq!(v["cells"][1]["kind"], "tool call");
    assert_eq!(v["cells"][1]["title"], "Bash");
    assert_eq!(v["cells"][1]["text"], "command: ls");
    let call = h.magpie.fake.script().calls_to("transcript").pop().unwrap();
    assert_eq!(call.header("aviary-person"), Some(STAFF));
    assert_eq!(call.header("aviary-viewer"), Some(ADMIN));
    assert_eq!(call.query.unwrap()["after"], 3);
}

// ---- Ops (spec 5.11) ----

#[tokio::test]
async fn ops_is_admin_only_read_only_and_forwarded() {
    let h = Harness::start().await;
    h.hermit.fake.script().always(
        "ops_page",
        Reply::bytes(200, "<html>ops</html>")
            .with_header("content-type", "text/html; charset=utf-8")
            .with_header("set-cookie", "x=1"),
    );
    // signed out, staff
    assert_eq!(h.get("/ops/").send().await.unwrap().status(), 401);
    let r = h.get_as("/ops/", STAFF).await;
    assert_eq!(r.status(), 403);
    assert!(r.text().await.unwrap().contains("只限管理員"));
    assert!(
        h.hermit.fake.script().calls_to("ops_page").is_empty(),
        "nothing forwarded"
    );
    // /ops -> /ops/
    let r = h.get_as("/ops", ADMIN).await;
    assert_eq!(r.status(), 302);
    assert_eq!(r.headers()["location"], "/ops/");
    // the query stays
    let r = h.get_as("/ops?tz=Asia%2FHong_Kong&x=1", ADMIN).await;
    assert_eq!(r.status(), 302);
    assert_eq!(r.headers()["location"], "/ops/?tz=Asia%2FHong_Kong&x=1");
    // forwarded with the bearer and the admin
    let r = h
        .as_token(h.get("/ops/?tz=Asia%2FHong_Kong"), &h.token(ADMIN))
        .header("cookie", "secret=1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers().get("set-cookie").is_none(), "set-cookie is dropped");
    assert_eq!(r.text().await.unwrap(), "<html>ops</html>");
    let call = h.hermit.fake.script().calls_to("ops_page").pop().unwrap();
    assert_eq!(call.header("aviary-person"), Some(ADMIN));
    assert!(call.header("authorization").unwrap().starts_with("Bearer "));
    assert_eq!(call.header("cookie"), None, "no cookie goes up");
    // a dot segment encoded twice stays under /ops: refused before anything is forwarded
    for path in [
        "/ops/%252e%252e/v1/deployment",
        "/ops/%252E%252E/v1/people/admin@example.com",
    ] {
        let r = h.get_as(path, ADMIN).await;
        assert_eq!(r.status(), 404, "{path}");
        assert_eq!(
            r.text().await.unwrap(),
            "Not found",
            "{path}: pigeon's 404, not hermit's answer"
        );
    }
    // `%3F` and `%23` stay in the path: `/ops/%3F` is not `/ops/` with a query
    let before = h.hermit.fake.script().calls_to("ops_page").len();
    for path in ["/ops/%3Fx=1", "/ops/%23", "/ops/a%3Fb"] {
        let r = h.get_as(path, ADMIN).await;
        assert_eq!(r.status(), 404, "{path}");
    }
    assert_eq!(
        h.hermit.fake.script().calls_to("ops_page").len(),
        before,
        "none of them reached hermit's /ops/"
    );
    // read-only
    let r = h.as_token(h.post("/ops/"), &h.token(ADMIN)).send().await.unwrap();
    assert_eq!(r.status(), 405);
    // hermit failing: the 502 page
    h.hermit.fake.faults().everywhere(FaultKind::Drop);
    let r = h.get_as("/ops/", ADMIN).await;
    assert_eq!(r.status(), 502);
    assert!(r.text().await.unwrap().contains("operator page did not answer"));
}

// ---- Uploads (spec 5.7) ----

fn multipart(parts: &[(&str, &str, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "----pigeon-test-boundary";
    let mut body = Vec::new();
    for (name, file, bytes) in parts {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[tokio::test]
async fn uploads_stream_to_magpie_with_their_limits() {
    let h = Harness::start().await;
    let t = h.token(STAFF);
    h.magpie.fake.script().always(
        "upload",
        Reply::json(
            201,
            json!({"uploadId": uuid::Uuid::new_v4(), "path": "uploads/7c9e/report.xlsx", "name": "report.xlsx",
                   "size": 5, "mimeType": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"}),
        ),
    );
    let (ct, body) = multipart(&[("files", "re/port.xlsx", b"hello")]);
    let r = h
        .as_token(h.post("/api/assets/files"), &t)
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(
        body_json(r).await,
        json!({"attachments": [{"path": "uploads/7c9e/report.xlsx", "name": "report.xlsx", "size": 5,
               "mimeType": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"}]})
    );
    let call = h.magpie.fake.script().calls_to("upload").pop().unwrap();
    assert_eq!(call.bytes.as_deref(), Some(&b"hello"[..]));
    assert_eq!(call.header("aviary-person"), Some(STAFF));
    assert_eq!(call.header("aviary-upload-kind"), Some("file"));
    assert_eq!(call.header("aviary-upload-name"), Some("re_port.xlsx"));
    assert_eq!(
        call.header("content-type"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "octet-stream parts take the type of their extension"
    );
    // two files: refused, and magpie stores neither (its body breaks before it ends)
    let stored = h.magpie.fake.script().calls_to("upload").len();
    let (ct, body) = multipart(&[("files", "a.txt", b"a"), ("files", "b.txt", b"b")]);
    let r = h
        .as_token(h.post("/api/assets/files"), &t)
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.magpie.fake.script().calls_to("upload").len(),
        stored,
        "the first of two files is not stored"
    );
    // none
    let (ct, body) = multipart(&[("other", "a.txt", b"a")]);
    let r = h
        .as_token(h.post("/api/assets/files"), &t)
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    // the first guard: a declared length over 50 MiB + 1 MiB, answered before reading
    // (a raw client: it sends 16 bytes of a declared 53 477 377 and reads the answer)
    let answer = {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut c = tokio::net::TcpStream::connect(h.url.trim_start_matches("http://"))
            .await
            .unwrap();
        let head = format!(
            "POST /api/assets/files HTTP/1.1\r\nhost: pigeon.test\r\ncf-access-jwt-assertion: {t}\r\n\
             content-type: multipart/form-data; boundary=x\r\ncontent-length: 53477377\r\n\r\n"
        );
        c.write_all(head.as_bytes()).await.unwrap();
        c.write_all(&[0u8; 16]).await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut chunk))
                .await
                .expect("pigeon answers before the body arrives")
                .unwrap_or(0);
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).into_owned();
            if n == 0 || text.contains('}') {
                break text;
            }
        }
    };
    assert!(answer.starts_with("HTTP/1.1 413"), "{answer}");
    assert!(
        answer.contains("檔案太大（上限 50MB）。"),
        "a sentence without the name, which is not known yet: {answer}"
    );
    // the second guard: a chunked body whose file passes 50 MiB
    let big = vec![b'x'; 52_428_801];
    let (ct, body) = multipart(&[("files", "big.bin", &big)]);
    let stream = futures_util::stream::iter(
        body.chunks(1 << 20)
            .map(|c| Ok::<_, std::io::Error>(c.to_vec()))
            .collect::<Vec<_>>(),
    );
    let calls_before = h.magpie.fake.script().calls_to("upload").len();
    let r = h
        .as_token(h.post("/api/assets/files"), &t)
        .header("content-type", ct)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    let v = body_json(r).await;
    assert_eq!(
        v["error"], "「big.bin」太大（上限 50MB）。",
        "zh-HK deployment: the localized sentence"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.magpie.fake.script().calls_to("upload").len(),
        calls_before,
        "magpie stored nothing for the aborted upload"
    );
    // magpie's 415 and failures
    h.magpie
        .fake
        .script()
        .always("upload", Reply::error(415, "unsupported_type", "no"));
    let (ct, body) = multipart(&[("files", "a.exe", b"MZ")]);
    let r = h
        .as_token(h.post("/api/assets/files"), &t)
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 415);
    assert_eq!(body_json(r).await["error"], "不支援這種檔案。");
    h.set_facts(|d| d.product.locale = "en".into()).await;
    h.magpie
        .fake
        .script()
        .always("upload", Reply::error(500, "internal", "no"));
    let (ct, body) = multipart(&[("files", "a.txt", b"a")]);
    let r = h
        .as_token(h.post("/api/assets/files"), &t)
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(body_json(r).await["error"], "Upload failed. Please try again.");
}

/// Sends a one-file upload on a raw connection, one `chunk`-byte piece every `every`, `pieces` pieces, while reading
/// the answer; returns the answer and when it arrived. The sender keeps going (ignoring a closed connection) so an
/// early answer shows pigeon did not wait for the rest.
async fn trickle_upload(h: &Harness, chunk: usize, pieces: usize, every: Duration) -> (String, Duration) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let boundary = "trickle";
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"slow.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    );
    let tail = format!("\r\n--{boundary}--\r\n");
    let len = head.len() + chunk * pieces + tail.len();
    let c = tokio::net::TcpStream::connect(h.url.trim_start_matches("http://"))
        .await
        .unwrap();
    let (mut rd, mut wr) = c.into_split();
    let request = format!(
        "POST /api/assets/files HTTP/1.1\r\nhost: pigeon.test\r\ncf-access-jwt-assertion: {}\r\n\
         content-type: multipart/form-data; boundary={boundary}\r\ncontent-length: {len}\r\n\r\n{head}",
        h.token(STAFF)
    );
    let started = Instant::now();
    tokio::spawn(async move {
        if wr.write_all(request.as_bytes()).await.is_err() {
            return;
        }
        for _ in 0..pieces {
            tokio::time::sleep(every).await;
            if wr.write_all(&vec![b'q'; chunk]).await.is_err() {
                return;
            }
        }
        let _ = wr.write_all(tail.as_bytes()).await;
        // hold the write half open until the answer is read
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let mut buf = Vec::new();
    let mut piece = [0u8; 4096];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(30), rd.read(&mut piece))
            .await
            .unwrap()
            .unwrap_or(0);
        buf.extend_from_slice(&piece[..n]);
        let text = String::from_utf8_lossy(&buf).into_owned();
        if n == 0 || text.contains('}') {
            return (text, started.elapsed());
        }
    }
}

#[tokio::test]
async fn a_slow_upload_that_keeps_moving_outlasts_the_idle_bound() {
    // spec 5.7 / 11.2 step 4 scaled: a 1 s idle bound, a 64 KiB piece every 300 ms for about 3.6 s (the review's
    // 50 MiB at 2 Mbit/s case: never idle, several idle bounds long)
    let h = Harness::start_with(pigeon::Tuning {
        transfer_idle: Duration::from_secs(1),
        ..tuning()
    })
    .await;
    h.magpie.fake.script().always(
        "upload",
        Reply::json(
            201,
            json!({"uploadId": uuid::Uuid::new_v4(), "path": "uploads/1/slow.bin", "name": "slow.bin",
                   "size": 786_432, "mimeType": "application/octet-stream"}),
        ),
    );
    let (answer, took) = trickle_upload(&h, 65_536, 12, Duration::from_millis(300)).await;
    assert!(answer.starts_with("HTTP/1.1 201"), "{answer}");
    assert!(took > Duration::from_secs(3), "{took:?}");
    let call = h.magpie.fake.script().calls_to("upload").pop().unwrap();
    assert_eq!(call.bytes.unwrap().len(), 786_432, "every byte reached magpie");
}

#[tokio::test]
async fn magpie_refusing_mid_body_is_answered_at_once() {
    // magpie answers before reading the body: pigeon answers its mapped status at once with `connection: close`,
    // without reading the client's remaining bytes (16 pieces, 300 ms apart, would take about 5 s)
    let h = Harness::start_with(pigeon::Tuning {
        transfer_idle: Duration::from_secs(1),
        ..tuning()
    })
    .await;
    h.magpie
        .fake
        .faults()
        .on("upload", FaultKind::Unavailable { retry_after_secs: 1 });
    let (answer, took) = trickle_upload(&h, 65_536, 16, Duration::from_millis(300)).await;
    assert!(answer.starts_with("HTTP/1.1 502"), "{answer}");
    assert!(answer.to_ascii_lowercase().contains("connection: close"), "{answer}");
    assert!(answer.contains("上載失敗，請再試一次。"), "{answer}");
    assert!(took < Duration::from_secs(2), "answered before the body ends: {took:?}");
}

// ---- Files and blobs (spec 5.8-5.9) ----

#[tokio::test]
async fn files_pass_through_with_ranges() {
    let h = Harness::start().await;
    let cid = uuid::Uuid::new_v4();
    h.magpie.fake.script().always(
        "files_raw",
        Reply::bytes(206, "lo wo")
            .with_header("content-type", "text/plain")
            .with_header("content-range", "bytes 3-7/11")
            .with_header("accept-ranges", "bytes")
            .with_header("etag", "\"e1\"")
            .with_header("content-disposition", "attachment; filename=\"a.txt\"")
            .with_header("x-internal", "dropped"),
    );
    let r = h
        .as_token(
            h.get(&format!(
                "{}{cid}/files/raw?path=out%2Fa.txt&inline=1&sent=toolu_01",
                compat::SESSIONS
            )),
            &h.token(STAFF),
        )
        .header("range", "bytes=3-7")
        .header("if-none-match", "\"e0\"")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["content-range"], "bytes 3-7/11");
    assert_eq!(r.headers()["etag"], "\"e1\"");
    assert_eq!(r.headers()["content-disposition"], "attachment; filename=\"a.txt\"");
    assert!(r.headers().get("x-internal").is_none());
    assert_eq!(r.text().await.unwrap(), "lo wo");
    let call = h.magpie.fake.script().calls_to("files_raw").pop().unwrap();
    assert_eq!(call.header("range"), Some("bytes=3-7"));
    assert_eq!(call.header("if-none-match"), Some("\"e0\""));
    assert_eq!(call.param("id"), Some(cid.to_string().as_str()));
    let q = call.query.unwrap();
    assert_eq!(q["path"], "out/a.txt");
    assert_eq!(q["sent"], "toolu_01");
    // magpie's JSON errors pass through
    h.magpie
        .fake
        .script()
        .always("files_raw", Reply::error(404, "not_found", "no such file"));
    let r = h
        .get_as(&format!("{}{cid}/files/raw?path=a.txt", compat::SESSIONS), STAFF)
        .await;
    assert_eq!(r.status(), 404);
    assert_eq!(body_json(r).await["code"], "not_found");
    // bad parameters never reach magpie
    let n = h.magpie.fake.script().calls_to("files_raw").len();
    for q in ["path=%2Fetc%2Fpasswd", "path=a&inline=yes", "path=a&sent=a.b", ""] {
        let r = h
            .get_as(&format!("{}{cid}/files/raw?{q}", compat::SESSIONS), STAFF)
            .await;
        assert_eq!(r.status(), 400, "{q}");
    }
    let r = h
        .get_as(&format!("{}not-a-uuid/files/raw?path=a", compat::SESSIONS), STAFF)
        .await;
    assert_eq!(r.status(), 400);
    assert_eq!(h.magpie.fake.script().calls_to("files_raw").len(), n);
}

#[tokio::test]
async fn blob_urls_are_signed_capabilities() {
    let h = Harness::start().await;
    h.magpie.fake.script().always(
        "blob",
        Reply::bytes(200, "WEBP").with_header("content-type", "image/webp"),
    );
    let url = h
        .app
        .blob_signer()
        .url("blob-1", magpie_api::uploads::BlobVariant::Thumb);
    let r = h.get_as(&url, STAFF).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "private, max-age=86400, immutable");
    assert_eq!(r.headers()["content-type"], "image/webp");
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"WEBP");
    let call = h.magpie.fake.script().calls_to("blob").pop().unwrap();
    assert_eq!(call.query.as_ref().unwrap()["variant"], "thumb");
    assert_eq!(call.header("aviary-person"), None, "M15 carries no person");
    // a bad signature, another variant, another blob: 404, magpie not asked
    let n = h.magpie.fake.script().calls_to("blob").len();
    let other_variant = url.replace("v=thumb", "v=full");
    let other_blob = url.replace("blob-1", "blob-2");
    let bad_sig = format!("{}AAAA", &url[..url.len() - 4]);
    for u in [
        other_variant,
        other_blob,
        bad_sig,
        "/api/blobs/blob-1?v=thumb".to_owned(),
    ] {
        assert_eq!(h.get_as(&u, STAFF).await.status(), 404, "{u}");
    }
    assert_eq!(h.magpie.fake.script().calls_to("blob").len(), n);
    // still under /api: a signed URL needs a signed-in person
    assert_eq!(h.get(&url).send().await.unwrap().status(), 401);
}

// ---- Shares (spec 5.10) ----

#[tokio::test]
async fn shares_keep_the_apps_shapes() {
    let h = Harness::start().await;
    let cid = uuid::Uuid::new_v4();
    let share = "AbCdEfGhIjKlMnOpQrStUv";
    let s = h.magpie.fake.script();
    s.always(
        "list_shares",
        Reply::ok(json!({"shares": [{"id": share, "createdAt": "2026-11-02T04:05:06.789Z"}]})),
    );
    s.once(
        "create_share",
        Reply::error(409, "nothing_to_share", "no finished turn"),
    );
    s.always(
        "get_share",
        Reply::ok(json!({"id": share, "title": "T", "ownerName": "staff", "isOwner": true,
        "createdAt": "2026-11-02T04:05:06.789Z", "epoch": "1",
        "rows": [
            {"seqStart": 1, "seqEnd": 1, "seqs": [1], "timestamp": "2026-11-02T04:05:06.789Z",
             "item": {"kind": "user", "messageId": "m1", "text": "hi",
                      "images": [{"blobId": "b1", "mimeType": "image/png"}], "files": []}},
            {"seqStart": 2, "seqEnd": 3, "seqs": [2, 3], "timestamp": "2026-11-02T04:05:07.000Z",
             "item": {"kind": "future-kind"}},
            {"seqStart": 4, "seqEnd": 4, "seqs": [4], "timestamp": "2026-11-02T04:05:08.000Z",
             "item": {"kind": "assistant", "messageId": "a1", "blockIndex": 0, "text": "hello"}}
        ]})),
    );
    s.always("continue_share", Reply::json(201, json!({"conversationId": cid})));
    s.always("revoke_share", Reply::empty(204));
    let t = h.token(STAFF);
    let r = h.get_as(&format!("{}{cid}/shares", compat::SESSIONS), STAFF).await;
    assert_eq!(
        body_json(r).await,
        json!({"shares": [{"id": share, "url": format!("/s/{share}"), "createdAt": "2026-11-02T04:05:06.789Z"}]})
    );
    let r = h
        .as_token(h.post(&format!("{}{cid}/shares", compat::SESSIONS)), &t)
        .send()
        .await
        .unwrap();
    assert_eq!(code(r).await, (409, "NOTHING_TO_SHARE".into()));
    s.always(
        "create_share",
        Reply::json(201, json!({"id": share, "createdAt": "2026-11-02T04:05:06.789Z"})),
    );
    let r = h
        .as_token(h.post(&format!("{}{cid}/shares", compat::SESSIONS)), &t)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(body_json(r).await["url"], format!("/s/{share}"));
    let v = body_json(h.get_as(&format!("{}{share}", compat::SHARES), STAFF).await).await;
    assert_eq!(v["title"], "T");
    assert_eq!(v["isOwner"], true);
    let timeline = v["timeline"].as_array().unwrap();
    assert_eq!(timeline.len(), 2, "an unknown row kind shows nothing: {timeline:?}");
    assert_eq!(timeline[0]["item"]["type"], "user_message");
    let img = timeline[0]["item"]["images"][0]["url"].as_str().unwrap();
    assert_eq!(
        h.get_as(img, STAFF).await.status(),
        501,
        "the signed URL reaches magpie (unscripted)"
    );
    assert_eq!(timeline[1]["item"]["text"], "hello");
    let r = h
        .as_token(h.post(&format!("{}{share}/continue", compat::SHARES)), &t)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    assert_eq!(body_json(r).await, json!({"sessionId": cid}));
    let r = h
        .as_token(h.http.delete(format!("{}{}{share}", h.url, compat::SHARES)), &t)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    s.always("get_share", Reply::error(404, "not_found", "gone"));
    let r = h.get_as(&format!("{}{share}", compat::SHARES), STAFF).await;
    assert_eq!(code(r).await, (404, "SHARE_NOT_FOUND".into()));
    s.always("get_share", Reply::error(500, "internal", "boom"));
    let r = h.get_as(&format!("{}{share}", compat::SHARES), STAFF).await;
    assert_eq!(code(r).await, (502, "SHARE_FAILED".into()));
}

// ---- Limits (spec 5.13) ----

/// Sends `raw` on a new connection and reads until the server closes it (or 5 s pass).
async fn raw_exchange(h: &Harness, raw: &[u8]) -> (String, bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut c = tokio::net::TcpStream::connect(h.url.trim_start_matches("http://"))
        .await
        .unwrap();
    c.write_all(raw).await.unwrap();
    let mut buf = Vec::new();
    let closed = tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut buf))
        .await
        .is_ok();
    (String::from_utf8_lossy(&buf).into_owned(), closed)
}

#[tokio::test]
async fn a_body_on_a_get_is_drained_and_the_connection_closed() {
    let h = Harness::start().await;
    let (answer, closed) = raw_exchange(
        &h,
        b"GET / HTTP/1.1\r\nhost: pigeon.test\r\ncontent-length: 10\r\n\r\n0123456789",
    )
    .await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.to_ascii_lowercase().contains("connection: close"), "{answer}");
    assert!(closed, "the connection is closed after the answer");
    // without a body the connection stays open for the next request
    let (answer, closed) = raw_exchange(&h, b"GET / HTTP/1.1\r\nhost: pigeon.test\r\n\r\n").await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(!closed, "keep-alive without a body");
}
