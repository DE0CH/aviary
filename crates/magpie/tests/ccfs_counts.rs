//! magpie 6.10's write path by its database work, which does not depend on the machine: every Claude Code write is
//! one transaction with the same statements whether it is the transcript's first line or its hundredth, 10 KiB or a
//! 1 MiB image line. A path that re-read the file, wrote a line twice or made a round trip per line already stored
//! shows here as a different count on every runner. (`ccfs_perf.rs` prints the latencies on the replicated server; they
//! are a report, never the check: a time on a shared runner measures the runner.)
//!
//! The replica under test follows: the test holds magpie's job lock itself, so no leader job and no hermit feed share
//! its pool, and with [`common::idle`] intervals no every-replica pass comes due; the pool's counts are this test's
//! requests alone.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_truncation)]

mod common;

use aviary_pg::Counts;
use base64::Engine as _;
use common::cc::{Sandbox, main_path, set_main};
use serde_json::json;

const ALICE: &str = "alice@example.org";

fn assistant_line(i: usize) -> Vec<u8> {
    let text = "x".repeat(10_100);
    let mut l = serde_json::to_vec(&json!({"type": "assistant", "uuid": format!("a{i}"),
        "message": {"model": "m", "content": [{"type": "text", "text": format!("{i} {text}")}]}}))
    .unwrap();
    l.push(b'\n');
    l
}

/// A user line carrying a 768 KiB PNG-like image (over 1 MiB of base64), distinct per `i`.
fn image_line(i: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut x: u32 = i.wrapping_mul(2_654_435_761).wrapping_add(1);
    while bytes.len() < 786_432 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
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
    l
}

#[tokio::test]
async fn a_cc_write_costs_the_same_database_work_wherever_it_lands() {
    let db = common::fresh_db("cc_counts").await;
    let lock = common::hold_job_lock(&db.url).await;

    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    let p = main_path();
    set_main(&sql, s.conversation, &p).await;

    let mut at = 0u64;
    let mut write = async |line: &[u8]| -> Counts {
        let before = r.pool().counts();
        s.write(&p, at, line).await.unwrap();
        at += line.len() as u64;
        r.pool().counts() - before
    };
    let mut small = Vec::new();
    for i in 0..40 {
        small.push(write(&assistant_line(i)).await);
    }
    let mut big = Vec::new();
    for i in 0..8 {
        big.push(write(&image_line(i)).await);
    }
    for i in 40..60 {
        small.push(write(&assistant_line(i)).await);
    }
    println!("a 10 KiB append: {:?}; a 1 MiB image line: {:?}", small[0], big[0]);
    // the first write of a file may set it up; from the second on, every write of a kind costs the same
    assert!(small[1..].iter().all(|c| *c == small[1]), "10 KiB appends: {small:?}");
    assert!(big.iter().all(|c| *c == big[0]), "1 MiB image lines: {big:?}");
    assert_eq!(small[1].transactions, 1, "one transaction per append: {:?}", small[1]);
    assert_eq!(big[0].transactions, 1, "one transaction per image line: {:?}", big[0]);
    assert_eq!(s.read(&p).await.unwrap().len() as u64, at);
    lock.release().await;
}
