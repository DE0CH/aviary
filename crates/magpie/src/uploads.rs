//! Uploads (M13, magpie 10): a file or image a person attaches, stored as a blob before the turn that uses it.
//!
//! The body (at most 50 MiB) is read whole (the blob's key is its sha256, and an image is decoded from the whole
//! body: README "Where this differs"), hashed and put in the bucket; then the blob rows, the upload row and its
//! `upload` blob references commit together. An image also gets its model copy (magpie 10.2), which is its `display`
//! variant; when the original is already fit for the model it is its own model copy. Unattached uploads are deleted
//! by retention after 24 h; admission (magpie 8.2) attaches them to a turn.

use axum::body::{Body, Bytes};
use magpie_api::errors::ErrorCode;
use magpie_api::ops::ViewAsEvent;
use magpie_api::uploads::{UPLOAD_MAX_BYTES, UploadKind, UploadMeta, UploadResponse};
use uuid::Uuid;

use crate::app::Magpie;
use crate::blobs::images::{self, ImageError};
use crate::blobs::{self, RawBlob, SharedLock};
use crate::conversations::{Person, ensure_person};
use crate::error::{Error, Result};
use crate::files::{raw_facts, read_body_held};
use crate::viewas;

fn unsupported(e: &ImageError) -> Error {
    Error::code(ErrorCode::UnsupportedType, e.to_string())
}

/// The type of an upload: sniffed, else from the name's extension, else as sent.
#[must_use]
pub fn mime_of(bytes: &[u8], name: &str, content_type: &str) -> String {
    if let Some(m) = images::sniff(bytes) {
        return m.to_owned();
    }
    let by_name = magpie_api::mime::from_name(name);
    if by_name != "application/octet-stream" {
        return by_name.to_owned();
    }
    let sent = content_type.split(';').next().unwrap_or("").trim();
    if sent.is_empty() || sent.contains(char::is_whitespace) {
        by_name.to_owned()
    } else {
        sent.to_lowercase()
    }
}

/// What an image upload adds: its type, its upright dimensions and its model copy (`None`: the original).
struct ImageFacts {
    mime: String,
    width: u32,
    height: u32,
    copy: Option<images::Copy>,
}

async fn image_facts(app: &Magpie, bytes: &Bytes, name: &str) -> Result<ImageFacts> {
    let sniffed = images::sniff(bytes).filter(|m| images::is_image(m));
    let by_name = Some(magpie_api::mime::from_name(name)).filter(|m| images::is_image(m));
    let mime = sniffed
        .or(by_name)
        .ok_or_else(|| Error::code(ErrorCode::UnsupportedType, "not a supported image"))?
        .to_owned();
    let raw = bytes.clone();
    let ty = mime.clone();
    let memory = images::decode_memory(bytes, &mime);
    let made = app
        .blobs
        .decode(memory, move || {
            let (width, height) = images::dimensions(&raw, &ty)?;
            let turned = (5..=8).contains(&images::orientation(&raw, &ty));
            let copy = images::model_copy(&raw, &ty)?;
            Ok(if turned {
                (height, width, copy)
            } else {
                (width, height, copy)
            })
        })
        .await?
        .map_err(|e| unsupported(&e))?;
    Ok(ImageFacts {
        mime,
        width: made.0,
        height: made.1,
        copy: made.2,
    })
}

/// M13 `POST /v1/uploads`.
///
/// # Errors
/// 413 `too_large`; 415 `unsupported_type` for an image upload that is not a supported image or is over the decode
/// limit; 503 when the bucket cannot be reached; a statement error.
pub async fn upload(app: &Magpie, person: &Person, meta: UploadMeta, body: Body) -> Result<UploadResponse> {
    // the body's memory is held until the upload is recorded (the bytes live that long)
    let (bytes, _held) = read_body_held(app, body, UPLOAD_MAX_BYTES).await?;
    let name = meta.name.clone();
    let (mime, dims, copy) = match meta.kind {
        UploadKind::Image => {
            let f = image_facts(app, &bytes, &name).await?;
            (f.mime, Some((f.width, f.height)), f.copy)
        }
        UploadKind::File => (mime_of(&bytes, &name, &meta.content_type), None, None),
    };
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let mut facts = raw_facts(blobs::sha256(&bytes), &bytes, Some(&mime));
    facts.mime = Some(mime.clone());
    let (sha, lock) = app.blobs.put_bytes(&app.pool, bytes).await?;
    let mut locks: Vec<SharedLock> = vec![lock];
    let model = match copy {
        Some(c) => {
            let model = RawBlob {
                sha: blobs::sha256(&c.bytes),
                size: i64::try_from(c.bytes.len()).unwrap_or(i64::MAX),
                kind: "media",
                mime: Some(c.mime.to_owned()),
                width: Some(c.width),
                height: Some(c.height),
            };
            match app.blobs.put_bytes(&app.pool, Bytes::from(c.bytes)).await {
                Ok((_, l)) => locks.push(l),
                Err(e) => {
                    for l in locks {
                        l.release().await;
                    }
                    return Err(e);
                }
            }
            Some(model)
        }
        None if meta.kind == UploadKind::Image => Some(facts.clone()),
        None => None,
    };
    let id = Uuid::now_v7();
    let role = match meta.kind {
        UploadKind::Image => "image",
        UploadKind::File => "file",
    };
    let size_i = i64::try_from(size).unwrap_or(i64::MAX);
    let (w, h) = dims.map_or((None, None), |(w, h)| (i32::try_from(w).ok(), i32::try_from(h).ok()));
    let (name_r, mime_r, facts_r, model_r, locks_r) = (&name, &mime, &facts, &model, &locks);
    let r = aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        ensure_person(t, &person.email).await?;
        blobs::record_stored(t, facts_r).await?;
        if let Some(m) = model_r {
            blobs::record_stored(t, m).await?;
        }
        let model_sha = model_r.as_ref().map(|m| m.sha.to_vec());
        t.execute(
            "insert into uploads (id, person, role, name, mime, size, blob, model_blob, width, height)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &id,
                &person.email,
                &role,
                name_r,
                mime_r,
                &size_i,
                &sha.as_slice(),
                &model_sha,
                &w,
                &h,
            ],
        )
        .await?;
        blobs::reference(t, id, "upload", &sha).await?;
        if let Some(m) = model_r {
            blobs::reference(t, id, "upload", &m.sha).await?;
        }
        viewas::record(t, person, ViewAsEvent::Upload, None, None, Some(size)).await?;
        for l in locks_r {
            l.check(t).await?;
        }
        Ok(())
    })
    .await;
    for l in locks {
        l.release().await;
    }
    r?;
    Ok(UploadResponse {
        upload_id: id,
        path: format!("uploads/{id}/{name}"),
        name,
        size,
        mime_type: mime,
        width: dims.map(|d| d.0),
        height: dims.map(|d| d.1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types() {
        assert_eq!(
            mime_of(b"\x89PNG\r\n\x1a\n....", "x.bin", "application/octet-stream"),
            "image/png"
        );
        assert_eq!(mime_of(b"a,b", "q3.csv", "application/octet-stream"), "text/csv");
        assert_eq!(mime_of(b"{}", "noext", "Application/Thing; x=1"), "application/thing");
        assert_eq!(mime_of(b"{}", "noext", ""), "application/octet-stream");
    }
}
