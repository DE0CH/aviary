//! Helpers for the tests of blobs, uploads, files and shares (WP-15).

#![allow(dead_code, clippy::unwrap_used, clippy::missing_panics_doc)]

use magpie_api::conversations::CreateRequest;
use magpie_api::turns::SendRequest;
use uuid::Uuid;

use super::Replica;

pub fn empty(key: &str) -> CreateRequest {
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

pub async fn conversation(r: &Replica, person: &str, key: &str) -> Uuid {
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
pub fn png(w: u32, h: u32, alpha: bool) -> Vec<u8> {
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

/// A gateway call for `conversation`'s sandbox (the identity headers the gateway adds, magpie 3.3).
pub fn gateway(
    r: &Replica,
    method: reqwest::Method,
    path: &str,
    conversation: Uuid,
    person: &str,
    lease: Option<Uuid>,
) -> reqwest::RequestBuilder {
    let mut b = super::http()
        .request(method, format!("{}{path}", r.url))
        .bearer_auth(&r.tokens.gateway)
        .header("aviary-request", Uuid::now_v7().to_string())
        .header("aviary-protocol", "magpie-sandbox=1")
        .header("aviary-sandbox-id", sandbox_of(conversation).to_string())
        .header("aviary-sandbox-conversation", conversation.to_string())
        .header("aviary-sandbox-person", person);
    if let Some(l) = lease {
        b = b.header("aviary-lease", l.to_string());
    }
    b
}

/// The sandbox [`gateway`] names for `conversation`.
pub fn sandbox_of(conversation: Uuid) -> Uuid {
    Uuid::new_v5(&conversation, b"sandbox")
}

/// Gives `conversation`'s sandbox ([`sandbox_of`]) a lease, as `POST /v1/sandbox/lease` would.
pub async fn grant_lease(pool: &aviary_pg::Pool, conversation: Uuid) -> Uuid {
    let lease = Uuid::new_v4();
    pool.get()
        .await
        .unwrap()
        .execute(
            "update conversations set lease_id = $2, lease_sandbox = $3 where id = $1",
            &[&conversation, &lease, &sandbox_of(conversation)],
        )
        .await
        .unwrap();
    lease
}
