//! Upload ingestion (cutover 7.4.3, DESIGN 9.5): every file of a person's old asset folder
//! (`/jfs/users/<key>/claude/cloudcli-assets/`) becomes an unattached magpie upload of theirs (through magpie's own
//! upload path) and an `import_uploads` row naming it by its file name, so a composer that holds an old asset path
//! resolves it after the move. The hand-over loop ingests every person's folder on every pass (a file is read again
//! only when its size or mtime changed, and written only when its bytes differ from the row's), and once more right
//! before each release; magpie's synchronous ingest of a miss asks for one file ([`one`], `POST /v1/ingest-upload`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use magpie::conversations::Person;
use magpie_api::legacy::LEGACY_ASSET_DIR;
use magpie_api::uploads::{UPLOAD_MAX_BYTES, UploadKind, UploadMeta};
use serde::Serialize;

use crate::{Error, Importer, Result};

/// A person's old asset folder.
#[must_use]
pub fn folder(jfs: &Path, key: &str) -> PathBuf {
    // LEGACY_ASSET_DIR is `.claude/cloudcli-assets`: the home's `.claude` is `users/<key>/claude` on the volume
    crate::tree::claude_dir(jfs, key).join(LEGACY_ASSET_DIR.trim_start_matches(".claude/"))
}

/// What the loop already read of each file: (person, name) → (size, mtime ns).
pub type Seen = HashMap<(String, String), (u64, i64)>;

/// What one ingestion did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ingested {
    /// Files written as new uploads.
    pub stored: usize,
    /// Files read whose bytes the row already had.
    pub unchanged: usize,
    /// Files that could not be ingested (logged; the synchronous ingest retries them on a miss).
    pub failed: Vec<String>,
}

fn stat_of(m: &std::fs::Metadata) -> (u64, i64) {
    use std::os::unix::fs::MetadataExt as _;
    (m.len(), m.mtime() * 1_000_000_000 + m.mtime_nsec())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// Ingests one file of `person`'s folder by name: `Ok(false)` when there is no such regular file.
///
/// # Errors
/// A file over the upload cap; magpie's upload refused; a read or statement error.
pub async fn one(imp: &Importer, person: &str, key: &str, name: &str) -> Result<bool> {
    if !valid_name(name) {
        return Ok(false);
    }
    let path = folder(&imp.jfs, key).join(name);
    match tokio::fs::symlink_metadata(&path).await {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    }
    store(imp, person, name, &path).await?;
    Ok(true)
}

/// Reads `path` and makes it the upload `name` resolves to, unless the row already has its bytes. Whether it
/// stored a new upload.
async fn store(imp: &Importer, person: &str, name: &str, path: &Path) -> Result<bool> {
    let len = tokio::fs::metadata(path).await?.len();
    if len > UPLOAD_MAX_BYTES {
        return Err(Error::Source(format!("{name}: {len} bytes, over the upload cap")));
    }
    let bytes = tokio::fs::read(path).await?;
    let sha = magpie::blobs::sha256(&bytes);
    let pool = &imp.magpie.pool;
    let known = pool
        .get()
        .await?
        .query_opt(
            "select 1 from import_uploads where person = $1 and legacy_path = $2 and sha256 = $3",
            &[&person, &name, &sha.as_slice()],
        )
        .await?
        .is_some();
    if known {
        return Ok(false);
    }
    let who = Person {
        email: person.to_owned(),
        viewer: None,
    };
    let mime = magpie_api::mime::from_name(name);
    let bytes = axum::body::Bytes::from(bytes);
    let meta = |kind| UploadMeta {
        name: name.to_owned(),
        kind,
        content_type: mime.to_owned(),
    };
    let made = if mime.starts_with("image/") {
        match magpie::uploads::upload(&imp.magpie, &who, meta(UploadKind::Image), bytes.clone().into()).await {
            Err(e) if e.is(&magpie_api::errors::ErrorCode::UnsupportedType) => {
                magpie::uploads::upload(&imp.magpie, &who, meta(UploadKind::File), bytes.into()).await
            }
            other => other,
        }
    } else {
        magpie::uploads::upload(&imp.magpie, &who, meta(UploadKind::File), bytes.into()).await
    }
    .map_err(Error::Magpie)?;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        magpie::legacy::paths::import_upload(t, person, name, made.upload_id, sha.as_slice()).await?;
        Ok(())
    })
    .await?;
    Ok(true)
}

/// Ingests every file of `person`'s folder whose (size, mtime) is not in `seen` (every file when `seen` is empty),
/// recording what it read in `seen`. A missing folder is an empty one.
///
/// # Errors
/// The folder unreadable (a file that fails is listed in [`Ingested::failed`] instead).
pub async fn person(imp: &Importer, person: &str, key: &str, seen: &mut Seen) -> Result<Ingested> {
    let dir = folder(&imp.jfs, key);
    let mut out = Ingested::default();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    let mut files = Vec::new();
    for e in entries {
        let e = e?;
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        let m = e.metadata()?;
        if !m.is_file() || !valid_name(&name) || crate::tree::is_export_temp(&name) {
            continue;
        }
        let st = stat_of(&m);
        if seen.get(&(person.to_owned(), name.clone())) == Some(&st) {
            continue;
        }
        files.push((name, st));
    }
    for (name, st) in files {
        match store(imp, person, &name, &dir.join(&name)).await {
            Ok(true) => out.stored += 1,
            Ok(false) => out.unchanged += 1,
            Err(e) => {
                tracing::warn!(person_key = %key, file = %name, error = %e, "an old asset was not ingested");
                out.failed.push(name);
                continue;
            }
        }
        seen.insert((person.to_owned(), name), st);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_the_homes_asset_folder() {
        assert_eq!(
            folder(Path::new("/jfs"), "u1"),
            PathBuf::from("/jfs/users/u1/claude/cloudcli-assets")
        );
        assert!(valid_name("a.png") && !valid_name("..") && !valid_name("a/b") && !valid_name(""));
    }
}
