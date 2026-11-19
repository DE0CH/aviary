//! The Claude account in the chat's model menu (Deyao, 2026-11-18: "Opus 5.5 · chendeyao000 or Opus 5.5 ·
//! chendeyao-uk"), hermit-jarvis's half, against a real Postgres (`HERMIT_TEST_DATABASE_URL`), aviary's FakeMagpie and
//! a stand-in router: a machine whose gateway switches accounts boots on a menu with one entry per model and account,
//! its session's account first, and its conversation starts on the session's model on that account; a machine from
//! before gets the file's menu; a model call naming an account switches the session only when the conversation's
//! pick names it.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aviary_fakes::magpie::FakeMagpie;
use aviary_fakes::script::Reply;
use axum::Json;
use axum::routing::{get, post};
use jarvis_aviary::app::{App, Inner, Keys};
use jarvis_aviary::jarvis::Jarvis;
use jarvis_aviary::wire::{AccountReply, BootReply, BootRequest, TOKEN_HEADER};
use serde_json::{Value, json};
use uuid::Uuid;

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

async fn serve(router: axum::Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    url
}

/// The stand-in router: session s1's account, and each switch it was asked for.
#[derive(Default)]
struct Router {
    account: Mutex<String>,
    switches: Mutex<Vec<(String, Value)>>,
}

fn conversation(cid: Uuid, picked: Option<&str>) -> Value {
    json!({"id": cid, "title": "", "titleSource": "provisional",
        "createdAt": "2026-11-18T03:00:00.000Z", "updatedAt": "2026-11-18T03:00:00.000Z", "lastUserMessageAt": null,
        "deletedAt": null, "purgeAt": null, "claudeSessionId": null, "turn": null, "lastTurn": null, "sandbox": null,
        "asks": [], "run": {"state": "idle", "background": []}, "model": null, "contextTokens": null,
        "origin": {"kind": "new"}, "pickedModel": picked})
}

