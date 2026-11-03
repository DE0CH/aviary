//! Blobs and the bucket (magpie 5, 15): staged blobs served before the uploader stores them, the uploader through a
//! bucket outage, the GC's rules and the reconcile, M15's variants and headers, the sandbox's blob routes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::files::{conversation, gateway, grant_lease, png};
use magpie::blobs::{self, Blobs, gc, store, uploader::Uploader};
use magpie_api::uploads::{BlobQuery, BlobVariant};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

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

#[tokio::test]
async fn cc_writes_that_add_blobs_keep_working_while_the_bucket_is_unreachable() {
    let db = common::fresh_db("ccoutage").await;
    let t = common::Tokens::new();
    let bucket = common::test_bucket(&db.url).await;
    let s3: std::net::SocketAddr = bucket.endpoint.trim_start_matches("http://").parse().unwrap();
    let proxy = common::CutProxy::start(s3).await;
    let mut via_proxy = bucket.clone();
    via_proxy.endpoint = format!("http://{}", proxy.addr);
    let r = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::fast(), via_proxy).await;
    let sb = common::cc::Sandbox::leased(&r, ALICE).await;
    sb.mkdirs("projects/-workspace").await;
    proxy.cut();
    // a line with an image of more than 1 KiB of base64: extracted into a staged blob, the write never waits for the
    // bucket
    let noise: Vec<u8> = (0..4096u32).flat_map(|i| blobs::sha256(&i.to_le_bytes())).collect();
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, noise);
    let line = format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"{b64}\"}}}}]}}}}\n"
    );
    let path = "projects/-workspace/side.jsonl";
    sb.write(path, 0, line.as_bytes()).await.unwrap();
    let staged: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from blob_staging", &[])
        .await
        .unwrap()
        .get(0);
    assert!(staged >= 1, "the image was not staged");
    sb.write(path, u64::try_from(line.len()).unwrap(), line.as_bytes())
        .await
        .unwrap();
    // the file reads back byte for byte from staging, the bucket still cut
    let mut want = line.clone().into_bytes();
    want.extend_from_slice(line.as_bytes());
    assert_eq!(sb.read(path).await.unwrap(), want);
    // the bucket back: the uploader (every 100 ms here) drains staging
    proxy.restore();
    r.pool()
        .get()
        .await
        .unwrap()
        .execute("update blob_staging set next_at = now()", &[])
        .await
        .unwrap();
    assert!(
        common::eventually(Duration::from_secs(30), async || {
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
        .await,
        "staging did not drain"
    );
    assert_eq!(sb.read(path).await.unwrap(), want);
}

#[tokio::test]
async fn a_resend_and_a_truncate_at_a_plain_line_need_no_bucket() {
    let db = common::fresh_db("ccrule0").await;
    let t = common::Tokens::new();
    let bucket = common::test_bucket(&db.url).await;
    let s3: std::net::SocketAddr = bucket.endpoint.trim_start_matches("http://").parse().unwrap();
    let proxy = common::CutProxy::start(s3).await;
    let mut via_proxy = bucket.clone();
    via_proxy.endpoint = format!("http://{}", proxy.addr);
    let r = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::fast(), via_proxy.clone()).await;
    let sb = common::cc::Sandbox::leased(&r, ALICE).await;
    sb.mkdirs("projects/-workspace").await;
    // an image line (its blob uploaded, out of staging) and two plain lines
    let noise: Vec<u8> = (0..4096u32).flat_map(|i| blobs::sha256(&i.to_le_bytes())).collect();
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, noise);
    let image = format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"{b64}\"}}}}]}}}}\n"
    );
    let plain = b"{\"type\":\"attachment\",\"n\":1}\n{\"type\":\"attachment\",\"n\":2}\n".to_vec();
    let path = "projects/-workspace/side.jsonl";
    let all = [image.as_bytes(), &plain].concat();
    let v = sb.write(path, 0, &all).await.unwrap();
    assert!(
        common::eventually(Duration::from_secs(30), async || {
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
        .await,
        "staging did not drain"
    );
    // a second replica (nothing in its read cache) with the bucket cut
    let r2 = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::fast(), via_proxy).await;
    let mut sb2 = common::cc::Sandbox::new(&r2, sb.conversation, sb.id, ALICE);
    sb2.lease = sb.lease;
    proxy.cut();
    // rule 0: the same bytes again (the sentry's resend after a checkpoint), whole and from the image line alone, by
    // the lines' sha256: no change, no bucket
    assert_eq!(sb2.write(path, 0, &all).await.unwrap(), v);
    assert_eq!(sb2.write(path, 0, image.as_bytes()).await.unwrap(), v);
    // a resend that cuts the plain lines compares their stubs; one that cuts the image line needs its blob (6.13 gap)
    assert_eq!(sb2.write(path, image.len() as u64 + 3, &plain[3..20]).await.unwrap(), v);
    let e = sb2.write(path, 10, &image.as_bytes()[10..40]).await.unwrap_err();
    assert_eq!(common::cc::code(&e), "unavailable");
    // a truncate inside a plain line and at a line start: no bucket
    let cut = image.len() + 10;
    assert_eq!(sb2.truncate(path, cut as u64).await.unwrap().size, cut as u64);
    assert_eq!(
        sb2.truncate(path, image.len() as u64).await.unwrap().size,
        image.len() as u64
    );
    // a truncate inside the image line needs its blob: 503 until the bucket is back
    let e = sb2.truncate(path, 100).await.unwrap_err();
    assert_eq!(common::cc::code(&e), "unavailable");
    // the replica that uploaded the blob keeps its bytes in its read cache: it cuts the image line without the bucket
    let size = image.len() as u64;
    let v = sb.write(path, 10, &image.as_bytes()[10..40]).await.unwrap();
    assert_eq!(sb.write(path, 10, &image.as_bytes()[10..40]).await.unwrap(), v);
    assert_eq!(sb.truncate(path, size - 7).await.unwrap().size, size - 7);
    assert_eq!(sb.read(path).await.unwrap(), image.as_bytes()[..image.len() - 7]);
    proxy.restore();
    assert_eq!(sb2.truncate(path, 100).await.unwrap().size, 100);
    assert_eq!(sb2.read(path).await.unwrap(), image.as_bytes()[..100]);
}

