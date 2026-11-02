//! Blobs and the bucket (magpie 5, 15): staged blobs served before the uploader stores them, the uploader through a
//! bucket outage, the GC's rules and the reconcile, M15's variants and headers, the sandbox's blob routes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use magpie::blobs::{self, Blobs, gc, store, uploader::Uploader};
use magpie_api::conversations::CreateRequest;
use magpie_api::turns::SendRequest;
use magpie_api::uploads::{BlobQuery, BlobVariant};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

fn empty(key: &str) -> CreateRequest {
    CreateRequest {
        idempotency_key: key.to_owned(),
        message: SendRequest {
            client_message_id: format!("{key}:m"),
            text: String::new(),
            images: Vec::new(),
            files: Vec::new(),
        },
    }
}

async fn conversation(r: &common::Replica, person: &str, key: &str) -> Uuid {
    r.pigeon()
        .person(person)
        .create_conversation(&empty(key))
        .await
        .unwrap()
        .into_inner()
        .conversation
        .id
}

/// A PNG of `w`×`h` (a gradient; `alpha` adds a transparent corner).
fn png(w: u32, h: u32, alpha: bool) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let a = if alpha && x < w / 4 && y < h / 4 { 0 } else { 255 };
        image::Rgba([
            u8::try_from(x * 255 / w.max(1)).unwrap(),
            u8::try_from(y * 255 / h.max(1)).unwrap(),
            128,
            a,
        ])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    if alpha {
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
    } else {
        image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(img).to_rgb8())
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
    }
    out.into_inner()
}

/// Stages a blob as a cc write does (magpie 5.2) and references it from `conversation`.
async fn stage(
    pool: &aviary_pg::Pool,
    conversation: Uuid,
    bytes: &[u8],
    kind: &str,
    encoding: &str,
    mime: Option<&str>,
) -> [u8; 32] {
    let sha = blobs::sha256(bytes);
    let c = pool.get().await.unwrap();
    c.execute(
        "insert into blobs (sha256, size, kind, encoding, mime, stored) values ($1, $2, $3, $4, $5, false)",
        &[
            &sha.as_slice(),
            &i64::try_from(bytes.len()).unwrap(),
            &kind,
            &encoding,
            &mime,
        ],
    )
    .await
    .unwrap();
    c.execute(
        "insert into blob_staging (sha256, bytes) values ($1, $2)",
        &[&sha.as_slice(), &bytes],
    )
    .await
    .unwrap();
    c.execute(
        "insert into blob_refs (owner, owner_kind, blob) values ($1, 'conversation', $2)",
        &[&conversation, &sha.as_slice()],
    )
    .await
    .unwrap();
    sha
}

/// A gateway call for `conversation`'s sandbox (the identity headers the gateway adds, magpie 3.3).
fn gateway(
    r: &common::Replica,
    method: reqwest::Method,
    path: &str,
    conversation: Uuid,
    person: &str,
    lease: Option<Uuid>,
) -> reqwest::RequestBuilder {
    let mut b = common::http()
        .request(method, format!("{}{path}", r.url))
        .bearer_auth(&r.tokens.gateway)
        .header("aviary-request", Uuid::now_v7().to_string())
        .header("aviary-protocol", "magpie-sandbox=1")
        .header("aviary-sandbox-id", Uuid::new_v4().to_string())
        .header("aviary-sandbox-conversation", conversation.to_string())
        .header("aviary-sandbox-person", person);
    if let Some(l) = lease {
        b = b.header("aviary-lease", l.to_string());
    }
    b
}

/// Gives `conversation` a lease (the cc store's own lease route is WP-12's).
async fn grant_lease(pool: &aviary_pg::Pool, conversation: Uuid) -> Uuid {
    let lease = Uuid::new_v4();
    pool.get()
        .await
        .unwrap()
        .execute(
            "update conversations set lease_id = $2 where id = $1",
            &[&conversation, &lease],
        )
        .await
        .unwrap();
    lease
}

async fn get_blob(r: &common::Replica, hex: &str, variant: BlobVariant) -> reqwest::Response {
    r.pigeon()
        .blob(hex, &BlobQuery { variant: Some(variant) }, None)
        .await
        .unwrap()
}

async fn status_of(r: &common::Replica, hex: &str, variant: BlobVariant) -> u16 {
    match r.pigeon().blob(hex, &BlobQuery { variant: Some(variant) }, None).await {
        Ok(resp) => resp.status().as_u16(),
        Err(e) => e.code().map_or(0, magpie_api::errors::ErrorCode::status),
    }
}

fn idle_bucket(r: &common::Replica) -> Arc<Blobs> {
    r.app().blobs.clone()
}

