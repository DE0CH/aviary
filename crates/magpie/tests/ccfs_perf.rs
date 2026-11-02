//! magpie 6.10's write acceptance: p50 <= 15 ms for a 10 KiB append and p99 <= 150 ms for a write carrying a 1 MiB
//! image line, with synchronous replication on. Ignored in the ordinary suite; ci.yml's `magpie-perf` job runs it
//! with a release build against `.github/ci/postgres-sync.sh` (a primary and a synchronous standby), and it refuses
//! to measure against a server that does not replicate synchronously.
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
    // 1 MiB image lines (a Read result), each written as Claude Code's two FUSE writes would be: here one call of
    // the whole line, the worst case for one transaction
    let mut big = Vec::new();
    let mut x: u32 = 1;
    for i in 0..120 {
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
    assert!(p50 <= Duration::from_millis(15), "10 KiB append p50 {p50:?}");
    assert!(p99 <= Duration::from_millis(150), "1 MiB image line p99 {p99:?}");
    assert_eq!(s.read(&p).await.unwrap().len() as u64, at);
}