#[tokio::test]
async fn a_hanging_bucket_pins_no_request_connection() {
    // the bucket accepts connections and never answers: uploads and the uploader hang in their PUTs (60 s each),
    // holding their blob locks; cc writes and every other request keep their latency, past the pool's 5 s timeout
    let db = common::fresh_db("hang").await;
    let t = common::Tokens::new();
    let bucket = common::test_bucket(&db.url).await;
    let s3: std::net::SocketAddr = bucket.endpoint.trim_start_matches("http://").parse().unwrap();
    let proxy = common::CutProxy::start(s3).await;
    let mut via_proxy = bucket.clone();
    via_proxy.endpoint = format!("http://{}", proxy.addr);
    let r = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::fast(), via_proxy).await;
    let sb = common::cc::Sandbox::leased(&r, ALICE).await;
    sb.mkdirs("projects/-workspace").await;
    proxy.stall();
    // staged images for the uploader (every 100 ms here) to hang on
    let path = "projects/-workspace/side.jsonl";
    let mut offset = 0u64;
    for i in 0..6u32 {
        let noise: Vec<u8> = (0..4096u32)
            .flat_map(|j| blobs::sha256(&(i * 4096 + j).to_le_bytes()))
            .collect();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, noise);
        let line = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"{b64}\"}}}}]}}}}\n"
        );
        sb.write(path, offset, line.as_bytes()).await.unwrap();
        offset += u64::try_from(line.len()).unwrap();
    }
    // 16 image uploads, each with a model copy (2 blob locks), all hanging in their PUTs
    let mut uploads = Vec::new();
    for i in 0..16u32 {
        let c = r.pigeon();
        let bytes = png(2100 + i, 40, false);
        uploads.push(tokio::spawn(async move {
            c.person(ALICE)
                .upload("wide.png", magpie_api::uploads::UploadKind::Image, "image/png", bytes)
                .await
        }));
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        uploads.iter().all(|u| !u.is_finished()),
        "the uploads should hang in the bucket"
    );
    let claimed: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from blob_staging where next_at > now()", &[])
        .await
        .unwrap()
        .get(0);
    assert!(claimed >= 1, "the uploader should be holding staged blobs");
    // 7 s of cc writes and reads: longer than the pool's 5 s wait, so a pinned pool would show as 503s
    let mut took = Vec::new();
    let end = tokio::time::Instant::now() + Duration::from_secs(7);
    let line = b"{\"type\":\"system\",\"subtype\":\"note\"}\n";
    let other = "projects/-workspace/other.jsonl";
    let mut at = 0u64;
    while tokio::time::Instant::now() < end {
        let t0 = std::time::Instant::now();
        sb.write(other, at, line)
            .await
            .expect("a cc write while the bucket hangs");
        at += u64::try_from(line.len()).unwrap();
        r.pigeon()
            .person(ALICE)
            .list_conversations(&magpie_api::conversations::ListQuery::default())
            .await
            .expect("a list while the bucket hangs");
        took.push(t0.elapsed());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    took.sort();
    let p99 = took[took.len() * 99 / 100];
    assert!(took.len() >= 20, "only {} rounds", took.len());
    assert!(
        p99 < Duration::from_millis(500),
        "p99 of a cc write and a list: {p99:?}"
    );
    assert!(
        uploads.iter().all(|u| !u.is_finished()),
        "the uploads are still hanging"
    );
    for u in uploads {
        u.abort();
    }
    proxy.restore();
}

