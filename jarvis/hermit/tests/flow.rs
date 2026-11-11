//! hermit-jarvis end to end against a real Postgres (`HERMIT_TEST_DATABASE_URL`: a server URL; TLS, as aviary-pg
//! requires; each test makes its own database), aviary's FakeMagpie and a stand-in router: a machine boots its
//! sandbox, the descriptor carries the boot, the supervisor's health makes it ready, magpie's get-or-start answers it,
//! Jarvis's delivery becomes magpie's turn, a fixed-name call gets the identity headers, a pause ends the sandbox, and
//! the next turn resumes the session and adopts its new machine's boot.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aviary_fakes::magpie::FakeMagpie;
use aviary_fakes::script::Reply;
use axum::Json;
use axum::routing::{get, post};
use jarvis_aviary::app::{App, Inner, Keys};
use jarvis_aviary::jarvis::Jarvis;
use jarvis_aviary::wire::{BootReply, BootRequest, TOKEN_HEADER};
use serde_json::{Value, json};
use uuid::Uuid;

const OWNER: &str = "owner@example.com";
const KEYS: &str = "1:BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc";

async fn fresh_db() -> String {
    let url = std::env::var("HERMIT_TEST_DATABASE_URL")
        .expect("HERMIT_TEST_DATABASE_URL (a TLS Postgres server)");
    let name = format!("hj_{}", Uuid::new_v4().simple());
    let admin = aviary_pg::connect(&url, "hermit-jarvis-test", 1, Duration::from_secs(10)).unwrap();
    admin
        .get()
        .await
        .unwrap()
        .batch_execute(&format!("create database {name}"))
        .await
        .unwrap();
    let (base, query) = url.split_once('?').unwrap_or((&url, ""));
    let base = base.rsplit_once('/').map_or(base, |(b, _)| b);
    format!("{base}/{name}?{query}")
}

/// The router's session states, as the stand-in answers them.
#[derive(Default)]
struct Router {
    state: Mutex<String>,
    resumes: Mutex<Vec<String>>,
}

async fn serve(router: axum::Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    url
}

