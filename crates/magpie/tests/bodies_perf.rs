//! Memory under many large buffered bodies at once (magpie 18.2's 1 GiB pod limit, the buffered-body budget
//! `limits::BODY_MEMORY_BYTES`): with the read cache full, 12 x 50 MiB file uploads, 8 x 64 MiB sandbox blob `PUT`s
//! and two 40 MP image uploads at once keep the process's peak RSS under 1 GiB. Its own test binary (the peak,
//! `VmHWM`, is the process's), ignored in the ordinary suite; ci.yml's `magpie-perf` job runs it with a release build.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::format_collect)]

mod common;

use std::time::Instant;

use magpie_api::uploads::{BlobQuery, UploadKind};

const ALICE: &str = "alice@example.org";
const LIMIT: u64 = 1 << 30;

/// A JPEG of `w`x`h` (a gradient with a seed) with EXIF orientation 6.
fn jpeg_turned(w: u32, h: u32, seed: u8) -> Vec<u8> {
    let mut j = Vec::new();
    {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([
                u8::try_from(x * 255 / w).unwrap(),
                u8::try_from(y * 255 / h).unwrap(),
                seed,
            ])
        });
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut j, 80)
            .encode_image(&img)
            .unwrap();
    }
    let mut tiff = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
    tiff.extend_from_slice(&6u16.to_be_bytes());
    tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let len = u16::try_from(app1.len() + 2).unwrap();
    let mut out = j[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&j[2..]);
    out
}

/// The process's peak resident set (`VmHWM`), bytes.
fn peak_rss() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    let kib: u64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    kib * 1024
}

const MIB: usize = 1 << 20;

/// Chunk `i` of the body `seed` (1 MiB; distinct per seed and chunk).
fn chunk(seed: u32, i: usize) -> Vec<u8> {
    let mut c = vec![u8::try_from(i % 251).unwrap(); MIB];
    c[..4].copy_from_slice(&seed.to_le_bytes());
    c[4..12].copy_from_slice(&u64::try_from(i).unwrap().to_le_bytes());
    c
}

/// A body of `mib` MiB made chunk by chunk as it is sent (the test never holds it whole).
fn streamed(seed: u32, mib: usize) -> reqwest::Body {
    reqwest::Body::wrap_stream(futures_util::stream::iter(
        (0..mib).map(move |i| Ok::<_, std::io::Error>(chunk(seed, i))),
    ))
}

/// The sha256 (hex) of [`streamed`]'s body.
fn sha_of(seed: u32, mib: usize) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    for i in 0..mib {
        h.update(chunk(seed, i));
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
#[ignore = "release build only: ci.yml magpie-perf"]
async fn many_large_bodies_at_once_with_the_cache_full_stay_under_1_gib() {
    let db = common::fresh_db("bodymem").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let images: Vec<Vec<u8>> = (0..2u8).map(|i| jpeg_turned(7296, 5472, i * 90)).collect();
    // the read cache filled: 6 x 50 MiB files uploaded and read back (256 MiB of them stay cached)
    for seed in 0..6u32 {
        let up = r
            .pigeon()
            .person(ALICE)
            .upload(
                "fill.bin",
                UploadKind::File,
                "application/octet-stream",
                streamed(seed, 50),
            )
            .await
            .unwrap();
        assert_eq!(up.size, 50 << 20);
        let mut resp = r
            .pigeon()
            .blob(&sha_of(seed, 50), &BlobQuery { variant: None }, None)
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let mut n = 0;
        while let Some(c) = resp.chunk().await.unwrap() {
            n += c.len();
        }
        assert_eq!(n, 50 << 20);
    }
    let c = common::files::conversation(&r, ALICE, "k").await;
    let lease = common::files::grant_lease(r.pool(), c).await;
    let before = peak_rss();
    let start = Instant::now();
    let mut tasks = Vec::new();
    // 12 x 50 MiB file uploads, 8 x 64 MiB sandbox blob PUTs and two 40 MP images, all at once
    for seed in 100..112u32 {
        let p = r.pigeon();
        tasks.push(tokio::spawn(async move {
            let up = p
                .person(ALICE)
                .upload(
                    "big.bin",
                    UploadKind::File,
                    "application/octet-stream",
                    streamed(seed, 50),
                )
                .await
                .unwrap();
            assert_eq!(up.size, 50 << 20);
        }));
    }
    for seed in 200..208u32 {
        let req = common::files::gateway(
            &r,
            reqwest::Method::PUT,
            &format!("/v1/sandbox/blobs/{}", sha_of(seed, 64)),
            c,
            ALICE,
            Some(lease),
        )
        .body(streamed(seed, 64));
        tasks.push(tokio::spawn(async move {
            assert_eq!(req.send().await.unwrap().status(), 201);
        }));
    }
    for b in images {
        let p = r.pigeon();
        tasks.push(tokio::spawn(async move {
            p.person(ALICE)
                .upload("photo.jpg", UploadKind::Image, "image/jpeg", b)
                .await
                .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let peak = peak_rss();
    println!(
        "12 x 50 MiB uploads, 8 x 64 MiB sandbox PUTs, 2 x 40 MP, cache full: {:?}; peak RSS {} MiB (before {} MiB)",
        start.elapsed(),
        peak >> 20,
        before >> 20
    );
    assert!(peak < LIMIT, "peak RSS {} MiB", peak >> 20);
}
