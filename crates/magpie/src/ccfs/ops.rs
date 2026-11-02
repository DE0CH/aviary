//! The state-changing cc operations (magpie 16.2), each one transaction through [`crate::cc_tx!`] and [`super::begin`]:
//! write, truncate, rename, unlink, mkdir, rmdir, setattr.
//!
//! Renames follow `rename(2)`: onto an existing empty directory (from a directory) or a file (from a file) they
//! replace the target; onto a non-empty directory 409 `not_empty`; a file onto a directory or the other way 409
//! `exists`; a directory into its own subtree 400 `bad_path`. A file whose name changes kind (`lines` ⇄ `bytes`) is
//! stored again in the other form.

use super::{
    Begun, Caller, File, Kind, Op, begin, check_path, create_file, has_children, kind_of, lines, load_file, load_under,
    n64, pages, require_parent, save_file, stored_bytes,
};
use crate::app::Magpie;
use crate::error::{Error, Result};
use crate::limits;
use aviary_pg::Tx;
use magpie_api::errors::ErrorCode;
use magpie_api::sandbox::{CcMkdir, CcPathOp, CcRename, CcSetattr, CcSizeVersion, CcTruncate, CcVersion, CcWriteQuery};

fn u64_of(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

fn i64_of(n: u64) -> Result<i64> {
    i64::try_from(n).map_err(|_| Error::bad_request("a size or offset out of range"))
}

fn mode_of(m: u32) -> Result<i32> {
    i32::try_from(m).map_err(|_| Error::bad_request("mode out of range"))
}

fn not_found(what: &str) -> Error {
    Error::code(ErrorCode::NotFound, format!("{what} does not exist"))
}

/// `POST /v1/sandbox/cc/write`.
///
/// # Errors
/// 400 `bad_path` / `bad_request`, 404, 409 `offset_beyond_eof` / `offset_mismatch` (an append whose offset is not
/// the size) / `lease_lost` / `conversation_migrating`, 413, 503, 507; a statement error.
pub async fn write(app: &Magpie, caller: &Caller, q: &CcWriteQuery, data: &[u8]) -> Result<CcSizeVersion> {
    check_path(&q.path)?;
    if data.len() > limits::CC_WRITE_MAX {
        return Err(Error::code(ErrorCode::TooLarge, "a cc write is over 16 MiB"));
    }
    let offset = i64_of(q.offset)?;
    let mode = mode_of(q.mode)?;
    let end = offset
        .checked_add(n64(data.len()))
        .ok_or_else(|| Error::bad_request("offset"))?;
    crate::cc_tx!(app, |pre, t| -> CcSizeVersion {
        let mut op = match begin(t, caller, q.op_id, pre).await? {
            Begun::Replay(v) => return Ok(serde_json::from_value(v)?),
            Begun::Go(op) => op,
        };
        let scope = op.conv.id;
        let file = load_file(t, scope, &q.path).await?;
        // rule 0 (6.6), before every other rule: a range inside the file that already holds exactly these bytes
        if let Some(f) = file.as_ref().filter(|f| f.kind != Kind::Dir && end <= f.size) {
            let same = data.is_empty()
                || match f.kind {
                    Kind::Lines => lines::read_range(t, &mut op, f, offset, end).await? == data,
                    _ => pages::read(t, f.id, f.size, offset, end).await? == data,
                };
            if same {
                return Ok(CcSizeVersion {
                    size: u64_of(f.size),
                    version: u64_of(f.version),
                });
            }
        }
        let mut f = match file {
            Some(f) if f.kind == Kind::Dir => return Err(Error::bad_request("the path is a directory")),
            Some(f) => f,
            None if q.create => {
                require_parent(t, scope, &q.path).await?;
                create_file(t, scope, &q.path, kind_of(&q.path), mode, q.mtime_ns).await?
            }
            None => return Err(not_found("the file")),
        };
        if offset > f.size && f.kind == Kind::Lines {
            return Err(Error::code(
                ErrorCode::OffsetBeyondEof,
                "the offset is past the end of the file",
            ));
        }
        // an append expects the file's size (6.10): a stale retry with other bytes cannot write twice
        if q.append && offset != f.size {
            return Err(Error::code(
                ErrorCode::OffsetMismatch,
                "the append's offset is not the file's size",
            ));
        }
        if f.kind == Kind::Bytes && end.max(f.size) > limits::CC_FILE_MAX {
            return Err(Error::code(ErrorCode::TooLarge, "the file is over 256 MiB"));
        }
        op.credit(t, q.turn_id).await?;
        if f.kind == Kind::Lines {
            lines::write(t, &mut op, &mut f, offset, data).await?;
        } else {
            pages::write(t, &mut op, &mut f, offset, data).await?;
        }
        f.version += 1;
        f.mtime_ns = q.mtime_ns;
        save_file(t, &f).await?;
        let r = CcSizeVersion {
            size: u64_of(f.size),
            version: u64_of(f.version),
        };
        op.finish(t, q.op_id, &serde_json::to_value(r)?).await?;
        Ok(r)
    })
}

/// `POST /v1/sandbox/cc/truncate`.
///
/// # Errors
/// 400, 404, 409 `offset_beyond_eof` (a lines file cannot grow by a truncate) / `lease_lost`, 413, 507.
pub async fn truncate(app: &Magpie, caller: &Caller, b: &CcTruncate) -> Result<CcSizeVersion> {
    check_path(&b.path)?;
    let size = i64_of(b.size)?;
    crate::cc_tx!(app, |pre, t| -> CcSizeVersion {
        let mut op = match begin(t, caller, b.op_id, pre).await? {
            Begun::Replay(v) => return Ok(serde_json::from_value(v)?),
            Begun::Go(op) => op,
        };
        let mut f = load_file(t, op.conv.id, &b.path)
            .await?
            .filter(|f| f.kind != Kind::Dir)
            .ok_or_else(|| not_found("the file"))?;
        if size > f.size && f.kind == Kind::Lines {
            return Err(Error::code(
                ErrorCode::OffsetBeyondEof,
                "the size is past the end of the file",
            ));
        }
        if f.kind == Kind::Bytes && size > limits::CC_FILE_MAX {
            return Err(Error::code(ErrorCode::TooLarge, "the file is over 256 MiB"));
        }
        // the limbo a truncate makes belongs to the turn running it (6.7)
        op.credit(t, None).await?;
        if f.kind == Kind::Lines {
            lines::truncate(t, &mut op, &mut f, size).await?;
        } else {
            pages::truncate(t, &mut op, &mut f, size).await?;
        }
        f.version += 1;
        f.mtime_ns = b.mtime_ns;
        save_file(t, &f).await?;
        let r = CcSizeVersion {
            size: u64_of(f.size),
            version: u64_of(f.version),
        };
        op.finish(t, b.op_id, &serde_json::to_value(r)?).await?;
        Ok(r)
    })
}

/// Stores a file's content again in the form its new name asks for.
async fn convert(t: &Tx<'_>, op: &mut Op<'_>, f: &mut File, to: Kind) -> Result<()> {
    if to == Kind::Bytes && f.size > limits::CC_FILE_MAX {
        return Err(Error::code(ErrorCode::TooLarge, "the file is over 256 MiB"));
    }
    let before = stored_bytes(t, f).await?;
    let content = if f.kind == Kind::Lines {
        lines::read_all(t, op, f).await?
    } else {
        pages::read_all(t, f).await?
    };
    t.execute("delete from cc_lines where file_id = $1", &[&f.id]).await?;
    t.execute("delete from cc_pages where file_id = $1", &[&f.id]).await?;
    op.cc_delta -= before;
    f.tail.clear();
    f.size = 0;
    f.kind = to;
    if to == Kind::Lines {
        lines::fill(t, op, f, &content).await
    } else {
        pages::write(t, op, f, 0, &content).await
    }
}

/// Removes a file's row (its lines or pages go with it).
async fn remove(t: &Tx<'_>, op: &mut Op<'_>, f: &File) -> Result<()> {
    op.cc_delta -= stored_bytes(t, f).await?;
    t.execute("delete from cc_files where id = $1", &[&f.id]).await?;
    Ok(())
}

/// The main-transcript rules of a file whose path changed from `from` to `f.path` (6.2).
async fn main_rules(t: &Tx<'_>, op: &mut Op<'_>, f: &File, from: &str) -> Result<()> {
    if f.kind != Kind::Lines {
        return Ok(());
    }
    let (was, is) = (op.conv.is_main(from), op.conv.is_main(&f.path));
    if was && !is {
        lines::moved_away(t, op, f.id).await?;
    }
    if is && !was {
        lines::promote(t, op, f.id).await?;
    }
    Ok(())
}

/// `POST /v1/sandbox/cc/rename` (replaces `to`).
///
/// # Errors
/// 400 `bad_path`, 404, 409 `exists` / `not_empty` / `lease_lost`, 507.
pub async fn rename(app: &Magpie, caller: &Caller, b: &CcRename) -> Result<CcVersion> {
    check_path(&b.from)?;
    check_path(&b.to)?;
    crate::cc_op!(app, caller, b.op_id, |t, op| -> CcVersion {
        let op = &mut *op;
        let scope = op.conv.id;
        let mut source = load_file(t, scope, &b.from)
            .await?
            .ok_or_else(|| not_found("the source"))?;
        require_parent(t, scope, &b.to).await?;
        if b.to == b.from {
            return Ok(CcVersion {
                version: u64_of(source.version),
            });
        }
        let from_dir = source.kind == Kind::Dir;
        if from_dir && b.to.starts_with(&format!("{}/", b.from)) {
            return Err(Error::code(
                ErrorCode::BadPath,
                "a directory cannot move into its own subtree",
            ));
        }
        let target = load_file(t, scope, &b.to).await?;
        if let Some(target) = &target {
            let target_dir = target.kind == Kind::Dir;
            if target_dir && has_children(t, scope, &b.to).await? {
                return Err(Error::code(ErrorCode::NotEmpty, "the target directory is not empty"));
            }
            if target_dir != from_dir {
                return Err(Error::code(
                    ErrorCode::Exists,
                    "a file and a directory cannot replace each other",
                ));
            }
        }
        op.credit(t, None).await?;
        let mut version = source.version.max(target.as_ref().map_or(0, |f| f.version)) + 1;
        if from_dir {
            if let Some(target) = &target {
                remove(t, op, target).await?;
            }
            for mut f in load_under(t, scope, &b.from).await? {
                let old = f.path.clone();
                f.path = format!("{}{}", b.to, &old[b.from.len()..]);
                if old == b.from {
                    f.version = version;
                } else {
                    f.version += 1;
                }
                // a rename of the main transcript's directory moves the main transcript itself: one statement per
                // path keeps the unique (scope, path) index consistent row by row (no path is reused inside one)
                t.execute(
                    "update cc_files set path = $2, version = $3 where id = $1",
                    &[&f.id, &f.path, &f.version],
                )
                .await?;
                main_rules(t, op, &f, &old).await?;
            }
        } else {
            if let Some(target) = &target {
                if target.kind == Kind::Lines && source.kind == Kind::Lines {
                    lines::rename_over(t, op, &source, target).await?;
                }
                remove(t, op, target).await?;
            }
            let new_kind = kind_of(&b.to);
            let old = source.path.clone();
            source.path.clone_from(&b.to);
            if new_kind != source.kind {
                convert(t, op, &mut source, new_kind).await?;
                // a byte file stored again as lines at the main path was classified as it was written
                version = version.max(source.version + 1);
            }
            source.version = version;
            save_file(t, &source).await?;
            main_rules(t, op, &source, &old).await?;
        }
        CcVersion {
            version: u64_of(version),
        }
    })
}

/// `POST /v1/sandbox/cc/unlink`: 403 `protected` for the main transcript (EPERM).
///
/// # Errors
/// 400, 403, 404, 409 `lease_lost`.
pub async fn unlink(app: &Magpie, caller: &Caller, b: &CcPathOp) -> Result<()> {
    check_path(&b.path)?;
    crate::cc_op!(app, caller, b.op_id, |t, op| -> serde_json::Value {
        let op = &mut *op;
        if op.conv.is_main(&b.path) {
            return Err(Error::code(
                ErrorCode::Protected,
                "the main transcript cannot be removed",
            ));
        }
        let f = load_file(t, op.conv.id, &b.path)
            .await?
            .filter(|f| f.kind != Kind::Dir)
            .ok_or_else(|| not_found("the file"))?;
        remove(t, op, &f).await?;
        serde_json::json!({})
    })
    .map(drop)
}

/// `POST /v1/sandbox/cc/mkdir`.
///
/// # Errors
/// 400, 404 (no parent), 409 `exists` / `lease_lost`.
pub async fn mkdir(app: &Magpie, caller: &Caller, b: &CcMkdir) -> Result<()> {
    check_path(&b.path)?;
    let mode = mode_of(b.mode)?;
    crate::cc_op!(app, caller, b.op_id, |t, op| -> serde_json::Value {
        let op = &mut *op;
        let scope = op.conv.id;
        if load_file(t, scope, &b.path).await?.is_some() {
            return Err(Error::code(ErrorCode::Exists, "the path exists"));
        }
        require_parent(t, scope, &b.path).await?;
        let mut f = create_file(t, scope, &b.path, Kind::Dir, mode, b.mtime_ns).await?;
        f.version = 1;
        save_file(t, &f).await?;
        serde_json::json!({})
    })
    .map(drop)
}

/// `POST /v1/sandbox/cc/rmdir`.
///
/// # Errors
/// 400, 404, 409 `not_empty` / `lease_lost`.
pub async fn rmdir(app: &Magpie, caller: &Caller, b: &CcPathOp) -> Result<()> {
    check_path(&b.path)?;
    crate::cc_op!(app, caller, b.op_id, |t, op| -> serde_json::Value {
        let op = &mut *op;
        let scope = op.conv.id;
        let f = load_file(t, scope, &b.path)
            .await?
            .filter(|f| f.kind == Kind::Dir)
            .ok_or_else(|| not_found("the directory"))?;
        if has_children(t, scope, &b.path).await? {
            return Err(Error::code(ErrorCode::NotEmpty, "the directory is not empty"));
        }
        remove(t, op, &f).await?;
        serde_json::json!({})
    })
    .map(drop)
}

/// `POST /v1/sandbox/cc/setattr`.
///
/// # Errors
/// 400, 404, 409 `lease_lost`.
pub async fn setattr(app: &Magpie, caller: &Caller, b: &CcSetattr) -> Result<CcVersion> {
    check_path(&b.path)?;
    let mode = b.mode.map(mode_of).transpose()?;
    crate::cc_op!(app, caller, b.op_id, |t, op| -> CcVersion {
        let op = &mut *op;
        let mut f = load_file(t, op.conv.id, &b.path)
            .await?
            .ok_or_else(|| not_found("the path"))?;
        if let Some(m) = mode {
            f.mode = m;
        }
        if let Some(m) = b.mtime_ns {
            f.mtime_ns = m;
        }
        f.version += 1;
        save_file(t, &f).await?;
        CcVersion {
            version: u64_of(f.version),
        }
    })
}
