//! Memory under whole reads of an image-heavy transcript (magpie 18.2's 1 GiB pod limit): three reads at once of a
//! 380 MiB transcript of 96 image lines (3 MiB of image each, base64 in the line) keep the process's peak RSS under
//! 768 MiB. A read batch is cut at `limits::CC_READ_BATCH_BYTES` as well as at `limits::CC_READ_BATCH` lines: cut by
//! count alone, one batch was every line of this file, its blobs and the spliced answer alive together (audit of
//! 2026-11-16, finding 5). Its own test binary (the peak, `VmHWM`, is the process's), ignored in the ordinary suite;
//! ci.yml's `magpie-perf` job runs it with a release build.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_truncation)]

mod common;

use base64::Engine as _;
use common::cc::{Sandbox, home, main_path, set_main};
use magpie_api::sandbox::CcFileQuery;
use serde_json::json;

const ALICE: &str = "alice@example.org";
const LIMIT: u64 = 768 << 20;
const LINES: u32 = 96;
const IMAGE: usize = 3 << 20;

/// The process's peak resident set (`VmHWM`), bytes.
fn peak_rss() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    let kib: u64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    kib * 1024
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
#[ignore = "release build only: ci.yml magpie-perf"]
async fn whole_reads_of_an_image_heavy_transcript_at_once_stay_under_768_mib() {
    let db = common::fresh_db("cclinesmem").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    let path = main_path();
    let sql = common::sql(&db.url);
    set_main(&sql, s.conversation, &path).await;
    let mut at = 0u64;
    for i in 0..LINES {
        let l = image_line(i, IMAGE);
        s.write(&path, at, &l).await.unwrap();
        at += l.len() as u64;
    }
    // read at once: the blobs are partly staged and partly uploaded, as right after a turn wrote them
    let before = peak_rss();
    let mut reads = Vec::new();
    for _ in 0..3 {
        let calls = s.calls();
        let home_path = home(&path);
        reads.push(async move {
            let mut res = calls
                .cc_file(&CcFileQuery {
                    path: home_path,
                    offset: None,
                    length: None,
                })
                .await
                .unwrap();
            // read as it arrives and let go: the test never holds the file
            let mut len = 0u64;
            while let Some(c) = res.chunk().await.unwrap() {
                len += c.len() as u64;
            }
            len
        });
    }
    for len in futures_util::future::join_all(reads).await {
        assert_eq!(len, at, "a whole read came back short");
    }
    let peak = peak_rss();
    println!(
        "3 x {} MiB transcript reads at once: peak RSS {} MiB (before {} MiB)",
        at >> 20,
        peak >> 20,
        before >> 20
    );
    assert!(peak < LIMIT, "peak RSS {} MiB", peak >> 20);
}
