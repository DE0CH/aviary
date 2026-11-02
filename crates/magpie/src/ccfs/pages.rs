//! Byte files (magpie 6.8): every file that is not a lines file, in 256 KiB pages (`cc_pages`). A write reads,
//! changes and writes back the pages it touches in its transaction; a write past the end zero-fills; a truncate drops
//! the pages past the new size and trims the last one. A page that does not exist inside the size reads as zeros, and
//! a short last page is zero-padded up to the size.

use std::collections::BTreeMap;

use aviary_pg::Tx;

use super::{File, Op, n64, nus};
use crate::db::Db;
use crate::error::Result;
use crate::limits;

const PAGE: i64 = limits::CC_PAGE;

fn page_no(off: i64) -> i32 {
    i32::try_from(off / PAGE).unwrap_or(i32::MAX)
}

async fn load(db: &impl Db, file: i64, first: i32, last: i32) -> Result<BTreeMap<i32, Vec<u8>>> {
    let rows = db
        .query(
            "select page_no, bytes from cc_pages where file_id = $1 and page_no between $2 and $3 order by page_no",
            &[&file, &first, &last],
        )
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

/// Bytes `[from, to)` of a byte file of `size` bytes.
///
/// # Errors
/// When a statement fails.
pub async fn read(db: &impl Db, file: i64, size: i64, from: i64, to: i64) -> Result<Vec<u8>> {
    let to = to.min(size);
    if from >= to {
        return Ok(Vec::new());
    }
    let pages = load(db, file, page_no(from), page_no(to - 1)).await?;
    let mut out = Vec::with_capacity(nus(to - from));
    for no in page_no(from)..=page_no(to - 1) {
        let start = i64::from(no) * PAGE;
        let lo = from.max(start) - start;
        let hi = to.min(start + PAGE) - start;
        let page = pages.get(&no).map_or(&[][..], Vec::as_slice);
        for i in lo..hi {
            out.push(page.get(nus(i)).copied().unwrap_or(0));
        }
    }
    Ok(out)
}

/// A write of `data` at `offset` (past the end zero-fills).
///
/// # Errors
/// When a statement fails.
pub async fn write(tx: &Tx<'_>, op: &mut Op<'_>, f: &mut File, offset: i64, data: &[u8]) -> Result<()> {
    let end = offset + n64(data.len());
    if !data.is_empty() {
        let (first, last) = (page_no(offset), page_no(end - 1));
        let mut pages = load(tx, f.id, first, last).await?;
        for no in first..=last {
            let start = i64::from(no) * PAGE;
            let page = pages.entry(no).or_default();
            let old = n64(page.len());
            let lo = offset.max(start) - start;
            let hi = end.min(start + PAGE) - start;
            if n64(page.len()) < hi {
                page.resize(nus(hi), 0);
            }
            let src = nus(start + lo - offset);
            page[nus(lo)..nus(hi)].copy_from_slice(&data[src..src + nus(hi - lo)]);
            tx.execute(
                "insert into cc_pages (file_id, page_no, bytes) values ($1, $2, $3)
                 on conflict (file_id, page_no) do update set bytes = excluded.bytes",
                &[&f.id, &no, &page.as_slice()],
            )
            .await?;
            op.cc_delta += n64(page.len()) - old;
        }
    }
    f.size = f.size.max(end);
    Ok(())
}

/// A truncate to `size` (extending zero-fills: the missing bytes read as zeros).
///
/// # Errors
/// When a statement fails.
pub async fn truncate(tx: &Tx<'_>, op: &mut Op<'_>, f: &mut File, size: i64) -> Result<()> {
    if size < f.size {
        let keep = if size == 0 { -1 } else { page_no(size - 1) };
        let dropped = tx
            .query_one(
                "with d as (delete from cc_pages where file_id = $1 and page_no > $2 returning bytes)
                 select coalesce(sum(octet_length(bytes)), 0)::bigint from d",
                &[&f.id, &keep],
            )
            .await?;
        op.cc_delta -= dropped.get::<_, i64>(0);
        if keep >= 0 {
            let in_last = i32::try_from(size - i64::from(keep) * PAGE).unwrap_or(i32::MAX);
            let len = tx
                .query_opt(
                    "select octet_length(bytes) from cc_pages where file_id = $1 and page_no = $2",
                    &[&f.id, &keep],
                )
                .await?
                .map(|r| r.get::<_, i32>(0));
            if let Some(len) = len.filter(|l| *l > in_last) {
                tx.execute(
                    "update cc_pages set bytes = substring(bytes from 1 for $3) where file_id = $1 and page_no = $2",
                    &[&f.id, &keep, &in_last],
                )
                .await?;
                op.cc_delta -= i64::from(len - in_last);
            }
        }
    }
    f.size = size;
    Ok(())
}

/// The whole file's bytes.
///
/// # Errors
/// When a statement fails.
pub async fn read_all(db: &impl Db, f: &File) -> Result<Vec<u8>> {
    read(db, f.id, f.size, 0, f.size).await
}
