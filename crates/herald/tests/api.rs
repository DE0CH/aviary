//! herald's API against a fake Telegram: who may call, which conversation owns a channel, the limits.
#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)

use std::sync::{Arc, Mutex};

use aviary_core::auth::{Caller, Callers};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use herald::app::{AppState, router};
use herald::handle::Key;
use herald::telegram::{BoxFuture, Created, Error, Format, Info, Telegram};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const TOKEN: &str = "gateway-token-for-tests-0123456789";

#[derive(Default)]
struct Fake {
    created: Mutex<Vec<(String, String)>>,
    posts: Mutex<Vec<(i64, String, Format, bool)>>,
    flood: Mutex<Option<u64>>,
}

impl Telegram for Fake {
    fn create_channel<'a>(&'a self, title: &'a str, about: &'a str) -> BoxFuture<'a, Result<Created, Error>> {
        Box::pin(async move {
            if let Some(s) = *self.flood.lock().unwrap() {
                return Err(Error::FloodWait(s));
            }
            let mut c = self.created.lock().unwrap();
            c.push((title.to_owned(), about.to_owned()));
            Ok(Created {
                chat_id: -1_000_000_000_000 - i64::try_from(c.len()).unwrap(),
                invite_link: format!("https://t.me/+fake{}", c.len()),
            })
        })
    }

    fn channel(&self, chat_id: i64) -> BoxFuture<'_, Result<Info, Error>> {
        Box::pin(async move {
            if chat_id == -1_000_000_000_099 {
                return Err(Error::NoChannel);
            }
            Ok(Info {
                title: "YYC AI · t".to_owned(),
                invite_link: Some("https://t.me/+fake1".to_owned()),
            })
        })
    }

    fn post<'a>(
        &'a self,
        chat_id: i64,
        text: &'a str,
        format: Format,
        silent: bool,
    ) -> BoxFuture<'a, Result<i64, Error>> {
        Box::pin(async move {
            let mut p = self.posts.lock().unwrap();
            p.push((chat_id, text.to_owned(), format, silent));
            Ok(i64::try_from(p.len()).unwrap())
        })
    }
}

fn app(fake: &Arc<Fake>) -> axum::Router {
    let callers = Callers::new([(Caller::Gateway, vec![TOKEN.to_owned()])]).unwrap();
    let state = AppState {
        telegram: Arc::clone(fake) as Arc<dyn Telegram>,
        key: Key::new("handle-key-for-tests-0123456789abcdef"),
        title_prefix: "YYC AI · ".to_owned(),
    };
    router(state, &callers)
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    conv: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"));
    if let Some(c) = conv {
        req = req.header("aviary-sandbox-conversation", c);
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn every_route_needs_the_gateway_token() {
    let fake = Arc::new(Fake::default());
    let app = app(&fake);
    for (m, p) in [
        ("POST", "/v1/channels"),
        ("GET", "/v1/channels/x"),
        ("POST", "/v1/channels/x/messages"),
    ] {
        let req = Request::builder().method(m).uri(p).body(Body::empty()).unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{m} {p}");
    }
}

#[tokio::test]
async fn a_conversation_creates_a_channel_and_only_it_can_post() {
    let fake = Arc::new(Fake::default());
    let app = app(&fake);
    let (s, v) = call(
        &app,
        "POST",
        "/v1/channels",
        Some("conv-a"),
        Some(json!({"title": "每日新聞", "description": "早報"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["title"], "YYC AI · 每日新聞");
    assert_eq!(v["inviteLink"], "https://t.me/+fake1");
    assert_eq!(
        fake.created.lock().unwrap()[0],
        ("YYC AI · 每日新聞".to_owned(), "早報".to_owned())
    );
    let ch = v["channel"].as_str().unwrap().to_owned();

    let (s, v) = call(
        &app,
        "POST",
        &format!("/v1/channels/{ch}/messages"),
        Some("conv-a"),
        Some(json!({"text": "<b>hi</b>", "format": "html", "silent": true})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["messageId"], 1);
    assert_eq!(
        fake.posts.lock().unwrap()[0],
        (-1_000_000_000_001, "<b>hi</b>".to_owned(), Format::Html, true)
    );

    let (s, _) = call(&app, "GET", &format!("/v1/channels/{ch}"), Some("conv-a"), None).await;
    assert_eq!(s, StatusCode::OK);

    // another conversation, a forged handle, no conversation at all
    let (s, v) = call(
        &app,
        "POST",
        &format!("/v1/channels/{ch}/messages"),
        Some("conv-b"),
        Some(json!({"text": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    let (s, _) = call(
        &app,
        "GET",
        "/v1/channels/-1000000000001.AAAAAAAAAAAAAAAAAAAAAA",
        Some("conv-a"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        "POST",
        &format!("/v1/channels/{ch}/messages"),
        None,
        Some(json!({"text": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(fake.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn limits_and_telegram_refusals_answer_clearly() {
    let fake = Arc::new(Fake::default());
    let app = app(&fake);
    let (s, _) = call(&app, "POST", "/v1/channels", Some("c"), Some(json!({"title": "  "}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        "POST",
        "/v1/channels",
        Some("c"),
        Some(json!({"title": "x".repeat(120)})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &app,
        "POST",
        "/v1/channels",
        Some("c"),
        Some(json!({"title": "t", "owner": "me"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, v) = call(&app, "POST", "/v1/channels", Some("c"), Some(json!({"title": "t"}))).await;
    let ch = v["channel"].as_str().unwrap().to_owned();
    let path = format!("/v1/channels/{ch}/messages");
    let (s, v) = call(&app, "POST", &path, Some("c"), Some(json!({"text": "字".repeat(4097)}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["message"].as_str().unwrap().contains("4096"), "{v}");
    let (s, _) = call(&app, "POST", &path, Some("c"), Some(json!({"text": "字".repeat(4096)}))).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = call(
        &app,
        "POST",
        &path,
        Some("c"),
        Some(json!({"text": "x", "format": "markdown"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    *fake.flood.lock().unwrap() = Some(42);
    let (s, _) = call(&app, "POST", "/v1/channels", Some("c"), Some(json!({"title": "t2"}))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}
