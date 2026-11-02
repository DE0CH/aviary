//! The byte-relay calls (`files_raw_relay`, `share_files_raw_relay`, `blob_relay`): they forward exactly the
//! range and conditional headers of [`magpie_api::client::RELAY_REQUEST_HEADERS`], keep the scope's identity
//! headers, and hand back every status (206, 304, 404, 416) for the caller to pass on.
#![allow(clippy::unwrap_used)] // tests

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use magpie_api::client::Client;
use magpie_api::files::RawFileQuery;
use magpie_api::uploads::{BlobQuery, BlobVariant};
use uuid::Uuid;

/// Echoes the headers it got as the body; the status comes from `x-want-status`.
async fn echo(h: HeaderMap) -> impl IntoResponse {
    let status = h
        .get("x-want-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u16>().ok())
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::OK);
    let mut seen: Vec<String> = h
        .iter()
        .filter(|(n, _)| n.as_str() != "host" && n.as_str() != "x-want-status")
        .map(|(n, v)| format!("{n}={}", v.to_str().unwrap_or("?")))
        .collect();
    seen.sort();
    (status, seen.join("\n"))
}

async fn client() -> Client {
    let app = Router::new()
        .route(magpie_api::routes::FILES_RAW, get(echo))
        .route(magpie_api::routes::SHARE_FILES_RAW, get(echo))
        .route(magpie_api::routes::BLOB, get(echo));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::new(reqwest::Client::new(), format!("http://{addr}"), "tok")
}

fn forward(status: u16) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("range", "bytes=0-9".parse().unwrap());
    h.insert("if-none-match", "\"e1\"".parse().unwrap());
    h.insert("if-modified-since", "Sat, 01 Nov 2026 00:00:00 GMT".parse().unwrap());
    h.insert("if-range", "\"e1\"".parse().unwrap());
    h.insert("cookie", "CF_Authorization=secret".parse().unwrap());
    h.insert("x-want-status", status.to_string().parse().unwrap());
    h
}

fn has(body: &str, line: &str) -> bool {
    body.lines().any(|l| l == line)
}

#[tokio::test]
async fn files_raw_relay_forwards_only_range_and_conditionals_and_keeps_every_status() {
    let c = client().await;
    let q = RawFileQuery {
        path: "out/a.txt".into(),
        sent: Some("toolu_1".into()),
        inline: true,
    };
    // x-want-status is not a relayed header: the echo answers 200, and lists only what was relayed
    let r = c
        .person("a@example.com")
        .files_raw_relay(Uuid::nil(), &q, &forward(304))
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "x-want-status must not be relayed");
    let body = r.text().await.unwrap();
    assert!(has(&body, "range=bytes=0-9"), "{body}");
    assert!(has(&body, "if-none-match=\"e1\""), "{body}");
    assert!(has(&body, "if-modified-since=Sat, 01 Nov 2026 00:00:00 GMT"), "{body}");
    assert!(has(&body, "if-range=\"e1\""), "{body}");
    assert!(has(&body, "aviary-person=a@example.com"), "{body}");
    assert!(has(&body, "authorization=Bearer tok"), "{body}");
    assert!(!body.contains("cookie"), "{body}");
}

#[tokio::test]
async fn relays_return_non_success_statuses_unchanged() {
    // a server that answers 304 / 416 / 404 by path
    let app = Router::new()
        .route(
            magpie_api::routes::FILES_RAW,
            get(|| async { StatusCode::NOT_MODIFIED }),
        )
        .route(
            magpie_api::routes::SHARE_FILES_RAW,
            get(|| async { (StatusCode::RANGE_NOT_SATISFIABLE, [("content-range", "bytes */10")]) }),
        )
        .route(
            magpie_api::routes::BLOB,
            get(|| async {
                (
                    StatusCode::NOT_FOUND,
                    [("content-type", "application/json")],
                    r#"{"code":"not_found","message":"no blob"}"#,
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let _ = rustls::crypto::ring::default_provider().install_default();
    let c = Client::new(reqwest::Client::new(), format!("http://{addr}"), "tok");
    let q = RawFileQuery {
        path: "a".into(),
        sent: None,
        inline: false,
    };
    let none = reqwest::header::HeaderMap::new();
    let r = c
        .person("a@example.com")
        .files_raw_relay(Uuid::nil(), &q, &none)
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 304);
    let r = c
        .viewing_as("a@example.com", "admin@example.com", "s1")
        .share_files_raw_relay("AAAAAAAAAAAAAAAAAAAAAA", &q, &none)
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 416);
    assert_eq!(r.headers()["content-range"], "bytes */10");
    let r = c
        .blob_relay(
            "b1",
            &BlobQuery {
                variant: Some(BlobVariant::Thumb),
            },
            &none,
        )
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    assert!(r.text().await.unwrap().contains("not_found"));
}

#[tokio::test]
async fn share_and_blob_relays_forward_the_same_headers() {
    let c = client().await;
    let q = RawFileQuery {
        path: "a".into(),
        sent: None,
        inline: false,
    };
    let body = c
        .person("a@example.com")
        .share_files_raw_relay("AAAAAAAAAAAAAAAAAAAAAA", &q, &forward(200))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(has(&body, "range=bytes=0-9"), "{body}");
    let body = c
        .blob_relay("b1", &BlobQuery { variant: None }, &forward(200))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(has(&body, "if-none-match=\"e1\""), "{body}");
    assert!(!body.contains("aviary-person"), "M15 carries no person: {body}");
}