#[tokio::test]
async fn a_staged_blob_is_served_before_and_after_the_uploader_stores_it() {
    let db = common::fresh_db("staged").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = conversation(&r, ALICE, "k1").await;
    let text = br#"line one\nline \"two\""#;
    let sha = stage(r.pool(), c, text, "text", "json-string", None).await;
    let hex = blobs::to_hex(&sha);

    let resp = get_blob(&r, &hex, BlobVariant::Full).await;
    assert_eq!(resp.headers()["content-type"], "text/plain; charset=utf-8");
    assert_eq!(resp.headers()["cache-control"], "private, max-age=31536000, immutable");
    assert_eq!(resp.headers()["etag"], format!("\"{hex}\"").as_str());
    assert_eq!(resp.headers()["x-content-type-options"], "nosniff");
    assert_eq!(resp.headers()["content-security-policy"], "default-src 'none'; sandbox");
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"line one\nline \"two\"");
    // the sandbox reads it raw, and as text
    let raw = gateway(
        &r,
        reqwest::Method::GET,
        &format!("/v1/sandbox/blobs/{hex}?as=raw"),
        c,
        ALICE,
        None,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(raw.status(), 200);
    assert_eq!(raw.bytes().await.unwrap().as_ref(), text);
    // a sandbox of another conversation does not see it
    let other = conversation(&r, ALICE, "k2").await;
    let denied = gateway(
        &r,
        reqwest::Method::GET,
        &format!("/v1/sandbox/blobs/{hex}"),
        other,
        ALICE,
        None,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(denied.status(), 404);

    // the uploader stores it: the row says so, the staging row is gone, the bucket has the bytes
    let up = Uploader::default();
    assert_eq!(up.pass(r.pool(), &idle_bucket(&r)).await.unwrap(), 1);
    let row = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select stored, (select count(*) from blob_staging) from blobs where sha256 = $1",
            &[&sha.as_slice()],
        )
        .await
        .unwrap();
    assert!(row.get::<_, bool>(0));
    assert_eq!(row.get::<_, i64>(1), 0);
    let object = r.app().blobs.bucket.get(&store::blob_key(&hex)).await.unwrap();
    assert_eq!(object.as_ref(), text);
    // a second replica (an empty read cache) serves it from the bucket
    let r2 = common::replica(&db.url, &t).await;
    let resp = get_blob(&r2, &hex, BlobVariant::Full).await;
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"line one\nline \"two\"");
    // conditional reads
    let not_modified = r2.pigeon().blob(&hex, &BlobQuery::default(), None).await;
    assert!(not_modified.is_ok());
    let cached = common::http()
        .get(format!("{}/v1/blobs/{hex}", r2.url))
        .bearer_auth(&t.pigeon)
        .header("if-none-match", format!("\"{hex}\""))
        .send()
        .await
        .unwrap();
    assert_eq!(cached.status(), 304);
}

#[tokio::test]
async fn a_bucket_outage_keeps_staged_blobs_readable_and_the_uploader_drains_afterwards() {
    let db = common::fresh_db("outage").await;
    let t = common::Tokens::new();
    let bucket = common::test_bucket(&db.url).await;
    let s3: std::net::SocketAddr = bucket.endpoint.trim_start_matches("http://").parse().unwrap();
    let proxy = common::CutProxy::start(s3).await;
    let mut via_proxy = bucket.clone();
    via_proxy.endpoint = format!("http://{}", proxy.addr);
    let r = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::idle(), via_proxy).await;
    let c = conversation(&r, ALICE, "k").await;
    proxy.cut();
    let media = png(64, 48, false);
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &media);
    let sha = stage(r.pool(), c, b64.as_bytes(), "media", "base64-json", Some("image/png")).await;
    let hex = blobs::to_hex(&sha);

    let up = Uploader::default();
    assert_eq!(up.pass(r.pool(), &idle_bucket(&r)).await.unwrap(), 0);
    let row = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select attempts, next_at > now() from blob_staging where sha256 = $1",
            &[&sha.as_slice()],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, i32>(0), 1);
    assert!(row.get::<_, bool>(1), "a failed upload backs off");
    // still readable, decoded from base64; its thumbnail is made without being kept
    let full = get_blob(&r, &hex, BlobVariant::Full).await;
    assert_eq!(full.headers()["content-type"], "image/png");
    assert_eq!(full.headers()["content-disposition"], "inline");
    assert_eq!(full.bytes().await.unwrap().as_ref(), media.as_slice());
    let thumb = get_blob(&r, &hex, BlobVariant::Thumb).await;
    assert_eq!(thumb.headers()["content-type"], "image/webp");

    proxy.restore();
    r.pool()
        .get()
        .await
        .unwrap()
        .execute("update blob_staging set next_at = now()", &[])
        .await
        .unwrap();
    assert!(
        common::eventually(Duration::from_secs(20), async || {
            up.pass(r.pool(), &idle_bucket(&r)).await.unwrap() == 1
        })
        .await
    );
    let stored: bool = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select stored from blobs where sha256 = $1", &[&sha.as_slice()])
        .await
        .unwrap()
        .get(0);
    assert!(stored);
}

