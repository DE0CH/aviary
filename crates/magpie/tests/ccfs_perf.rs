//! magpie 6.10's write acceptance: p50 <= 15 ms for a 10 KiB append and p99 <= 150 ms for a write carrying a 1 MiB
//! image line, with synchronous replication on; and 6.11's read acceptance: an 87 MB transcript (about 50 MB of
//! distinct blobs, every one only in the bucket) streams in under 5 s. Ignored in the ordinary suite; ci.yml's
//! `magpie-perf` job runs them with a release build against `.github/ci/postgres-sync.sh` (a primary and a synchronous
//! standby), and the write test refuses to measure against a server that does not replicate synchronously.
//!
//! Measured at the client over loopback HTTP, so the figures include the HTTP round trip the handler-side numbers
//! do not.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

mod common;

use std::time::{Duration, Instant};

use base64::Engine as _;
use common::cc::{Sandbox, main_path, set_main};
use serde_json::json;

const ALICE: &str = "alice@example.org";

fn pct(sorted: &[Duration], p: f64) -> Duration {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

#[tokio::test]
#[ignore = "run by ci.yml's magpie-perf job: a release build against a synchronously replicated Postgres"]
async fn cc_write_latency_with_synchronous_replication() {
    let db = common::fresh_db("cc_perf").await;
    let sql = common::sql(&db.url);
    let conn = sql.get().await.unwrap();
    let sync: i64 = conn
        .query_one(
            "select count(*) from pg_stat_replication where sync_state = 'sync'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let commit: String = conn.query_one("show synchronous_commit", &[]).await.unwrap().get(0);
    assert!(
        sync >= 1 && commit != "off" && commit != "local",
        "not a synchronously replicated server"
    );
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    let p = main_path();
    set_main(&sql, s.conversation, &p).await;

    // 10 KiB appends: assistant lines (each an event and a row)
    let mut at = 0u64;
    let mut small = Vec::new();
    for i in 0..400 {
        let text = "x".repeat(10_100);
        let mut l = serde_json::to_vec(&json!({"type": "assistant", "uuid": format!("a{i}"),
            "message": {"model": "m", "content": [{"type": "text", "text": format!("{i} {text}")}]}}))
        .unwrap();
        l.push(b'\n');
        let t0 = Instant::now();
        s.write(&p, at, &l).await.unwrap();
        small.push(t0.elapsed());
        at += l.len() as u64;
    }
    // 1 MiB image lines (a Read result; 500, so the p99 is the 5th slowest), each written as Claude Code's two FUSE writes would be: here one call of
    // the whole line, the worst case for one transaction
    let mut big = Vec::new();
    let mut x: u32 = 1;
    for i in 0..500 {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        while bytes.len() < 786_432 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345 + i);
            bytes.push((x >> 16) as u8);
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mut l = serde_json::to_vec(
            &json!({"type": "user", "uuid": format!("i{i}"), "message": {"content": [
            {"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": [{"type": "image",
             "source": {"type": "base64", "media_type": "image/png", "data": b64}}]}]}}),
        )
        .unwrap();
        l.push(b'\n');
        assert!(l.len() > 1 << 20);
        let t0 = Instant::now();
        s.write(&p, at, &l).await.unwrap();
        big.push(t0.elapsed());
        at += l.len() as u64;
    }
    small.sort();
    big.sort();
    let (p50, p99) = (pct(&small, 0.5), pct(&big, 0.99));
    println!(
        "10 KiB append: p50 {p50:?} p99 {:?}; 1 MiB image line: p50 {:?} p99 {p99:?}",
        pct(&small, 0.99),
        pct(&big, 0.5)
    );
    println!("slowest 1 MiB writes: {:?}", &big[big.len() - 5..]);
    assert!(p50 <= Duration::from_millis(15), "10 KiB append p50 {p50:?}");
    assert!(p99 <= Duration::from_millis(150), "1 MiB image line p99 {p99:?}");
    assert_eq!(s.read(&p).await.unwrap().len() as u64, at);
}

/// An image line with `bytes` of distinct PNG-like data, base64 in a tool result (one blob).
fn image_line(i: u32, bytes: usize) -> Vec<u8> {
    let mut data = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut x = i.wrapping_mul(2_654_435_761).wrapping_add(1);
    while data.len() < bytes {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        data.push((x >> 16) as u8);
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    let mut l = serde_json::to_vec(
        &json!({"type": "user", "uuid": format!("i{i}"), "message": {"content": [
        {"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": [{"type": "image",
         "source": {"type": "base64", "media_type": "image/png", "data": b64}}]}]}}),
    )
    .unwrap();
    l.push(b'\n');
    l
}

#[tokio::test]
#[ignore = "run by ci.yml's magpie-perf job: a release build"]
async fn an_87_mb_transcript_streams_from_the_bucket_in_under_5_s() {
    let db = common::fresh_db("cc_read_perf").await;
    let t = common::Tokens::new();
    // the bucket 30 ms away (each request's first byte), as S3 is from a pod
    let bucket = common::test_bucket(&db.url).await;
    let s3: std::net::SocketAddr = bucket.endpoint.trim_start_matches("http://").parse().unwrap();
    let proxy = common::CutProxy::start_with_latency(s3, Duration::from_millis(30)).await;
    let mut far = bucket.clone();
    far.endpoint = format!("http://{}", proxy.addr);
    let r = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::fast(), far.clone()).await;
    let s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    let p = main_path();
    // 400 image lines of 96 KiB (128 KiB of base64 each: 52 MB of distinct blobs) between assistant lines of 10 KiB
    // text (3,400 of them, 35 MB), written about 1 MiB at a time
    let mut content = Vec::new();
    let mut pending = Vec::new();
    let mut at = 0u64;
    for i in 0..3_800u32 {
        let l = if i % 19 == 0 || i % 19 == 9 {
            image_line(i, 96 * 1024)
        } else {
            let mut l = serde_json::to_vec(&json!({"type": "assistant", "uuid": format!("a{i}"),
                "message": {"model": "m", "content": [{"type": "text", "text": format!("{i} {}", "y".repeat(10_200))}]}}))
            .unwrap();
            l.push(b'\n');
            l
        };
        pending.extend_from_slice(&l);
        if pending.len() >= 1 << 20 {
            s.write(&p, at, &pending).await.unwrap();
            at += pending.len() as u64;
            content.append(&mut pending);
        }
    }
    s.write(&p, at, &pending).await.unwrap();
    content.append(&mut pending);
    assert!(content.len() > 87_000_000, "{} bytes", content.len());
    let blobs: (i64, i64) = {
        let row = common::sql(&db.url)
            .get()
            .await
            .unwrap()
            .query_one("select count(*), coalesce(sum(size), 0)::bigint from blobs", &[])
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert!(blobs.0 >= 400 && blobs.1 > 50_000_000, "{blobs:?}");
    // every blob uploaded and out of staging
    let sql = common::sql(&db.url);
    sql.get()
        .await
        .unwrap()
        .execute("update blob_staging set next_at = now()", &[])
        .await
        .unwrap();
    assert!(
        common::eventually(Duration::from_secs(120), async || {
            sql.get()
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
    // a fresh replica (nothing in its read cache), as a resume in a fresh sandbox reads it
    let r2 = common::replica_full(&db.url, &t, "http://127.0.0.1:1", common::fast(), far).await;
    let mut s2 = Sandbox::new(&r2, s.conversation, s.id, ALICE);
    s2.lease = s.lease;
    let t0 = Instant::now();
    let got = s2.read(&p).await.unwrap();
    let took = t0.elapsed();
    println!(
        "read of {} bytes ({} blobs, {} bytes of them, each from the bucket): {took:?}",
        got.len(),
        blobs.0,
        blobs.1
    );
    assert!(got == content, "the file differs");
    assert!(took < Duration::from_secs(5), "an 87 MB read took {took:?}");
}
