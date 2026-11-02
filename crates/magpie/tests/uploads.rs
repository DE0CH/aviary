//! Uploads (M13, magpie 10): limits and types, the model copy's bounds and EXIF rotation, HEIC, the decode cap,
//! the display variant of an upload, view-as recording, retention of unattached uploads.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::format_collect
)]

mod common;

use magpie_api::errors::ErrorCode;
use magpie_api::ops::{ViewAsEvent, ViewAsLogQuery, ViewAsRequest};
use magpie_api::uploads::{BlobQuery, BlobVariant, UploadKind, UploadResponse};

const ALICE: &str = "alice@example.com";
const HEIC: &[u8] = include_bytes!("fixtures/photo-640x480.heic");

/// A PNG of `w`×`h`: a gradient, `alpha` adds a transparent corner, `noise` makes it incompressible.
fn png(w: u32, h: u32, alpha: bool, noise: bool) -> Vec<u8> {
    let mut seed: u32 = 0x1234_5678;
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let a = if alpha && x < w / 4 && y < h / 4 { 0 } else { 255 };
        if noise {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let [r, g, b, _] = seed.to_le_bytes();
            return image::Rgba([r, g, b, a]);
        }
        image::Rgba([
            u8::try_from(x * 255 / w.max(1)).unwrap(),
            u8::try_from(y * 255 / h.max(1)).unwrap(),
            128,
            a,
        ])
    });
    let img = image::DynamicImage::ImageRgba8(img);
    let img = if alpha {
        img
    } else {
        image::DynamicImage::ImageRgb8(img.to_rgb8())
    };
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// A JPEG of `w`×`h` with an EXIF orientation tag `o` (APP1 right after SOI).
fn jpeg_oriented(w: u32, h: u32, o: u16) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, _| image::Rgb([u8::try_from(x * 255 / w).unwrap(), 0, 0]));
    let mut j = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut j, 90)
        .encode_image(&img)
        .unwrap();
    let mut tiff = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
    tiff.extend_from_slice(&o.to_be_bytes());
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

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// A PNG that claims `w`×`h` (a valid header, no pixel data).
fn png_header_only(w: u32, h: u32) -> Vec<u8> {
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend_from_slice(&13u32.to_be_bytes());
    out.extend_from_slice(&ihdr);
    out.extend_from_slice(&crc32(&ihdr).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(b"IEND");
    out.extend_from_slice(&crc32(b"IEND").to_be_bytes());
    out
}

async fn upload(
    r: &common::Replica,
    name: &str,
    kind: UploadKind,
    ct: &str,
    bytes: Vec<u8>,
) -> Result<UploadResponse, u16> {
    r.pigeon()
        .person(ALICE)
        .upload(name, kind, ct, bytes)
        .await
        .map_err(|e| e.code().map_or(0, ErrorCode::status))
}

async fn display(r: &common::Replica, up: &UploadResponse) -> (String, image::DynamicImage, usize) {
    let row = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select blob from uploads where id = $1", &[&up.upload_id])
        .await
        .unwrap();
    let sha: Vec<u8> = row.get(0);
    let hex: String = sha.iter().map(|b| format!("{b:02x}")).collect();
    let resp = r
        .pigeon()
        .blob(
            &hex,
            &BlobQuery {
                variant: Some(BlobVariant::Display),
            },
            None,
        )
        .await
        .unwrap();
    let ct = resp.headers()["content-type"].to_str().unwrap().to_owned();
    let bytes = resp.bytes().await.unwrap();
    (ct, image::load_from_memory(&bytes).unwrap(), bytes.len())
}

#[tokio::test]
async fn uploads_limits_types_and_model_copies() {
    let db = common::fresh_db("uploads").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;

    // a file: its type from its bytes, else its name
    let f = upload(
        &r,
        "Q3 報告.csv",
        UploadKind::File,
        "application/octet-stream",
        b"a,b\n1,2\n".to_vec(),
    )
    .await
    .unwrap();
    assert_eq!(f.mime_type, "text/csv");
    assert_eq!(f.path, format!("uploads/{}/Q3 報告.csv", f.upload_id));
    assert_eq!((f.size, f.width, f.height), (8, None, None));
    // an image sent as a file stays a file
    let as_file = upload(&r, "x.png", UploadKind::File, "image/png", png(10, 10, false, false))
        .await
        .unwrap();
    assert_eq!((as_file.mime_type.as_str(), as_file.width), ("image/png", None));
    let role: String = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select role from uploads where id = $1", &[&as_file.upload_id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(role, "file");

    // 413 over 50 MiB, 415 for an image upload that is not one
    let over = vec![0u8; usize::try_from(magpie_api::uploads::UPLOAD_MAX_BYTES).unwrap() + 1];
    assert_eq!(
        upload(&r, "big.bin", UploadKind::File, "application/octet-stream", over)
            .await
            .unwrap_err(),
        413
    );
    assert_eq!(
        upload(&r, "a.svg", UploadKind::Image, "image/svg+xml", b"<svg/>".to_vec())
            .await
            .unwrap_err(),
        415
    );
    assert_eq!(
        upload(&r, "a.png", UploadKind::Image, "image/png", b"not a png".to_vec())
            .await
            .unwrap_err(),
        415
    );
    // the 40-megapixel decode cap
    assert_eq!(
        upload(
            &r,
            "huge.png",
            UploadKind::Image,
            "image/png",
            png_header_only(8000, 6000)
        )
        .await
        .unwrap_err(),
        415
    );

    // a small model-native image is its own model copy (and display variant)
    let small = png(300, 200, false, false);
    let up = upload(&r, "small.png", UploadKind::Image, "image/png", small.clone())
        .await
        .unwrap();
    assert_eq!((up.width, up.height), (Some(300), Some(200)));
    let same: bool = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select blob = model_blob from uploads where id = $1", &[&up.upload_id])
        .await
        .unwrap()
        .get(0);
    assert!(same);

    // a large incompressible PNG: fitted to 2000 px, JPEG, within 3,932,160 bytes
    let big = png(3000, 2400, false, true);
    assert!(big.len() > 3_932_160);
    let up = upload(&r, "big.png", UploadKind::Image, "image/png", big)
        .await
        .unwrap();
    assert_eq!((up.width, up.height), (Some(3000), Some(2400)));
    let (ct, img, len) = display(&r, &up).await;
    assert_eq!(ct, "image/jpeg");
    assert_eq!((img.width(), img.height()), (2000, 1600));
    assert!(len <= 3_932_160, "{len}");

    // a transparent PNG over 2000 px stays PNG
    let up = upload(
        &r,
        "alpha.png",
        UploadKind::Image,
        "image/png",
        png(2400, 1200, true, false),
    )
    .await
    .unwrap();
    let (ct, img, _) = display(&r, &up).await;
    assert_eq!(ct, "image/png");
    assert_eq!((img.width(), img.height()), (2000, 1000));
    assert!(img.color().has_alpha());

    // EXIF orientation 6 (rotate 90°): the copy is upright, and the upload reports upright dimensions
    let up = upload(
        &r,
        "photo.jpg",
        UploadKind::Image,
        "image/jpeg",
        jpeg_oriented(400, 300, 6),
    )
    .await
    .unwrap();
    assert_eq!((up.width, up.height), (Some(300), Some(400)));
    let (ct, img, _) = display(&r, &up).await;
    assert_eq!(ct, "image/jpeg");
    assert_eq!((img.width(), img.height()), (300, 400));
    // the red gradient ran left to right; turned 90° clockwise it runs top to bottom
    let px = img.to_rgb8();
    assert!(
        px.get_pixel(150, 0).0[0] < 40 && px.get_pixel(150, 399).0[0] > 200,
        "not rotated"
    );

    // HEIC: decoded through libheif, its copy a JPEG
    let up = upload(&r, "IMG_0042.HEIC", UploadKind::Image, "image/heic", HEIC.to_vec())
        .await
        .unwrap();
    assert_eq!(up.mime_type, "image/heic");
    assert_eq!((up.width, up.height), (Some(640), Some(480)));
    let (ct, img, _) = display(&r, &up).await;
    assert_eq!(ct, "image/jpeg");
    assert_eq!((img.width(), img.height()), (640, 480));

    // the original and the model copy are referenced by the upload
    let refs: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from blob_refs where owner = $1 and owner_kind = 'upload'",
            &[&up.upload_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(refs, 2);
}

#[tokio::test]
async fn an_upload_while_viewing_as_is_recorded_and_unattached_uploads_expire() {
    let db = common::fresh_db("upview").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    // M17: the start is recorded; a bad request is 400
    let start = ViewAsRequest {
        admin: "Root@Example.com".into(),
        person: ALICE.into(),
        session: "s-1".into(),
        expires_at: magpie_api::Timestamp(time::OffsetDateTime::now_utc() + time::Duration::hours(1)),
    };
    r.pigeon().view_as(&start).await.unwrap();
    let mut bad = start.clone();
    bad.session = "x".repeat(65);
    assert_eq!(
        r.pigeon().view_as(&bad).await.unwrap_err().code(),
        Some(&ErrorCode::BadRequest)
    );
    let up = r
        .pigeon()
        .viewing_as(ALICE, "root@example.com", "s-1")
        .upload("notes.txt", UploadKind::File, "text/plain", b"hello".to_vec())
        .await
        .unwrap();
    let log = r
        .hermit()
        .service(None)
        .ops_view_as(&ViewAsLogQuery::default())
        .await
        .unwrap();
    let events: Vec<(ViewAsEvent, Option<u64>, String)> = log
        .view_as
        .iter()
        .map(|e| (e.event, e.chars, e.admin.clone()))
        .collect();
    assert_eq!(
        events,
        vec![
            (ViewAsEvent::Upload, Some(5), "root@example.com".to_owned()),
            (ViewAsEvent::Start, None, "root@example.com".to_owned()),
        ]
    );
    // a send while viewing as: its view_as_log row (the text's length, never the text) and the event's viewer, in
    // the send's own transaction
    let c = common::files::conversation(&r, ALICE, "v").await;
    let send = magpie_api::turns::SendRequest {
        client_message_id: "v:1".into(),
        text: "看看這個".into(),
        images: Vec::new(),
        files: Vec::new(),
    };
    r.pigeon()
        .viewing_as(ALICE, "root@example.com", "s-1")
        .send_turn(c, &send)
        .await
        .unwrap();
    let row = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select l.chars, l.seq, e.viewer, e.kind from view_as_log l
             join events e on e.conversation_id = l.conversation_id and e.seq = l.seq
             where l.event = 'send' and l.conversation_id = $1",
            &[&c],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<i32>>(0), Some(4));
    assert_eq!(row.get::<_, Option<String>>(2).as_deref(), Some("root@example.com"));
    assert_eq!(row.get::<_, String>(3), "user.sent");
    // 24 h later retention deletes the unattached upload and its references; the GC can then free the blob
    let sql = r.pool().get().await.unwrap();
    sql.execute("update uploads set created_at = now() - interval '25 hours'", &[])
        .await
        .unwrap();
    magpie::jobs::retention::pass(r.pool()).await.unwrap();
    let left: i64 = sql
        .query_one(
            "select (select count(*) from uploads) + (select count(*) from blob_refs where owner = $1)",
            &[&up.upload_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
    sql.execute("update blobs set last_ref_at = now() - interval '25 hours'", &[])
        .await
        .unwrap();
    assert_eq!(magpie::blobs::gc::free(r.pool(), &r.app().blobs).await.unwrap(), 1);
}