#[tokio::test]
async fn the_gc_frees_only_unreferenced_old_blobs_and_the_reconcile_removes_orphans() {
    let db = common::fresh_db("gc").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = conversation(&r, ALICE, "k").await;
    let up = Uploader::default();
    let b = idle_bucket(&r);
    let kept = stage(r.pool(), c, b"referenced", "text", "json-string", None).await;
    let young = stage(r.pool(), c, b"young", "text", "json-string", None).await;
    let old = stage(r.pool(), c, b"old", "text", "json-string", None).await;
    let staged = stage(r.pool(), c, b"still staged", "text", "json-string", None).await;
    assert!(
        common::eventually(Duration::from_secs(20), async || {
            up.pass(r.pool(), &b).await.unwrap();
            r.pool()
                .get()
                .await
                .unwrap()
                .query_one("select count(*) from blob_staging", &[])
                .await
                .unwrap()
                .get::<_, i64>(0)
                == 0
        })
        .await
    );
    // put one back to staging: a staged blob is never freed
    let sql = r.pool().get().await.unwrap();
    sql.execute(
        "update blobs set stored = false where sha256 = $1",
        &[&staged.as_slice()],
    )
    .await
    .unwrap();
    sql.execute(
        "insert into blob_staging (sha256, bytes) values ($1, 'still staged')",
        &[&staged.as_slice()],
    )
    .await
    .unwrap();
    // drop the references of all but `kept`, and age all but `young`
    sql.execute("delete from blob_refs where blob <> $1", &[&kept.as_slice()])
        .await
        .unwrap();
    sql.execute(
        "update blobs set last_ref_at = now() - interval '25 hours' where sha256 <> $1",
        &[&young.as_slice()],
    )
    .await
    .unwrap();

    assert_eq!(gc::free(r.pool(), &b).await.unwrap(), 1);
    let left: Vec<Vec<u8>> = sql
        .query("select sha256 from blobs order by sha256", &[])
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    let mut want = vec![kept.to_vec(), young.to_vec(), staged.to_vec()];
    want.sort();
    assert_eq!(left, want);
    assert!(matches!(
        b.bucket.get(&store::blob_key(&blobs::to_hex(&old))).await,
        Err(store::StoreError::NotFound)
    ));
    assert_eq!(status_of(&r, &blobs::to_hex(&old), BlobVariant::Full).await, 404);
    // an unreferenced blob disappears for readers at once, before the GC frees it
    assert_eq!(status_of(&r, &blobs::to_hex(&young), BlobVariant::Full).await, 404);
    assert_eq!(status_of(&r, &blobs::to_hex(&kept), BlobVariant::Full).await, 200);

    // the reconcile: an object with no row goes, objects with rows stay (cutoff in the future: everything is "old")
    let orphan = blobs::sha256(b"orphan");
    b.bucket
        .put(
            &store::blob_key(&blobs::to_hex(&orphan)),
            b"orphan".to_vec().into(),
            None,
        )
        .await
        .unwrap();
    b.bucket
        .put(&store::thumb_key(&blobs::to_hex(&orphan)), b"x".to_vec().into(), None)
        .await
        .unwrap();
    let cutoff = time::OffsetDateTime::now_utc() + time::Duration::minutes(1);
    assert_eq!(gc::reconcile_before(r.pool(), &b, cutoff).await.unwrap(), 2);
    assert!(b.bucket.get(&store::blob_key(&blobs::to_hex(&kept))).await.is_ok());
    assert!(matches!(
        b.bucket.get(&store::blob_key(&blobs::to_hex(&orphan))).await,
        Err(store::StoreError::NotFound)
    ));
}