/// The blob advisory locks (classid `mblb`) held on this test's database (`pg_locks` covers every database of the
/// server, and other tests run alongside).
async fn blob_locks(pool: &aviary_pg::Pool) -> i64 {
    pool.get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from pg_locks where locktype = 'advisory' and classid = 1835166818::oid
               and objsubid = 2 and granted
               and database = (select oid from pg_database where datname = current_database())",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn a_cancelled_upload_gives_its_blob_locks_back() {
    // the bucket hangs; an image upload (with its model copy) is abandoned by its client mid-PUT: its locks go back
    // at once, and the GC can free the blob later
    let db = common::fresh_db("cancelled").await;
    let t = common::Tokens::new();
    let bucket = common::test_bucket(&db.url).await;
    let s3: std::net::SocketAddr = bucket.endpoint.trim_start_matches("http://").parse().unwrap();
    let proxy = common::CutProxy::start(s3).await;
    let mut via_proxy = bucket.clone();
    via_proxy.endpoint = format!("http://{}", proxy.addr);
    let r = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::idle(), via_proxy).await;
    proxy.stall();
    let bytes = png(2100, 40, false);
    let mut uploads = Vec::new();
    for _ in 0..3 {
        let c = r.pigeon();
        let body = bytes.clone();
        uploads.push(tokio::spawn(async move {
            c.person(ALICE)
                .upload("wide.png", magpie_api::uploads::UploadKind::Image, "image/png", body)
                .await
        }));
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(uploads.iter().all(|u| !u.is_finished()), "the uploads should hang");
    assert!(blob_locks(r.pool()).await >= 1, "the hanging uploads hold their locks");
    for u in uploads {
        u.abort();
    }
    proxy.restore();
    assert!(
        common::eventually(Duration::from_secs(10), async || blob_locks(r.pool()).await == 0).await,
        "{} blob locks left after the uploads were abandoned",
        blob_locks(r.pool()).await
    );
    // the same image again, through: then unreferenced and aged, the GC frees both blobs (a lock left on the session
    // would keep the GC's exclusive lock from ever being had)
    let up = r
        .pigeon()
        .person(ALICE)
        .upload("wide.png", magpie_api::uploads::UploadKind::Image, "image/png", bytes)
        .await
        .unwrap();
    let sql = r.pool().get().await.unwrap();
    sql.execute("delete from blob_refs where owner = $1", &[&up.upload_id])
        .await
        .unwrap();
    sql.execute("delete from uploads where id = $1", &[&up.upload_id])
        .await
        .unwrap();
    sql.execute("update blobs set last_ref_at = now() - interval '2 days'", &[])
        .await
        .unwrap();
    assert_eq!(gc::free(r.pool(), &r.app().blobs).await.unwrap(), 2);
    assert_eq!(blob_locks(r.pool()).await, 0);
}