#[tokio::test]
async fn a_session_boots_turns_pauses_and_resumes() {
    let magpie = FakeMagpie::unguarded().serve().await.unwrap();
    let cid = Uuid::new_v4();
    let conv = json!({"conversation": {"id": cid, "title": "", "titleSource": "provisional",
        "createdAt": "2026-11-11T03:00:00.000Z", "updatedAt": "2026-11-11T03:00:00.000Z", "lastUserMessageAt": null,
        "deletedAt": null, "purgeAt": null, "claudeSessionId": null, "turn": null, "lastTurn": null, "sandbox": null,
        "asks": [], "run": {"state": "idle", "background": []}, "model": null, "contextTokens": null,
        "origin": {"kind": "new"}}, "turn": null});
    magpie
        .fake
        .script()
        .always("hermit_create_conversation", Reply::json(201, conv));
    magpie.fake.script().once(
        "hermit_send_turn",
        Reply::json(
            202,
            json!({"turn": {"id": Uuid::now_v7(), "startedAt": "2026-11-11T03:00:01.000Z"}}),
        ),
    );
    magpie.fake.script().once(
        "hermit_send_turn",
        Reply::error(409, "turn_running", "a turn is running"),
    );
    magpie.fake.script().always(
        "cc_tree",
        Reply::json(
            200,
            json!({"files": [], "ccBytes": 0, "quotaBytes": 1048576}),
        ),
    );

    let jr = Arc::new(Router::default());
    *jr.state.lock().unwrap() = "started".into();
    let jr2 = jr.clone();
    let jr3 = jr.clone();
    let jr4 = jr.clone();
    let router_url = serve(
        axum::Router::new()
            .route(
                "/api/machine-signatures/verify",
                post(move |Json(v): Json<Value>| {
                    let jr = jr2.clone();
                    async move {
                        // the stand-in knows machines m1 and m2 of session s1; it checks nothing else
                        if matches!(v["machine"].as_str(), Some("m1" | "m2")) && v["path"] == "/jarvis/boot" {
                            let state = jr.state.lock().unwrap().clone();
                            (axum::http::StatusCode::OK, Json(json!({"session": "s1", "harness": "aviary", "state": state})))
                        } else {
                            (axum::http::StatusCode::FORBIDDEN, Json(json!({"error": "no"})))
                        }
                    }
                }),
            )
            .route(
                "/api/remotes",
                get(move || {
                    let jr = jr3.clone();
                    async move {
                        let state = jr.state.lock().unwrap().clone();
                        Json(json!({"sessions": [{"id": "s1", "state": state, "model": "aviary/claude-opus-5-5"}]}))
                    }
                }),
            )
            .route(
                "/api/sessions/{id}/start",
                post(move |axum::extract::Path(id): axum::extract::Path<String>| {
                    let jr = jr4.clone();
                    async move {
                        jr.resumes.lock().unwrap().push(id);
                        *jr.state.lock().unwrap() = "resuming".into();
                        (axum::http::StatusCode::ACCEPTED, Json(json!({"ok": true})))
                    }
                }),
            ),
    )
    .await;

    let pool = jarvis_aviary::db::open(&fresh_db().await).await.unwrap();
    let http = jarvis_aviary::central::http().unwrap();
    let hermit_l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hermit_url = format!("http://{}", hermit_l.local_addr().unwrap());
    let app = App(Arc::new(Inner {
        pool,
        keys: Keys::parse(KEYS).unwrap(),
        magpie: magpie_api::client::Client::new(http.clone(), magpie.url(), "t-hermit-magpie"),
        jarvis: Jarvis::new(&router_url, "id", "secret").unwrap(),
        owner: OWNER.into(),
        access_team: "example.cloudflareaccess.com".into(),
        access_aud: "aud".into(),
        magpie_url: magpie.url(),
        hermit_url: hermit_url.clone(),
        gateway_to_magpie: "t-gw-magpie".into(),
        gateway_to_hermit: "t-gw-hermit".into(),
        http: http.clone(),
    }));
    let hermit_router = hermit_api::server::router(Arc::new(app.clone()));
    tokio::spawn(async move { axum::serve(hermit_l, hermit_router).await.unwrap() });
    let gw = serve(jarvis_aviary::central::router(app.clone())).await;
    let c = jarvis_aviary::central::http().unwrap();

    // 1. the machine boots (signed: the stand-in router accepts m1's signature)
    let boot = |machine: &'static str| {
        let c = c.clone();
        let gw = gw.clone();
        async move {
            let body = serde_json::to_vec(&BootRequest {
                session: "s1".into(),
                image: format!("sha256:{}", "a".repeat(64)),
                memory_bytes: 8 << 30,
                cpus: 4,
                env: [
                    ("SESSION_ID".into(), "s1".into()),
                    ("SECRET_THING".into(), "never".into()),
                ]
                .into_iter()
                .collect(),
            })
            .unwrap();
            c.post(format!("{gw}/jarvis/boot"))
                .header("x-machine", machine)
                .header("x-time", "0")
                .header("x-sig", "sig")
                .body(body)
                .send()
                .await
                .unwrap()
        }
    };
    let r = boot("m1").await;
    assert_eq!(r.status(), 200, "{:?}", r.text().await);
    let b: BootReply = r.json().await.unwrap();
    assert_eq!(b.conversation_id, cid);
    assert!(b.token.starts_with("hst1."));
    // an unknown machine is refused
    assert_eq!(boot("mX").await.status(), 403);
    // the same machine again keeps its sandbox
    let again: BootReply = boot("m1").await.json().await.unwrap();
    assert_eq!(again.sandbox_id, b.sandbox_id);

    // 2. the descriptor, through the gateway listener as the supervisor reads it
    let d: Value = c
        .get(format!("{gw}/hermit/v1/sandbox/self"))
        .header(TOKEN_HEADER, &b.token)
        .header("aviary-protocol", "hermit-sandbox=1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(d["claude"]["projectSettings"], true, "{d}");
    assert_eq!(d["env"]["SESSION_ID"], "s1");
    assert!(
        d["env"].get("SECRET_THING").is_none(),
        "only Jarvis's own names pass"
    );
    assert!(
        d["env"]["HTTPS_PROXY"]
            .as_str()
            .unwrap()
            .contains("@127.0.0.1:3128")
    );
    assert_eq!(d["resources"]["memoryBytes"], 8u64 << 30);
    // no token, or a forged one: refused
    let r = c
        .get(format!("{gw}/hermit/v1/sandbox/self"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = c
        .get(format!("{gw}/hermit/v1/sandbox/self"))
        .header(TOKEN_HEADER, "hst1.1.AAAA.BBBB")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // 3. starting until the supervisor says it is ready
    let get_sandbox = || {
        let c = c.clone();
        let u = format!("{hermit_url}/v1/conversations/{cid}/sandbox");
        async move {
            c.get(u)
                .header("aviary-protocol", "hermit-core=1")
                .send()
                .await
                .unwrap()
        }
    };
    let s: Value = get_sandbox().await.json().await.unwrap();
    assert_eq!(s["state"], "starting");
    let health = json!({"ok": true, "ready": true, "protocol": 1, "version": "abc", "bootId": "b1", "busy": false});
    let r = c
        .post(format!("{gw}/jarvis/health"))
        .header(TOKEN_HEADER, &b.token)
        .json(&health)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let put = c
        .put(format!("{hermit_url}/v1/conversations/{cid}/sandbox"))
        .header("aviary-protocol", "hermit-core=1")
        .json(&json!({"person": OWNER}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);
    let s: Value = put.json().await.unwrap();
    assert_eq!(s["state"], "ready");
    assert_eq!(s["id"], b.sandbox_id.to_string());
    // another person is refused
    let put = c
        .put(format!("{hermit_url}/v1/conversations/{cid}/sandbox"))
        .header("aviary-protocol", "hermit-core=1")
        .json(&json!({"person": "else@example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 409);

    // 4. Jarvis's delivery is magpie's turn as the owner; a second while one runs is 409
    let r = c
        .post(format!("{gw}/jarvis/deliver"))
        .header(TOKEN_HEADER, &b.token)
        .json(&json!({"id": "jarvis-1", "text": "a wakeup"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 202);
    let sent = magpie.fake.script().calls_to("hermit_send_turn");
    assert_eq!(sent[0].param("id"), Some(cid.to_string().as_str()));
    assert_eq!(sent[0].body.as_ref().unwrap()["person"], OWNER);
    assert_eq!(
        sent[0].body.as_ref().unwrap()["message"]["clientMessageId"],
        "jarvis-1"
    );
    let r = c
        .post(format!("{gw}/jarvis/deliver"))
        .header(TOKEN_HEADER, &b.token)
        .json(&json!({"id": "jarvis-2", "text": "another"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);

    // 5. a fixed-name call: the identity headers and the gateway's bearer replace whatever the sandbox sent
    let r = c
        .get(format!("{gw}/magpie/v1/sandbox/cc/tree"))
        .header(TOKEN_HEADER, &b.token)
        .header("aviary-protocol", "magpie-sandbox=1")
        .header("aviary-sandbox-person", "forged@example.com")
        .header("authorization", "Bearer sandbox")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let hb = magpie.fake.script().calls_to("cc_tree");
    let call = hb.last().unwrap();
    assert_eq!(call.header("aviary-sandbox-person"), Some(OWNER));
    assert_eq!(
        call.header("aviary-sandbox-id"),
        Some(b.sandbox_id.to_string().as_str())
    );
    assert_eq!(call.header("authorization"), Some("Bearer t-gw-magpie"));
    // only the contract's paths
    let r = c
        .get(format!("{gw}/magpie/v1/conversations"))
        .header(TOKEN_HEADER, &b.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // 6. Jarvis pauses the session: the next pass ends the sandbox (idle) and its token no longer works
    *jr.state.lock().unwrap() = "paused".into();
    jarvis_aviary::reconcile::pass(&app).await.unwrap();
    let s: Value = get_sandbox().await.json().await.unwrap();
    assert_eq!(s["state"], "gone");
    assert_eq!(s["release"]["reason"], "idle");
    let r = c
        .post(format!("{gw}/jarvis/health"))
        .header(TOKEN_HEADER, &b.token)
        .json(&health)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // 7. a turn for a paused session resumes it, and its new machine's boot is that sandbox
    let put = c
        .put(format!("{hermit_url}/v1/conversations/{cid}/sandbox"))
        .header("aviary-protocol", "hermit-core=1")
        .json(&json!({"person": OWNER}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 202);
    let waiting: Value = put.json().await.unwrap();
    assert_eq!(waiting["state"], "starting");
    for _ in 0..50 {
        if !jr.resumes.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(*jr.resumes.lock().unwrap(), vec!["s1".to_owned()]);
    // a reconcile pass while the resume is in flight leaves it alone
    jarvis_aviary::reconcile::pass(&app).await.unwrap();
    *jr.state.lock().unwrap() = "started".into();
    let b2: BootReply = boot("m2").await.json().await.unwrap();
    assert_eq!(b2.sandbox_id.to_string(), waiting["id"].as_str().unwrap());
    assert_ne!(b2.sandbox_id, b.sandbox_id);

    // 8. the change feed saw every step, in order
    let feed: Value = c
        .get(format!("{hermit_url}/v1/sandbox-events?after=0&limit=100"))
        .header("aviary-protocol", "hermit-core=1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let states: Vec<&str> = feed["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["state"].as_str().unwrap())
        .collect();
    assert!(
        states.contains(&"ready") && states.contains(&"gone"),
        "{states:?}"
    );
    assert_eq!(*states.last().unwrap(), "starting");
}
