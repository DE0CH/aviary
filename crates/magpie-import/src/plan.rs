//! `magpie import plan` (cutover 7.9): what an import would copy, read-only. Counts and sizes per schema, the
//! failed turns, the largest lines, transcripts not on disk, the auto-memory files (copied into each of the person's
//! conversations, decision 6: reported, never a blocker), and the blockers: a schema the importer cannot map, a
//! sessions row whose id is not a uuid, an id that is a native magpie conversation's or in two schemas, a line over
//! magpie's cap.

use std::io::{BufRead as _, BufReader, Read as _};

use serde::Serialize;

use crate::source::{self};
use crate::{Importer, Result, tree};

/// How many of the largest lines the plan lists.
const LARGEST: usize = 5;

/// One schema.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaPlan {
    /// `cc_u…`.
    pub schema: String,
    /// Sessions rows.
    pub conversations: usize,
    /// In Recently deleted.
    pub deleted: usize,
    /// Failed-turn rows.
    pub failed_turns: usize,
    /// Source files (transcripts, tool results, subagents, memory per conversation).
    pub files: usize,
    /// Their bytes.
    pub bytes: u64,
    /// Sessions rows with no transcript on disk (imported with their failed turns only).
    pub no_transcript: usize,
}

/// One memory file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFile {
    /// The person's key.
    pub person_key: String,
    /// The cc path.
    pub path: String,
    /// Bytes.
    pub size: u64,
    /// The conversations it is copied into.
    pub conversations: usize,
}

/// A long line.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongLine {
    /// The conversation.
    pub conversation: String,
    /// The cc path.
    pub path: String,
    /// Bytes.
    pub bytes: u64,
}

/// The plan.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// Per schema.
    pub schemas: Vec<SchemaPlan>,
    /// Totals.
    pub conversations: usize,
    /// Totals.
    pub bytes: u64,
    /// Already imported (an earlier `run`).
    pub imported: usize,
    /// Auto-memory files.
    pub memory: Vec<MemoryFile>,
    /// The largest lines.
    pub largest_lines: Vec<LongLine>,
    /// Shares.
    pub shares: SharesPlan,
    /// What stops an import (exit 1).
    pub blockers: Vec<String>,
}

/// The shares `plan` found (cutover 7.4.4).
#[derive(Debug, Default, Clone, Serialize)]
pub struct SharesPlan {
    /// Live rows.
    pub live: usize,
    /// Revoked rows.
    pub revoked: usize,
    /// Live rows whose conversation has no sessions row (imported as revoked).
    pub orphaned: usize,
    /// Live rows without a frozen transcript on the mount.
    pub no_transcript: usize,
}

fn longest_line(abs: &std::path::Path, size: u64) -> Result<u64> {
    let mut r = BufReader::new(std::fs::File::open(abs)?.take(size));
    let mut best = 0u64;
    let mut cur = 0u64;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let n = buf.len();
        for &b in buf {
            cur += 1;
            if b == b'\n' {
                best = best.max(cur);
                cur = 0;
            }
        }
        r.consume(n);
    }
    Ok(best.max(cur))
}

/// Builds the plan.
///
/// # Errors
/// A source read error.
#[allow(clippy::too_many_lines)] // the schemas, then the shares, in the order the plan reports them
pub async fn plan(imp: &Importer) -> Result<Plan> {
    let snap = source::read(&imp.cloudcli).await?;
    let known = crate::pass::known(imp).await?;
    let mut plan = Plan::default();
    for (schema, why) in &snap.problems {
        plan.blockers.push(format!("schema {schema}: {why}"));
    }
    for (id, schemas) in snap.duplicate_ids() {
        plan.blockers.push(format!(
            "conversation {id}: its sessions row is in more than one schema ({})",
            schemas.join(", ")
        ));
    }
    let conn = imp.magpie.pool.get().await?;
    let mut lines: Vec<LongLine> = Vec::new();
    for p in &snap.persons {
        let mut sp = SchemaPlan {
            schema: p.schema.clone(),
            ..SchemaPlan::default()
        };
        let memory = tree::memory(&imp.jfs, &p.key)?;
        for (path, m) in &memory {
            plan.memory.push(MemoryFile {
                person_key: p.key.clone(),
                path: path.clone(),
                size: m.stat.size,
                conversations: p.sessions.len(),
            });
        }
        for s in &p.sessions {
            sp.conversations += 1;
            if s.deleted().is_some() {
                sp.deleted += 1;
            }
            sp.failed_turns += p.failed.get(&s.session_id).map_or(0, Vec::len);
            let Ok(id) = s.id() else {
                plan.blockers
                    .push(format!("sessions row {} of {}: not a uuid", s.session_id, p.schema));
                continue;
            };
            if known.contains_key(&id) {
                plan.imported += 1;
            } else if conn
                .query_opt("select 1 from conversations where id = $1", &[&id])
                .await?
                .is_some()
            {
                plan.blockers
                    .push(format!("conversation {id}: the id is a native conversation's"));
            }
            let sources = tree::sources(&imp.jfs, &p.key, s.psid(), s.id().ok())?;
            if !sources.contains_key(&tree::main_path(s.psid())) {
                sp.no_transcript += 1;
            }
            for (path, src) in &sources {
                sp.files += 1;
                sp.bytes += src.stat.size;
                if magpie::ccfs::kind_of(path) == magpie::ccfs::Kind::Lines {
                    let n = longest_line(&src.abs, src.stat.size)?;
                    if i64::try_from(n).unwrap_or(i64::MAX) > magpie::limits::CC_LINE_MAX {
                        plan.blockers.push(format!(
                            "conversation {id}: a line of {path} is over magpie's cap ({n} bytes)"
                        ));
                    }
                    lines.push(LongLine {
                        conversation: id.to_string(),
                        path: path.clone(),
                        bytes: n,
                    });
                }
            }
        }
        plan.conversations += sp.conversations;
        plan.bytes += sp.bytes;
        plan.schemas.push(sp);
    }
    let rows: std::collections::BTreeMap<&str, (&str, &str)> = snap
        .persons
        .iter()
        .flat_map(|p| {
            p.sessions
                .iter()
                .map(|s| (s.session_id.as_str(), (p.key.as_str(), s.psid())))
        })
        .collect();
    for s in &snap.shares {
        if s.revoked_at.is_some() {
            plan.shares.revoked += 1;
            continue;
        }
        plan.shares.live += 1;
        let Some((key, psid)) = rows.get(s.session_id.as_str()) else {
            plan.shares.orphaned += 1;
            continue;
        };
        let frozen = crate::share::folder(&imp.jfs, s).join("transcript.jsonl");
        if !frozen.is_file() {
            plan.shares.no_transcript += 1;
            continue;
        }
        // the share's frozen transcript must be a byte prefix of the live one (cutover 7.4.4): the import does not
        // cut a share to the lines the two have in common
        let live = tree::claude_dir(&imp.jfs, key).join(tree::main_path(psid));
        let size = std::fs::metadata(&frozen)?.len();
        let (want, n) = tree::hash_prefix(&frozen, size)?;
        let prefix = live.is_file() && tree::hash_prefix(&live, size)? == (want, n);
        if !prefix {
            plan.blockers.push(format!(
                "share {}: its frozen transcript is not a prefix of its conversation's transcript",
                s.id
            ));
        }
    }
    lines.sort_by_key(|l| std::cmp::Reverse(l.bytes));
    lines.truncate(LARGEST);
    plan.largest_lines = lines;
    Ok(plan)
}
