//! herald's API against a fake Telegram: who may call, which conversation owns a channel, the limits.
#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)

use std::sync::{Arc, Mutex};

use aviary_core::auth::{Caller, Callers};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use herald::app::{AppState, router};
use herald::handle::Key;
use herald::telegram::{Attachment, BoxFuture, Chunks, Created, Error, Format, Info, Kind, Telegram};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const TOKEN: &str = "gateway-token-for-tests-0123456789";

#[derive(Default)]
struct Fake {
    created: Mutex<Vec<(String, String)>>,
    posts: Mutex<Vec<(i64, String, Format, bool)>>,
    flood: Mutex<Option<u64>>,
    /// Files whose body arrived whole: what was asked and the bytes.
    files: Mutex<Vec<(i64, Attachment, Vec<u8>)>>,
    /// Bodies that ended in an error (herald broke the upload off): nothing was posted.
    broken: Mutex<usize>,
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

    fn send_file(&self, chat_id: i64, file: Attachment, mut body: Chunks) -> BoxFuture<'_, Result<i64, Error>> {
        Box::pin(async move {
            let mut bytes = Vec::new();
            while let Some(chunk) = body.recv().await {
                match chunk {
                    Ok(c) => bytes.extend_from_slice(&c),
                    Err(e) => {
                        *self.broken.lock().unwrap() += 1;
                        return Err(Error::Unavailable(e.to_string()));
                    }
                }
            }
            let mut f = self.files.lock().unwrap();
            f.push((chat_id, file, bytes));
            Ok(100 + i64::try_from(f.len()).unwrap())
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
        ("POST", "/v1/channels/x/files"),
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

const BOUNDARY: &str = "XyZ-test-boundary-42";

/// A multipart body: text fields and file parts (`name`, file name, content type, bytes), in order.
enum Part<'a> {
    Text(&'a str, &'a str),
    File(&'a str, &'a str, &'a str, &'a [u8]),
}

fn multipart(parts: &[Part<'_>]) -> Vec<u8> {
    let mut b = Vec::new();
    for p in parts {
        b.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match p {
            Part::Text(name, value) => {
                b.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}").as_bytes(),
                );
            }
            Part::File(name, file, ct, bytes) => {
                b.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\nContent-Type: {ct}\r\n\r\n")
                        .as_bytes(),
                );
                b.extend_from_slice(bytes);
            }
        }
        b.extend_from_slice(b"\r\n");
    }
    b.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    b
}