#[tokio::test]
async fn a_stored_row_whose_object_a_gc_delete_lost_is_put_again() {
    // the GC's reverify (after it lost its lock mid-delete): a row that says stored with no object clears `stored`,
    // and the next writer of those bytes puts them again
    let db = common::fresh_db("reverify").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let bytes = b"a sent file whose object went".to_vec();
    let sha = blobs::sha256(&bytes);
    let sql = r.pool().get().await.unwrap();
    sql.execute(
        "insert into blobs (sha256, size, kind, encoding, stored) values ($1, $2, 'file', 'raw', true)",
        &[&sha.as_slice(), &i64::try_from(bytes.len()).unwrap()],
    )
    .await
    .unwrap();
    gc::reverify(r.pool(), &r.app().blobs, &sha).await.unwrap();
    let stored: bool = sql
        .query_one("select stored from blobs where sha256 = $1", &[&sha.as_slice()])
        .await
        .unwrap()
        .get(0);
    assert!(!stored);
    let c = conversation(&r, ALICE, "k").await;
    let lease = grant_lease(r.pool(), c).await;
    let st = gateway(
        &r,
        reqwest::Method::PUT,
        &format!("/v1/sandbox/blobs/{}", blobs::to_hex(&sha)),
        c,
        ALICE,
        Some(lease),
    )
    .body(bytes.clone())
    .send()
    .await
    .unwrap()
    .status();
    assert_eq!(st, 201);
    let stored: bool = sql
        .query_one("select stored from blobs where sha256 = $1", &[&sha.as_slice()])
        .await
        .unwrap()
        .get(0);
    assert!(stored);
    let got = r
        .app()
        .blobs
        .bucket
        .get(&store::blob_key(&blobs::to_hex(&sha)))
        .await
        .unwrap();
    assert_eq!(got.as_ref(), bytes.as_slice());
    // a stored row whose object is there is left as it is
    gc::reverify(r.pool(), &r.app().blobs, &sha).await.unwrap();
    let stored: bool = sql
        .query_one("select stored from blobs where sha256 = $1", &[&sha.as_slice()])
        .await
        .unwrap()
        .get(0);
    assert!(stored);
}

#[tokio::test]
async fn a_revalidation_answers_304_before_the_bucket_is_asked() {
    let db = common::fresh_db("early304").await;
    let t = common::Tokens::new();
    let bucket = common::test_bucket(&db.url).await;
    let s3: std::net::SocketAddr = bucket.endpoint.trim_start_matches("http://").parse().unwrap();
    let proxy = common::CutProxy::start(s3).await;
    let mut via_proxy = bucket.clone();
    via_proxy.endpoint = format!("http://{}", proxy.addr);
    let r = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::idle(), via_proxy).await;
    let up = r
        .pigeon()
        .person(ALICE)
        .upload(
            "p.png",
            magpie_api::uploads::UploadKind::Image,
            "image/png",
            png(64, 48, false),
        )
        .await
        .unwrap();
    let sha: Vec<u8> = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select blob from uploads where id = $1", &[&up.upload_id])
        .await
        .unwrap()
        .get(0);
    let hex = blobs::to_hex(&<[u8; 32]>::try_from(sha.as_slice()).unwrap());
    proxy.cut();
    let get = |variant: &'static str, etag: Option<String>| {
        let mut req = common::http()
            .get(format!("{}/v1/blobs/{hex}?variant={variant}", r.url))
            .bearer_auth(&t.pigeon);
        if let Some(e) = etag {
            req = req.header("if-none-match", e);
        }
        req.send()
    };
    // the bucket cut: a read needs it, a revalidation does not
    assert_eq!(get("full", None).await.unwrap().status(), 503);
    let full = get("full", Some(format!("\"{hex}\""))).await.unwrap();
    assert_eq!(full.status(), 304);
    assert_eq!(full.headers()["etag"], format!("\"{hex}\"").as_str());
    let thumb = get("thumb", Some(format!("\"{hex}-thumb\""))).await.unwrap();
    assert_eq!(thumb.status(), 304);
    // another variant's etag is not a match
    assert_eq!(
        get("full", Some(format!("\"{hex}-thumb\""))).await.unwrap().status(),
        503
    );
    proxy.restore();
}

