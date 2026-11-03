//! The legacy-path set (DESIGN 9.5, magpie 8.2 step 4, switch-final 10.1 and 23 C15), until A7: a composer loaded
//! before the switch, or fed by the T0→gate upload tee, holds old asset paths, and any of them must work on whichever
//! path receives the send. In a native send (M3's first message, M7) each `files[].path` that is an old asset path of
//! the person is resolved, before the admission's transaction, through `uploads.legacy_path` (the tee), then
//! `import_uploads` (the importer's ingestion), then a synchronous ingest by the import Deployment, and replaced by
//! the path of a **fresh copy** of that upload (a new unattached upload with its own blob refs to the same blobs),
//! since the old stack reuses one asset path any number of times. Only when all three miss: 400 `unknown_upload`.

use std::collections::HashMap;

use aviary_pg::{Pool, Tx};
use magpie_api::errors::ErrorCode;
use magpie_api::legacy::legacy_asset_name;
use magpie_api::turns::SendRequest;
use uuid::Uuid;

use crate::app::Magpie;
use crate::conversations::Person;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::limits;

/// `PUT /v1/uploads/{uploadId}/legacy-path`: records the old path pigeon's tee got back for the person's upload. The
/// record is a copy of the upload of its own (same bytes and facts, its own blob refs, never attached), so attaching
/// the upload itself by its magpie id (DESIGN 9.5's gate-time path) and later purging that conversation leave the
/// old path resolvable. A repeat for the same bytes and path adds nothing.
///
/// # Errors
/// 400 `bad_path` for a path that is not an old asset path of the person, 404 for an upload that is not theirs; a
/// statement error.
pub async fn record(pool: &Pool, person: &Person, upload: Uuid, legacy_path: &str) -> Result<()> {
    let key = aviary_core::log::person_key(&person.email);
    let name = legacy_asset_name(legacy_path, &key)
        .ok_or_else(|| Error::code(ErrorCode::BadPath, "not an old asset path of the person"))?;
    let known = pool
        .get()
        .await?
        .query_opt(
            "select exists (select 1 from uploads r where r.person = u.person and r.legacy_path = $3
                              and r.blob = u.blob and r.conversation_id is null)
             from uploads u where u.id = $1 and u.person = $2",
            &[&upload, &person.email, &name],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    if known.get::<_, bool>(0) {
        return Ok(());
    }
    copy_as(pool, &person.email, upload, Some(name)).await.map_err(|e| {
        if e.is(&ErrorCode::UnknownUpload) {
            Error::not_found()
        } else {
            e
        }
    })?;
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

/// The upload an old asset name resolves to: the newest tee record with that path, else the imported one.
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
    copy_as(pool, person, src, None).await
}

/// [`copy`], the copy carrying `legacy_path` (the tee's record) or none.
async fn copy_as(pool: &Pool, person: &str, src: Uuid, legacy_path: Option<&str>) -> Result<Uuid> {
    let id = Uuid::now_v7();
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let n = t
            .execute(
                "insert into uploads (id, person, role, name, mime, size, blob, model_blob, width, height, legacy_path)
                 select $1, person, role, name, mime, size, blob, model_blob, width, height, $4
                 from uploads where id = $2 and person = $3",
                &[&id, &src, &person, &legacy_path],
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
/// `uploads/<id>/<name>` path. Paths that are not old asset paths are left to the admission. The same old path twice
/// in one send becomes one copy, so it is one attachment, as the same upload id twice is (the admission de-duplicates
/// by upload id). The misses are ingested concurrently
/// under one deadline ([`limits::INGEST_TIMEOUT`], magpie 8.2's 5 s for the whole send); once every conversation is
/// owned (`import_control.all_owned_at`) nothing is left to ingest and a miss is `unknown_upload` at once.
///
/// # Errors
/// 400 `unknown_upload` when an old path resolves nowhere, 503 when the synchronous ingest cannot be reached or does
/// not answer in time; a statement error.
pub async fn resolve(app: &Magpie, person: &Person, m: &mut SendRequest) -> Result<()> {
    let key = aviary_core::log::person_key(&person.email);
    let mut names: Vec<String> = Vec::new();
    for f in &m.files {
        if f.path.starts_with("uploads/") {
            continue;
        }
        if let Some(name) = legacy_asset_name(&f.path, &key)
            && !names.iter().any(|n| n == name)
        {
            names.push(name.to_owned());
        }
    }
    if names.is_empty() {
        return Ok(());
    }
    let mut found: HashMap<String, Uuid> = HashMap::new();
    {
        let conn = app.pool.get().await?;
        for name in &names {
            app.legacy.legacy_path();
            if let Some(src) = lookup(&conn, &person.email, name).await? {
                found.insert(name.clone(), src);
            }
        }
    }
    let misses: Vec<&String> = names.iter().filter(|n| !found.contains_key(*n)).collect();
    if !misses.is_empty() && !all_owned(&app.pool.get().await?).await? {
        let calls = futures_util::future::join_all(misses.iter().map(|n| app.ingest.ingest(&person.email, n)));
        let answers = tokio::time::timeout(limits::INGEST_TIMEOUT, calls).await.map_err(|_| {
            tracing::warn!(person = %key, "the synchronous ingest did not answer in time");
            super::ingest::unavailable()
        })?;
        let conn = app.pool.get().await?;
        for (name, ingested) in misses.iter().zip(answers) {
            if ingested? && let Some(src) = lookup(&conn, &person.email, name).await? {
                found.insert((*name).clone(), src);
            }
        }
    }
    let mut fresh: HashMap<String, String> = HashMap::new();
    for name in &names {
        let Some(src) = found.get(name).copied() else {
            app.legacy.unknown_upload(true);
            tracing::warn!(person = %key, "an old asset path resolved nowhere");
            return Err(unknown());
        };
        let copy = copy(&app.pool, &person.email, src).await?;
        let file = name.rsplit('/').next().unwrap_or(name);
        fresh.insert(name.clone(), format!("uploads/{copy}/{file}"));
    }
    for f in &mut m.files {
        if f.path.starts_with("uploads/") {
            continue;
        }
        if let Some(path) = legacy_asset_name(&f.path, &key).and_then(|n| fresh.get(n)) {
            f.path.clone_from(path);
        }
    }
    Ok(())
}

/// Whether the importer has recorded every conversation owned (`import_control.all_owned_at`): from then on no old
/// asset is left to ingest.
async fn all_owned(db: &impl Db) -> Result<bool> {
    Ok(db
        .query_opt("select all_owned_at is not null from import_control", &[])
        .await?
        .is_some_and(|r| r.get::<_, bool>(0)))
}

/// Whether `m` names an old asset path of `person` (a send without one skips [`resolve`] entirely).
#[must_use]
pub fn has_legacy_path(person: &Person, m: &SendRequest) -> bool {
    let key = aviary_core::log::person_key(&person.email);
    m.files
        .iter()
        .any(|f| !f.path.starts_with("uploads/") && legacy_asset_name(&f.path, &key).is_some())
}
