//! Memory under concurrent large image uploads (magpie 5.5, 10.2, 18.2's 1 GiB pod limit): four 40 MP JPEG uploads
//! at once (each turned upright by its EXIF orientation and given a model copy), then the four thumbnails at once,
//! keep the process's peak RSS under 1 GiB. Ignored in the ordinary suite (a debug build decodes 40 MP far too
//! slowly); ci.yml's `magpie-perf` job runs it with a release build. The replica runs in this process, so the peak
//! RSS (`VmHWM`) covers it, the test's own image generation included.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::format_collect
)]

mod common;

use std::time::Instant;

use magpie_api::uploads::{BlobQuery, BlobVariant, UploadKind};

const ALICE: &str = "alice@example.org";
const LIMIT: u64 = 1 << 30;

/// A JPEG of `w`×`h` (a gradient with a seed, so each is its own blob) with EXIF orientation 6 (APP1 after SOI).
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

#[tokio::test]
#[ignore = "release build only: ci.yml magpie-perf"]
async fn four_40_mp_uploads_at_once_stay_under_1_gib() {
    let db = common::fresh_db("uploadmem").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    // 7296 x 5472 = 39.9 MP, the decode limit's size
    let (w, h) = (7296, 5472);
    let bodies: Vec<Vec<u8>> = (0..4u8).map(|i| jpeg_turned(w, h, i * 60)).collect();
    let before = peak_rss();
    let start = Instant::now();
    let mut tasks = Vec::new();
    for b in bodies {
        let c = r.pigeon();
        tasks.push(tokio::spawn(async move {
            c.person(ALICE)
                .upload("photo.jpg", UploadKind::Image, "image/jpeg", b)
                .await
                .unwrap()
        }));
    }
    let mut ups = Vec::new();
    for task in tasks {
        ups.push(task.await.unwrap());
    }
    let uploaded = start.elapsed();
    for up in &ups {
        // upright: the EXIF orientation turned the picture
        assert_eq!((up.width, up.height), (Some(h), Some(w)));
    }
    let mut thumbs = Vec::new();
    for up in &ups {
        let sha: Vec<u8> = r
            .pool()
            .get()
            .await
            .unwrap()
            .query_one("select blob from uploads where id = $1", &[&up.upload_id])
            .await
            .unwrap()
            .get(0);
        let hex: String = sha.iter().map(|b| format!("{b:02x}")).collect();
        let c = r.pigeon();
        thumbs.push(tokio::spawn(async move {
            let resp = c
                .blob(
                    &hex,
                    &BlobQuery {
                        variant: Some(BlobVariant::Thumb),
                    },
                    None,
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
            resp.bytes().await.unwrap().len()
        }));
    }
    for th in thumbs {
        assert!(th.await.unwrap() > 0);
    }
    let peak = peak_rss();
    println!(
        "4 x 40 MP: uploads {uploaded:?}, all {:?}; peak RSS {} MiB (before the uploads {} MiB)",
        start.elapsed(),
        peak >> 20,
        before >> 20
    );
    assert!(peak < LIMIT, "peak RSS {} MiB", peak >> 20);
}
