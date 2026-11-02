//! Files magpie serves: blobs by id (M15) and to the sandbox, the sandbox's blob `PUT`s (magpie 5.4, 11.2).

use axum::body::{Body, Bytes};
use magpie_api::errors::ErrorCode;
use magpie_api::server::Ctx;
use magpie_api::uploads::{BlobVariant, SandboxBlobAs};
use magpie_api::{Admitted, Empty};

use crate::app::Magpie;
use crate::blobs::read::Served;
use crate::blobs::{self, RawBlob, Sha, images};
use crate::error::{Error, Result};
use crate::sandbox;

/// Reads `body`, at most `max` bytes (413 `too_large` beyond).
///
/// # Errors
/// That refusal; 400 when the body cannot be read.
pub async fn read_body(body: Body, max: u64) -> Result<Bytes> {
    let limit = usize::try_from(max).unwrap_or(usize::MAX);
    match axum::body::to_bytes(body, limit).await {
        Ok(b) => Ok(b),
        Err(e) => {
            let too_large = std::error::Error::source(&e)
                .is_some_and(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
                || e.to_string().contains("length limit");
            if too_large {
                Err(Error::code(ErrorCode::TooLarge, format!("over {max} bytes")))
            } else {
                Err(Error::bad_request(format!("body: {e}")))
            }
        }
    }
}

/// The blob of a hex id (404 when it is not one, or no such blob exists).
async fn blob_row(app: &Magpie, hex: &str) -> Result<blobs::BlobRow> {
    let sha = blobs::from_hex(hex).ok_or_else(Error::not_found)?;
    let conn = app.pool.get().await?;
    blobs::row(&conn, &sha).await?.ok_or_else(Error::not_found)
}

/// M15: the blob `hex` in `variant`, if anything references it (a purged conversation's unshared blobs disappear at
/// once for readers, before the GC frees the bytes).
///
/// # Errors
/// 404, 415, 503; a statement error.
pub async fn serve_blob(app: &Magpie, hex: &str, variant: BlobVariant) -> Result<(Sha, Served)> {
    let row = blob_row(app, hex).await?;
    let conn = app.pool.get().await?;
    let referenced = conn
        .query_opt(
            "select 1 from blob_refs where blob = $1 limit 1",
            &[&row.sha.as_slice()],
        )
        .await?
        .is_some();
    if !referenced {
        return Err(Error::not_found());
    }
    let model = if variant == BlobVariant::Display {
        let m = conn
            .query_opt(
                "select model_blob from uploads where blob = $1 and model_blob is not null limit 1",
                &[&row.sha.as_slice()],
            )
            .await?;
        match m.and_then(|r| r.get::<_, Option<Vec<u8>>>(0)) {
            Some(m) => match Sha::try_from(m.as_slice()) {
                Ok(m) => blobs::row(&conn, &m).await?,
                Err(_) => None,
            },
            None => None,
        }
    } else {
        None
    };
    drop(conn);
    let served = app.blobs.variant(&app.pool, &row, variant, model.as_ref()).await?;
    Ok((row.sha, served))
}

/// `GET /v1/sandbox/blobs/{sha}`: a blob the sandbox's conversation references.
///
/// # Errors
/// 404, 415, 503; a statement error.
pub async fn serve_sandbox_blob(app: &Magpie, ctx: &Ctx, hex: &str, as_: SandboxBlobAs) -> Result<(Sha, Served)> {
    let db = app.pool.get().await?;
    let conv = sandbox::conversation(&db, ctx, false).await?;
    drop(db);
    let row = blob_row(app, hex).await?;
    let referenced = app
        .pool
        .get()
        .await?
        .query_opt(
            "select 1 from blob_refs where blob = $1 and owner = $2",
            &[&row.sha.as_slice(), &conv.id],
        )
        .await?
        .is_some();
    if !referenced {
        return Err(Error::not_found());
    }
    let served = app.blobs.for_sandbox(&app.pool, &row, as_).await?;
    Ok((row.sha, served))
}

/// A raw blob's facts from its bytes: `media` for images and PDF (with the image's dimensions), `file` otherwise.
#[must_use]
pub fn raw_facts(sha: Sha, bytes: &[u8], mime: Option<&str>) -> RawBlob {
    let sniffed = images::sniff(bytes);
    let mime = sniffed.or(mime).map(str::to_owned);
    let media = sniffed.is_some();
    let (width, height) = match sniffed {
        Some(m) if images::is_image(m) => {
            images::dimensions(bytes, m).map_or((None, None), |(w, h)| (Some(w), Some(h)))
        }
        _ => (None, None),
    };
    RawBlob {
        sha,
        size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        kind: if media { "media" } else { "file" },
        mime,
        width,
        height,
    }
}

/// `PUT /v1/sandbox/blobs/{sha}` (lease): the raw bytes of a sent file, verified against the sha; 201 when new,
/// 200 when magpie had them.
///
/// # Errors
/// 400 `sha_mismatch`, 404, 409 `lease_lost`, 413, 503; a statement error.
pub async fn put_sandbox_blob(app: &Magpie, ctx: &Ctx, hex: &str, body: Body) -> Result<Admitted<Empty>> {
    let sha = blobs::from_hex(hex).ok_or_else(|| Error::code(ErrorCode::ShaMismatch, "not a sha256"))?;
    {
        let conn = app.pool.get().await?;
        sandbox::conversation(&conn, ctx, true).await?;
        if blobs::row(&conn, &sha).await?.is_some_and(|r| r.stored) {
            return Ok(Admitted::Repeat(Empty {}));
        }
    }
    let bytes = read_body(body, magpie_api::sandbox::SENT_FILE_MAX_BYTES).await?;
    if blobs::sha256(&bytes) != sha {
        return Err(Error::code(
            ErrorCode::ShaMismatch,
            "the body's sha256 is not the path's",
        ));
    }
    let facts = raw_facts(sha, &bytes, None);
    let (_, lock) = app.blobs.put_bytes(&app.pool, bytes).await?;
    let r = aviary_pg::tx!(&app.pool, |t| -> Result<()> { blobs::record_stored(t, &facts).await }).await;
    lock.release().await;
    r?;
    Ok(Admitted::New(Empty {}))
}