/// A stored blob of `mib` MiB put straight into the bucket under a made-up sha (`seed`), referenced from a made-up
/// conversation, so M15 serves it; nothing is cached.
async fn stored_blob(r: &common::Replica, seed: u8, mib: usize, encoding: &str, bytes: Option<Vec<u8>>) -> [u8; 32] {
    let bytes = bytes.unwrap_or_else(|| vec![seed; mib << 20]);
    let mut sha = [seed; 32];
    sha[31] = 0xa5;
    r.app()
        .blobs
        .bucket
        .put(&store::blob_key(&blobs::to_hex(&sha)), bytes.clone().into(), None)
        .await
        .unwrap();
    let sql = r.pool().get().await.unwrap();
    let kind = if encoding == "raw" { "file" } else { "media" };
    let mime = if encoding == "raw" { None } else { Some("image/png") };
    sql.execute(
        "insert into blobs (sha256, size, kind, encoding, mime, stored) values ($1, $2, $3, $4, $5, true)",
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
    sql.execute(
        "insert into blob_refs (owner, owner_kind, blob) values ($1, 'conversation', $2)",
        &[&Uuid::new_v4(), &sha.as_slice()],
    )
    .await
    .unwrap();
    sha
}

/// Opens M15 reads of `shas` that never read their bodies (stalled clients): each sends its status once it has its
/// headers and holds its response until `release` fires.
fn stalled_readers(
    r: &common::Replica,
    shas: &[[u8; 32]],
    release: &Arc<tokio::sync::Notify>,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<u16>,
    Vec<tokio::task::JoinHandle<()>>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let tasks = shas
        .iter()
        .map(|sha| {
            let p = r.pigeon();
            let hex = blobs::to_hex(sha);
            let tx = tx.clone();
            let release = release.clone();
            tokio::spawn(async move {
                let resp = p
                    .blob(
                        &hex,
                        &BlobQuery {
                            variant: Some(BlobVariant::Full),
                        },
                        None,
                    )
                    .await
                    .unwrap();
                let _ = tx.send(resp.status().as_u16());
                release.notified().await;
                drop(resp);
            })
        })
        .collect();
    (rx, tasks)
}

#[tokio::test]
async fn slow_readers_of_large_uncached_blobs_never_keep_an_upload_waiting() {
    // eight clients read 30 MiB blobs the cache does not hold and never take their bodies: reads hold at most their
    // part of the body budget (128 MiB: four of them), the others wait for it, and an upload is still admitted at once
    let db = common::fresh_db("slowreads").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let mut shas = Vec::new();
    for seed in 1..=8u8 {
        shas.push(stored_blob(&r, seed, 30, "raw", None).await);
    }
    let release = Arc::new(tokio::sync::Notify::new());
    let (mut statuses, readers) = stalled_readers(&r, &shas, &release);
    for _ in 0..4 {
        let st = tokio::time::timeout(Duration::from_secs(15), statuses.recv())
            .await
            .expect("four reads fit the reads' part")
            .unwrap();
        assert_eq!(st, 200);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(statuses.try_recv().is_err(), "a fifth read waits for the reads' part");
    // the four hold 120 MiB of the whole budget (the waiting fifth has the last 8 MiB of the reads' part set aside)
    let (body_free, read_free) = r.app().blobs.memory_free();
    assert!(read_free <= 128 - 120);
    assert_eq!(body_free, 256 - 120);
    // an upload is admitted while the reads wait
    let up = tokio::time::timeout(
        Duration::from_secs(20),
        r.pigeon().person(ALICE).upload(
            "while-reading.bin",
            magpie_api::uploads::UploadKind::File,
            "application/octet-stream",
            vec![7u8; 20 << 20],
        ),
    )
    .await
    .expect("the upload is not kept waiting by slow readers")
    .unwrap();
    assert_eq!(up.size, 20 << 20);
    // the stalled clients go: the waiting reads are answered
    for _ in 0..4 {
        release.notify_one();
    }
    for _ in 0..4 {
        let st = tokio::time::timeout(Duration::from_secs(15), statuses.recv())
            .await
            .expect("the waiting reads are answered")
            .unwrap();
        assert_eq!(st, 200);
    }
    for _ in 0..4 {
        release.notify_one();
    }
    for t in readers {
        t.await.unwrap();
    }
    assert!(
        common::eventually(Duration::from_secs(10), async || r.app().blobs.memory_free()
            == (256, 128))
        .await,
        "the reads give their memory back: {:?}",
        r.app().blobs.memory_free()
    );
}

#[tokio::test]
async fn many_readers_of_one_uncached_blob_make_one_fetch() {
    // six stalled clients open one 60 MiB blob the cache does not hold at once: one fetch holds its 60 MiB, and the
    // others are answered from the cache it filled (each taking its own 60 MiB, only two would fit the reads' part)
    let db = common::fresh_db("onefetch").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let sha = stored_blob(&r, 9, 60, "raw", None).await;
    let release = Arc::new(tokio::sync::Notify::new());
    let (mut statuses, readers) = stalled_readers(&r, &[sha; 6], &release);
    for _ in 0..6 {
        let st = tokio::time::timeout(Duration::from_secs(15), statuses.recv())
            .await
            .expect("every reader is answered")
            .unwrap();
        assert_eq!(st, 200);
    }
    assert_eq!(r.app().blobs.memory_free().1, 128 - 60);
    for _ in 0..6 {
        release.notify_one();
    }
    for t in readers {
        t.await.unwrap();
    }
}

#[tokio::test]
async fn fetched_and_decoded_blobs_hold_their_memory_until_dropped() {
    let db = common::fresh_db("heldfetch").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let first = stored_blob(&r, 21, 8, "raw", None).await;
    let second = stored_blob(&r, 22, 8, "raw", None).await;
    // the cc store's batch: the whole batch's share at once, kept by the buffers themselves (no copies)
    let got = blobs::fetch_stored(r.app(), &[first, second]).await.unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[&first].len(), 8 << 20);
    assert_eq!(r.app().blobs.memory_free(), (256 - 16, 128));
    let one = got[&first].clone();
    drop(got);
    assert_eq!(
        r.app().blobs.memory_free().0,
        256 - 16,
        "a buffer still alive keeps the batch's share"
    );
    drop(one);
    assert_eq!(r.app().blobs.memory_free(), (256, 128));
    // a base64-json blob's decoded copy (from the cache now) holds its own size of the reads' part while it is alive
    let raw = vec![3u8; 6 << 20];
    let text = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &raw).into_bytes();
    let encoded = stored_blob(&r, 23, 0, "base64-json", Some(text)).await;
    let row = blobs::row(&r.pool().get().await.unwrap(), &encoded)
        .await
        .unwrap()
        .unwrap();
    let decoded = r.app().blobs.original(r.pool(), &row).await.unwrap();
    assert_eq!(decoded.as_ref(), raw.as_slice());
    assert_eq!(r.app().blobs.memory_free().1, 128 - 6);
    drop(decoded);
    assert_eq!(r.app().blobs.memory_free(), (256, 128));
}

