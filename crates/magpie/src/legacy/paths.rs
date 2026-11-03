//! The legacy-path set (DESIGN 9.5, magpie 8.2 step 4, switch-final 10.1 and 23 C15), until A7: a composer loaded
//! before the switch, or fed by the T0→gate upload tee, holds old asset paths, and any of them must work on whichever
//! path receives the send. In a native send (M3's first message, M7) each `files[].path` that is an old asset path of
//! the person is resolved, before the admission's transaction, through `uploads.legacy_path` (the tee), then
//! `import_uploads` (the importer's ingestion), then a synchronous ingest by the import Deployment, and replaced by
//! the path of a **fresh copy** of that upload (a new unattached upload with its own blob refs to the same blobs),
//! since the old stack reuses one asset path any number of times. Only when all three miss: 400 `unknown_upload`.

use aviary_pg::{Pool, Tx};
use magpie_api::errors::ErrorCode;
use magpie_api::legacy::legacy_asset_name;
use magpie_api::turns::SendRequest;
use uuid::Uuid;

use crate::app::Magpie;
use crate::conversations::Person;
use crate::db::Db;
use crate::error::{Error, Result};

/// `PUT /v1/uploads/{uploadId}/legacy-path`: records the old path pigeon's tee got back for the person's upload.
///
/// # Errors
/// 400 `bad_path` for a path that is not an old asset path of the person, 404 for an upload that is not theirs; a
/// statement error.
pub async fn record(pool: &Pool, person: &Person, upload: Uuid, legacy_path: &str) -> Result<()> {
    let key = aviary_core::log::person_key(&person.email);
    let name = legacy_asset_name(legacy_path, &key)
        .ok_or_else(|| Error::code(ErrorCode::BadPath, "not an old asset path of the person"))?;
    let n = pool
        .get()
        .await?
        .execute(
            "update uploads set legacy_path = $3 where id = $1 and person = $2",
            &[&upload, &person.email, &name],
        )
        .await?;
    if n == 0 {
        return Err(Error::not_found());
    }
    Ok(())
}

/// The importer's ingestion of one old asset, in its transaction: `upload` (an unattached upload of `person` holding
/// the bytes, made by the importer through magpie's upload path) is what `legacy_path` (the canonical name) resolves
/// to from now on.
///
/// # Errors
/// When a statement fails.
pub async fn import_upload(t: &Tx<'_>, person: &str, legacy_path: &str, upload: Uuid, sha256: &[u8]) -> Result<()> {
    t.execute(
        "insert into import_uploads (person, legacy_path, upload_id, sha256) values ($1, $2, $3, $4)
         on conflict (person, legacy_path) do update
           set upload_id = excluded.upload_id, sha256 = excluded.sha256, imported_at = now()
           where import_uploads.sha256 <> excluded.sha256",
        &[&person, &legacy_path, &upload, &sha256],
    )
    .await?;
    Ok(())
}

/// The upload an old asset name resolves to: the newest teed upload with that path, else the imported one.
///
/// # Errors
/// When a statement fails.
pub async fn lookup(db: &impl Db, person: &str, name: &str) -> Result<Option<Uuid>> {
    if let Some(r) = db
        .query_opt(
            "select id from uploads where person = $1 and legacy_path = $2 order by created_at desc limit 1",
            &[&person, &name],
        )
        .await?
    {
        return Ok(Some(r.get(0)));
    }
    Ok(db
        .query_opt(
            "select upload_id from import_uploads where person = $1 and legacy_path = $2",
            &[&person, &name],
        )
        .await?
        .map(|r| r.get(0)))
}

/// A fresh unattached copy of the person's upload `src` (same bytes and facts, its own blob refs); its id.
///
/// # Errors
/// 400 `unknown_upload` when `src` is gone; a statement error.
pub async fn copy(pool: &Pool, person: &str, src: Uuid) -> Result<Uuid> {
    let id = Uuid::now_v7();
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let n = t
            .execute(
                "insert into uploads (id, person, role, name, mime, size, blob, model_blob, width, height)
                 select $1, person, role, name, mime, size, blob, model_blob, width, height
                 from uploads where id = $2 and person = $3",
                &[&id, &src, &person],
            )
            .await?;
        if n == 0 {
            return Err(unknown());
        }
        t.execute(
            "update blobs set last_ref_at = now() where sha256 in
               (select blob from blob_refs where owner = $1 and owner_kind = 'upload')",
            &[&src],
        )
        .await?;
        t.execute(
            "insert into blob_refs (owner, owner_kind, blob)
             select $1, 'upload', blob from blob_refs where owner = $2 and owner_kind = 'upload'
             on conflict do nothing",
            &[&id, &src],
        )
        .await?;
        Ok(())
    })
    .await?;
    Ok(id)
}

fn unknown() -> Error {
    Error::code(
        ErrorCode::UnknownUpload,
        "an attachment names no unattached upload of the person",
    )
}

/// Resolves every old asset path of `m.files` (see the module), rewriting it to the fresh copy's
/// `uploads/<id>/<name>` path. Paths that are not old asset paths are left to the admission.
///
/// # Errors
/// 400 `unknown_upload` when an old path resolves nowhere, 503 when the synchronous ingest cannot be reached; a
/// statement error.
pub async fn resolve(app: &Magpie, person: &Person, m: &mut SendRequest) -> Result<()> {
    let key = aviary_core::log::person_key(&person.email);
    for f in &mut m.files {
        if f.path.starts_with("uploads/") {
            continue;
        }
        let Some(name) = legacy_asset_name(&f.path, &key).map(str::to_owned) else {
            continue;
        };
        app.legacy.legacy_path();
        let mut src = lookup(&app.pool.get().await?, &person.email, &name).await?;
        if src.is_none() && app.ingest.ingest(&person.email, &name).await? {
            src = lookup(&app.pool.get().await?, &person.email, &name).await?;
        }
        let Some(src) = src else {
            app.legacy.unknown_upload(true);
            tracing::warn!(person = %key, "an old asset path resolved nowhere");
            return Err(unknown());
        };
        let fresh = copy(&app.pool, &person.email, src).await?;
        let file = name.rsplit('/').next().unwrap_or(&name).to_owned();
        f.path = format!("uploads/{fresh}/{file}");
    }
    Ok(())
}

/// Whether `m` names an old asset path of `person` (a send without one skips [`resolve`] entirely).
#[must_use]
pub fn has_legacy_path(person: &Person, m: &SendRequest) -> bool {
    let key = aviary_core::log::person_key(&person.email);
    m.files
        .iter()
        .any(|f| !f.path.starts_with("uploads/") && legacy_asset_name(&f.path, &key).is_some())
}