async fn upload(app: &axum::Router, channel: &str, conv: &str, body: Vec<u8>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/channels/{channel}/files"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("aviary-sandbox-conversation", conv)
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn channel_for(app: &axum::Router, conv: &str) -> String {
    let (_, v) = call(app, "POST", "/v1/channels", Some(conv), Some(json!({"title": "files"}))).await;
    v["channel"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_file_is_posted_whole_with_its_caption() {
    let fake = Arc::new(Fake::default());
    let app = app(&fake);
    let ch = channel_for(&app, "conv-a").await;
    // larger than the JSON routes' 64 KiB limit, and bytes that are not UTF-8
    let pdf: Vec<u8> = (0..300_000u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
    let (s, v) = upload(
        &app,
        &ch,
        "conv-a",
        multipart(&[
            Part::Text("caption", "<b>每日報告</b>"),
            Part::Text("format", "html"),
            Part::Text("silent", "true"),
            Part::File("file", "報告 2026-11-11.pdf", "application/pdf", &pdf),
        ]),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["messageId"], 101);
    let files = fake.files.lock().unwrap();
    let (chat, att, bytes) = &files[0];
    assert_eq!(*chat, -1_000_000_000_001);
    assert_eq!(att.kind, Kind::Document);
    assert_eq!(att.name, "報告 2026-11-11.pdf");
    assert_eq!(att.content_type, "application/pdf");
    assert_eq!(att.caption.as_deref(), Some("<b>每日報告</b>"));
    assert_eq!(att.format, Format::Html);
    assert!(att.silent);
    assert_eq!(bytes, &pdf);
}

#[tokio::test]
async fn an_image_can_go_as_a_photo() {
    let fake = Arc::new(Fake::default());
    let app = app(&fake);
    let ch = channel_for(&app, "c").await;
    let (s, v) = upload(
        &app,
        &ch,
        "c",
        multipart(&[
            Part::Text("as", "photo"),
            Part::File("file", "chart.png", "image/png", b"\x89PNG"),
        ]),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let files = fake.files.lock().unwrap();
    assert_eq!(files[0].1.kind, Kind::Photo);
    assert_eq!(files[0].1.caption, None);
    assert_eq!(files[0].1.format, Format::Plain);
    assert!(!files[0].1.silent);
}

#[tokio::test]
async fn file_names_cannot_break_the_upload() {
    let fake = Arc::new(Fake::default());
    let app = app(&fake);
    let ch = channel_for(&app, "c").await;
    let (s, _) = upload(
        &app,
        &ch,
        "c",
        multipart(&[Part::File("file", "../a\\b\"c.txt", "text/plain", b"x")]),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let name = fake.files.lock().unwrap()[0].1.name.clone();
    assert!(!name.contains(['/', '\\', '"']), "{name}");
}

#[tokio::test]
async fn bad_uploads_are_refused_and_nothing_is_posted() {
    let fake = Arc::new(Fake::default());
    let app = app(&fake);
    let ch = channel_for(&app, "c").await;
    let file = Part::File("file", "a.txt", "text/plain", b"hello");
    for (why, body, want) in [
        (
            "no file",
            multipart(&[Part::Text("caption", "x")]),
            StatusCode::BAD_REQUEST,
        ),
        (
            "unknown field",
            multipart(&[
                Part::Text("owner", "me"),
                Part::File("file", "a.txt", "text/plain", b"x"),
            ]),
            StatusCode::BAD_REQUEST,
        ),
        (
            "bad format",
            multipart(&[
                Part::Text("format", "markdown"),
                Part::File("file", "a.txt", "text/plain", b"x"),
            ]),
            StatusCode::BAD_REQUEST,
        ),
        (
            "bad as",
            multipart(&[
                Part::Text("as", "sticker"),
                Part::File("file", "a.txt", "text/plain", b"x"),
            ]),
            StatusCode::BAD_REQUEST,
        ),
        (
            "long caption",
            multipart(&[
                Part::Text("caption", &"字".repeat(1025)),
                Part::File("file", "a.txt", "text/plain", b"x"),
            ]),
            StatusCode::BAD_REQUEST,
        ),
        (
            "empty file",
            multipart(&[Part::File("file", "a.txt", "text/plain", b"")]),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (s, v) = upload(&app, &ch, "c", body).await;
        assert_eq!(s, want, "{why}: {v}");
        assert!(v["message"].is_string(), "{why}: {v}");
    }
    // a field after the file: the upload to Telegram is broken off, so nothing is posted
    let (s, v) = upload(&app, &ch, "c", multipart(&[file, Part::Text("caption", "late")])).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["message"].as_str().unwrap().contains("before"), "{v}");
    // two files
    let (s, _) = upload(
        &app,
        &ch,
        "c",
        multipart(&[
            Part::File("file", "a.txt", "text/plain", b"a"),
            Part::File("file", "b.txt", "text/plain", b"b"),
        ]),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // a photo over Telegram's 10 MB
    let big = vec![0u8; 10 * 1024 * 1024 + 1];
    let (s, v) = upload(
        &app,
        &ch,
        "c",
        multipart(&[
            Part::Text("as", "photo"),
            Part::File("file", "big.jpg", "image/jpeg", &big),
        ]),
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    // another conversation's channel
    let (s, _) = upload(
        &app,
        &ch,
        "other",
        multipart(&[Part::File("file", "a.txt", "text/plain", b"x")]),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(fake.files.lock().unwrap().is_empty());
    assert_eq!(*fake.broken.lock().unwrap(), 4); // the empty file, the late field, two files, the big photo
}

#[tokio::test]
async fn a_document_over_50_mb_is_refused() {
    let fake = Arc::new(Fake::default());
    let app = app(&fake);
    let ch = channel_for(&app, "c").await;
    let big = vec![7u8; 50 * 1024 * 1024 + 1];
    let (s, v) = upload(
        &app,
        &ch,
        "c",
        multipart(&[Part::File("file", "big.bin", "application/octet-stream", &big)]),
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert!(fake.files.lock().unwrap().is_empty());
}