async fn queued(sql: &aviary_pg::Conn, sha: &[u8; 32]) -> i64 {
    sql.query_one(
        "select count(*) from blob_reverify where sha256 = $1",
        &[&sha.as_slice()],
    )
    .await
    .unwrap()
    .get(0)
}

#[tokio::test]
async fn a_reverification_the_gc_could_not_run_is_retried_by_the_next_pass() {
    // the GC lost a blob's lock mid-delete and cannot take it back (a writer holds it) within its wait: the blob stays
    // recorded in blob_reverify, and a later pass runs the check (here: the object is gone, so stored is cleared)
    let db = common::fresh_db("reverifyq").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let bytes = b"an object a late GC delete took".to_vec();
    let sha = blobs::sha256(&bytes);
    let sql = r.pool().get().await.unwrap();
    sql.execute(
        "insert into blobs (sha256, size, kind, encoding, stored) values ($1, $2, 'file', 'raw', true)",
        &[&sha.as_slice(), &i64::try_from(bytes.len()).unwrap()],
    )
    .await
    .unwrap();
    let writer = r.app().blobs.shared(&sha).await.unwrap();
    gc::reverify_within(r.pool(), &r.app().blobs, &sha, Duration::from_millis(500))
        .await
        .unwrap();
    assert_eq!(queued(&sql, &sha).await, 1, "not checked: recorded for the next pass");
    // still held at the next pass: still recorded
    assert_eq!(gc::reverify_queued(r.pool(), &r.app().blobs).await.unwrap(), 1);
    writer.release().await;
    assert_eq!(gc::reverify_queued(r.pool(), &r.app().blobs).await.unwrap(), 0);
    assert_eq!(queued(&sql, &sha).await, 0);
    let stored: bool = sql
        .query_one("select stored from blobs where sha256 = $1", &[&sha.as_slice()])
        .await
        .unwrap()
        .get(0);
    assert!(!stored, "the missing object was found and stored cleared");
}

#[tokio::test]
async fn a_lock_session_found_ended_is_replaced_without_a_failed_lock() {
    // a writer's check finds its lock lost (the session's backend ended): the next lock opens a new session at once
    // instead of failing once on the dead one
    let db = common::fresh_db("lockbroken").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let a = blobs::sha256(b"a");
    let lock = r.app().blobs.shared(&a).await.unwrap();
    let sql = r.pool().get().await.unwrap();
    let pid: i32 = sql
        .query_one(
            "select pid from pg_locks where locktype = 'advisory' and classid = 1835166818::oid and granted
               and database = (select oid from pg_database where datname = current_database())",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    sql.execute("select pg_terminate_backend($1)", &[&pid]).await.unwrap();
    assert!(
        common::eventually(Duration::from_secs(5), async || lock.check(&sql).await.is_err()).await,
        "the check finds the lock lost"
    );
    drop(lock);
    let b = blobs::sha256(b"b");
    let next = r
        .app()
        .blobs
        .shared(&b)
        .await
        .expect("a new session, not a failed lock");
    next.check(&sql).await.unwrap();
    next.release().await;
}