#[tokio::test]
async fn image_variants_and_their_refusals() {
    let db = common::fresh_db("variants").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = conversation(&r, ALICE, "k").await;
    let raw = |bytes: &[u8], mime: &str| {
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
        (b64, mime.to_owned())
    };
    let (small, _) = raw(&png(300, 200, false), "image/png");
    let (big, _) = raw(&png(3000, 1000, false), "image/png");
    let small_sha = stage(r.pool(), c, small.as_bytes(), "media", "base64-json", Some("image/png")).await;
    let big_sha = stage(r.pool(), c, big.as_bytes(), "media", "base64-json", Some("image/png")).await;
    let sql = r.pool().get().await.unwrap();
    sql.execute(
        "update blobs set width = 300, height = 200 where sha256 = $1",
        &[&small_sha.as_slice()],
    )
    .await
    .unwrap();
    sql.execute(
        "update blobs set width = 3000, height = 1000 where sha256 = $1",
        &[&big_sha.as_slice()],
    )
    .await
    .unwrap();
    let text_sha = stage(r.pool(), c, b"plain", "text", "json-string", None).await;

    // a small model-native image is its own display copy
    let d = get_blob(&r, &blobs::to_hex(&small_sha), BlobVariant::Display).await;
    assert_eq!(d.headers()["content-type"], "image/png");
    assert_eq!(d.bytes().await.unwrap().as_ref(), png(300, 200, false).as_slice());
    // a large one gets a WebP fitted to 2000 px, kept under display/
    let d = get_blob(&r, &blobs::to_hex(&big_sha), BlobVariant::Display).await;
    assert_eq!(d.headers()["content-type"], "image/webp");
    assert_eq!(
        d.headers()["etag"],
        format!("\"{}-display\"", blobs::to_hex(&big_sha)).as_str()
    );
    let img = image::load_from_memory(&d.bytes().await.unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (2000, 667));
    assert!(
        r.app()
            .blobs
            .bucket
            .get(&store::display_key(&blobs::to_hex(&big_sha)))
            .await
            .is_ok()
    );
    // thumbnails: long edge 512
    let th = get_blob(&r, &blobs::to_hex(&big_sha), BlobVariant::Thumb).await;
    let img = image::load_from_memory(&th.bytes().await.unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (512, 171));
    // a text blob has no image variant; an unknown id is not found
    assert_eq!(status_of(&r, &blobs::to_hex(&text_sha), BlobVariant::Thumb).await, 415);
    assert_eq!(
        status_of(&r, &blobs::to_hex(&text_sha), BlobVariant::Display).await,
        415
    );
    assert_eq!(status_of(&r, &"0".repeat(64), BlobVariant::Full).await, 404);
    assert_eq!(status_of(&r, "nothex", BlobVariant::Full).await, 404);
}

#[tokio::test]
async fn the_sandbox_puts_blobs_with_its_lease_verified_against_the_sha() {
    let db = common::fresh_db("sbput").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let c = conversation(&r, ALICE, "k").await;
    let lease = grant_lease(r.pool(), c).await;
    let bytes = b"%PDF-1.7 a sent file".to_vec();
    let hex = blobs::to_hex(&blobs::sha256(&bytes));
    let put = |lease: Option<Uuid>, path_hex: String, body: Vec<u8>| {
        gateway(
            &r,
            reqwest::Method::PUT,
            &format!("/v1/sandbox/blobs/{path_hex}"),
            c,
            ALICE,
            lease,
        )
        .body(body)
        .send()
    };
    assert_eq!(put(None, hex.clone(), bytes.clone()).await.unwrap().status(), 409);
    assert_eq!(
        put(Some(Uuid::new_v4()), hex.clone(), bytes.clone())
            .await
            .unwrap()
            .status(),
        409
    );
    let wrong = put(Some(lease), "a".repeat(64), bytes.clone()).await.unwrap();
    assert_eq!(wrong.status(), 400);
    assert_eq!(wrong.json::<serde_json::Value>().await.unwrap()["code"], "sha_mismatch");
    assert_eq!(
        put(Some(lease), hex.clone(), bytes.clone()).await.unwrap().status(),
        201
    );
    assert_eq!(
        put(Some(lease), hex.clone(), bytes.clone()).await.unwrap().status(),
        200
    );
    let row = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select kind, mime, stored, encoding from blobs", &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "media");
    assert_eq!(row.get::<_, Option<String>>(1).as_deref(), Some("application/pdf"));
    assert!(row.get::<_, bool>(2));
    assert_eq!(row.get::<_, String>(3), "raw");
    let object = r.app().blobs.bucket.get(&store::blob_key(&hex)).await.unwrap();
    assert_eq!(object.as_ref(), bytes.as_slice());
    // someone else's conversation is not found
    let theirs = gateway(
        &r,
        reqwest::Method::PUT,
        &format!("/v1/sandbox/blobs/{hex}"),
        c,
        "bob@example.com",
        Some(lease),
    )
    .body(bytes.clone())
    .send()
    .await
    .unwrap();
    assert_eq!(theirs.status(), 404);
}