#[tokio::test]
async fn the_menu_offers_each_account_and_a_pick_switches_the_session() {
    let magpie = FakeMagpie::unguarded().serve().await.unwrap();
    let cid = Uuid::new_v4();
    magpie.fake.script().always(
        "hermit_create_conversation",
        Reply::json(
            201,
            json!({"conversation": conversation(cid, None), "turn": null}),
        ),
    );

    let jr = Arc::new(Router::default());
    *jr.account.lock().unwrap() = "claude-chendeyao000".into();
    let (jr2, jr3) = (jr.clone(), jr.clone());
    let router_url = serve(
        axum::Router::new()
            .route(
                "/api/machine-signatures/verify",
                post(move |Json(v): Json<Value>| async move {
                    if matches!(v["machine"].as_str(), Some("m1" | "m2")) {
                        (axum::http::StatusCode::OK, Json(json!({"session": "s1", "harness": "aviary", "state": "started"})))
                    } else {
                        (axum::http::StatusCode::FORBIDDEN, Json(json!({"error": "no"})))
                    }
                }),
            )
            .route(
                "/api/remotes",
                get(move || {
                    let jr = jr2.clone();
                    async move {
                        let account = jr.account.lock().unwrap().clone();
                        Json(json!({
                            "sessions": [{"id": "s1", "state": "started", "model": "aviary/claude-opus-5-5", "account": account}],
                            "accounts": [
                                {"store": "claude-chendeyao000", "name": "chendeyao000", "default": true},
                                {"store": "claude-chendeyao-uk", "name": "chendeyao-uk", "default": false},
                            ],
                        }))
                    }
                }),
            )
            .route(
                "/api/remotes/{id}/account",
                post(
                    move |axum::extract::Path(id): axum::extract::Path<String>, Json(b): Json<Value>| {
                        let jr = jr3.clone();
                        async move {
                            jr.switches.lock().unwrap().push((id, b.clone()));
                            let to = format!("claude-{}", b["account"].as_str().unwrap());
                            let mut on = jr.account.lock().unwrap();
                            if *on == to {
                                return (axum::http::StatusCode::OK, Json(json!({"ok": true, "unchanged": true})));
                            }
                            *on = to;
                            (axum::http::StatusCode::ACCEPTED, Json(json!({"started": true, "kind": "account"})))
                        }
                    },
                ),
            ),
    )
    .await;

    let pool = jarvis_aviary::db::open(&fresh_db().await).await.unwrap();
    let http = jarvis_aviary::central::http().unwrap();
    let app = App(Arc::new(Inner {
        pool,
        keys: Keys::parse(KEYS).unwrap(),
        magpie: magpie_api::client::Client::new(http.clone(), magpie.url(), "t-hermit-magpie"),
        jarvis: Jarvis::new(&router_url, "id", "secret").unwrap(),
        owner: "owner@example.com".into(),
        access_team: "example.cloudflareaccess.com".into(),
        access_aud: "aud".into(),
        magpie_url: magpie.url(),
        hermit_url: "http://127.0.0.1:1".into(),
        gateway_to_magpie: "t-gw-magpie".into(),
        gateway_to_hermit: "t-gw-hermit".into(),
        http: http.clone(),
        settings: jarvis_aviary::settings::Settings::parse(include_str!("settings.json")).unwrap(),
        fabric: None,
        chat_schedules: std::sync::Mutex::default(),
        listing: tokio::sync::Mutex::default(),
    }));
    let gw = serve(jarvis_aviary::central::router(app.clone())).await;
    let c = jarvis_aviary::central::http().unwrap();
    let boot = |machine: &'static str, accounts: bool| {
        let (c, gw) = (c.clone(), gw.clone());
        async move {
            let body = serde_json::to_vec(&BootRequest {
                session: "s1".into(),
                image: format!("sha256:{}", "a".repeat(64)),
                memory_bytes: 8 << 30,
                cpus: 4,
                env: [
                    ("SESSION_ID".into(), "s1".into()),
                    ("SESSION_MODEL".into(), "aviary/claude-opus-5-5".into()),
                ]
                .into_iter()
                .collect(),
                accounts,
            })
            .unwrap();
            let r = c
                .post(format!("{gw}/jarvis/boot"))
                .header("x-machine", machine)
                .header("x-time", "0")
                .header("x-sig", "sig")
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            r.json::<BootReply>().await.unwrap()
        }
    };

    // 1. a gateway that switches accounts: one entry per model and account, the session's account first
    let b = boot("m1", true).await;
    assert_eq!(b.account.as_deref(), Some("chendeyao000"));
    let menu = b.settings.as_ref().unwrap();
    let ids: Vec<&str> = menu.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "claude-opus-5-5+chendeyao000",
            "claude-opus-5-5+chendeyao-uk",
            "claude-fable-5-2+chendeyao000",
            "claude-fable-5-2+chendeyao-uk",
            "z-ai/glm-5.3",
        ]
    );
    assert_eq!(menu.models[1].label, "Opus 5.5 · chendeyao-uk");
    assert_eq!(
        b.reasoning
            .get("claude-opus-5-5+chendeyao-uk")
            .map(String::as_str),
        Some("high")
    );
    // the conversation starts on the session's model, on the session's account
    let made = magpie.fake.script().calls_to("hermit_create_conversation");
    assert_eq!(
        made[0].body.as_ref().unwrap()["model"],
        "claude-opus-5-5+chendeyao000"
    );

    let ask = |token: String, account: &'static str| {
        let (c, gw) = (c.clone(), gw.clone());
        async move {
            let r = c
                .post(format!("{gw}/jarvis/account"))
                .header(TOKEN_HEADER, token)
                .json(&json!({"account": account}))
                .send()
                .await
                .unwrap();
            let status = r.status().as_u16();
            (status, r.json::<Value>().await.unwrap_or_default())
        }
    };
    let picked = |model: Option<&'static str>| {
        magpie.fake.script().once(
            "hermit_conversation",
            Reply::json(200, conversation(cid, model)),
        );
    };

    // 2. Deyao picked "Opus 5.5 · chendeyao-uk": the call naming it switches the session
    picked(Some("claude-opus-5-5+chendeyao-uk"));
    let (status, r) = ask(b.token.clone(), "chendeyao-uk").await;
    assert_eq!(status, 200, "{r}");
    assert_eq!(
        serde_json::from_value::<AccountReply>(r).unwrap(),
        AccountReply { switched: true }
    );
    assert_eq!(
        *jr.switches.lock().unwrap(),
        [("s1".to_owned(), json!({"account": "chendeyao-uk"}))]
    );

    // 3. the reply that began before the pick goes on, on the account the session is on: nothing switched...
    picked(Some("claude-opus-5-5+chendeyao000"));
    let (status, r) = ask(b.token.clone(), "chendeyao-uk").await;
    assert_eq!((status, r["switched"].clone()), (200, json!(false)));
    // ...but an account neither picked nor the session's is never switched to: a sandbox gets no login by naming it
    picked(Some("claude-opus-5-5+chendeyao-uk"));
    let (status, r) = ask(b.token.clone(), "chendeyao000").await;
    assert_eq!(status, 403, "{r}");
    assert_eq!(jr.switches.lock().unwrap().len(), 1);

    // 4. a pick that names no account (a conversation from before) runs on the account the session has
    picked(Some("claude-opus-5-5"));
    let (status, r) = ask(b.token.clone(), "chendeyao000").await;
    assert_eq!((status, r["switched"].clone()), (200, json!(false)));
    assert_eq!(jr.switches.lock().unwrap().len(), 1);

    // 5. no sandbox token, no switch
    let r = c
        .post(format!("{gw}/jarvis/account"))
        .json(&json!({"account": "chendeyao-uk"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // 6. a gateway from before says nothing of accounts: the file's menu, whose ids Anthropic knows
    let old = boot("m2", false).await;
    assert_eq!(old.account, None);
    let ids: Vec<&str> = old
        .settings
        .as_ref()
        .unwrap()
        .models
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ids, ["claude-opus-5-5", "claude-fable-5-2", "z-ai/glm-5.3"]);
    // and it never sends a member it does not know
    let sent = serde_json::to_value(&BootRequest {
        session: "s1".into(),
        image: String::new(),
        memory_bytes: 0,
        cpus: 0,
        env: Default::default(),
        accounts: false,
    })
    .unwrap();
    assert!(sent.get("accounts").is_none(), "{sent}");
}
