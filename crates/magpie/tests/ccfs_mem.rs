//! Memory under large byte-file reads (magpie 18.2's 1 GiB pod limit): three whole reads at once of a 200 MiB state
//! file that is not a lines file keep the process's peak RSS under 512 MiB. The file is read in slices of
//! `limits::CC_BYTES_SLICE` and streamed; read whole (its rows, its pages and the answer alive together, about three
//! times its size per request) two such reads took a replica past its limit. Its own test binary (the peak, `VmHWM`,
//! is the process's), ignored in the ordinary suite; ci.yml's `magpie-perf` job runs it with a release build.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::cc::{Sandbox, home};
use magpie_api::sandbox::CcFileQuery;

const ALICE: &str = "alice@example.org";
const LIMIT: u64 = 512 << 20;
const MIB: usize = 1 << 20;
const FILE_MIB: usize = 200;

/// The process's peak resident set (`VmHWM`), bytes.
fn peak_rss() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    let kib: u64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    kib * 1024
}

/// MiB `i` of the file: distinct per MiB, so a slice out of place shows.
fn piece(i: usize) -> Vec<u8> {
    let mut c = vec![u8::try_from(i % 251).unwrap(); MIB];
    c[..8].copy_from_slice(&u64::try_from(i).unwrap().to_le_bytes());
    c
}

#[tokio::test]
#[ignore = "release build only: ci.yml magpie-perf"]
async fn whole_reads_of_a_large_byte_file_at_once_stay_under_512_mib() {
    let db = common::fresh_db("ccmem").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let s = Sandbox::leased(&r, ALICE).await;
    let path = "big.bin";
    for i in 0..FILE_MIB {
        s.write(path, u64::try_from(i * MIB).unwrap(), &piece(i)).await.unwrap();
    }
    let before = peak_rss();
    let mut reads = Vec::new();
    for _ in 0..3 {
        let calls = s.calls();
        reads.push(async move {
            let mut res = calls
                .cc_file(&CcFileQuery {
                    path: home(path),
                    offset: None,
                    length: None,
                })
                .await
                .unwrap();
            assert_eq!(res.content_length(), Some(u64::try_from(FILE_MIB * MIB).unwrap()));
            // read as it arrives and let go: the test never holds the file
            let (mut len, mut held) = (0usize, Vec::new());
            while let Some(c) = res.chunk().await.unwrap() {
                held.extend_from_slice(&c);
                while held.len() >= MIB {
                    let rest = held.split_off(MIB);
                    assert!(held == piece(len / MIB), "MiB {} came back changed", len / MIB);
                    len += MIB;
                    held = rest;
                }
            }
            assert_eq!(held.len(), 0, "a whole number of MiB");
            assert_eq!(len, FILE_MIB * MIB);
        });
    }
    futures_util::future::join_all(reads).await;
    let peak = peak_rss();
    println!(
        "3 x {FILE_MIB} MiB byte-file reads at once: peak RSS {} MiB (before {} MiB)",
        peak >> 20,
        before >> 20
    );
    assert!(peak < LIMIT, "peak RSS {} MiB", peak >> 20);
}
