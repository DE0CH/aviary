//! Verification (cutover 7.7, magpie 19.2). An open conversation: every cc file is reconstructed through magpie's
//! own read path (the code the FUSE layer reads through, `GET /v1/sandbox/cc/file`) and its sha256 compared with the
//! source file's; magpie's scope must hold no other file. A mismatch is logged, that file re-ingested from offset 0
//! and verified again; still different, the conversation stays legacy with `last_error` (status `stuck`). All
//! equal: `verified_at` and `verified_stats`. A released conversation: every source file still has the size and
//! sha256 recorded at the release (the old side wrote nothing after the hand-over); magpie's copy has grown since,
//! so it is not compared.

use std::collections::BTreeMap;

use http_body_util::BodyExt as _;
use magpie::ccfs::{Caller, reconstruct};
use magpie_api::sandbox::CcFileQuery;
use serde::Serialize;
use sha2::Digest as _;
use uuid::Uuid;

use crate::sync::{Files, Writer};
use crate::tree::{self, Source};
use crate::{Error, Importer, Result};

/// One file that did not verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mismatch {
    /// The cc path.
    pub path: String,
    /// The source's size (`None`: not in the source).
    pub source_size: Option<u64>,
    /// magpie's size (`None`: not in magpie).
    pub magpie_size: Option<u64>,
}

/// A conversation's verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    /// The conversation.
    pub conversation: Uuid,
    /// Released before (the sources checked against the record).
    pub released: bool,
    /// Files checked.
    pub files: usize,
    /// Files re-ingested after a first mismatch.
    pub reingested: Vec<String>,
    /// What is still different.
    pub mismatches: Vec<Mismatch>,
    /// Sanity notes, not a gate (cutover 7.7): event and prompt counts against the classifier's count of the
    /// source's lines, a workspace not on disk yet.
    pub notes: Vec<String>,
}

impl Verdict {
    /// Every file equal.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// sha256 hex and size of magpie's reconstruction of `path`, `None` when magpie has no such file.
///
/// # Errors
/// A read that failed (a blob missing, a sha mismatch inside magpie).
pub async fn reconstruct(imp: &Importer, id: Uuid, person: &str, path: &str) -> Result<Option<(String, u64)>> {
    let caller = Caller::importer(id, person, magpie::ccfs::Importer::default());
    let q = CcFileQuery {
        path: path.to_owned(),
        offset: None,
        length: None,
    };
    let res = match reconstruct::file(imp.magpie.clone(), &caller, &q).await {
        Ok(r) => r,
        Err(e) if e.is(&magpie_api::errors::ErrorCode::NotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut body = res.into_body();
    let mut h = sha2::Sha256::new();
    let mut n = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| Error::Source(format!("the read of {path} ended early: {e}")))?;
        if let Ok(data) = frame.into_data() {
            h.update(&data);
            n += data.len() as u64;
        }
    }
    Ok(Some((crate::hex(&h.finalize()), n)))
}

/// The files of magpie's scope of `id` (not folders), with sizes.
///
/// # Errors
/// A statement error.
pub async fn magpie_files(imp: &Importer, id: Uuid) -> Result<BTreeMap<String, u64>> {
    let rows = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select path, size from cc_files where scope = $1 and kind <> 'dir' order by path",
            &[&id],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get::<_, String>(0), u64::try_from(r.get::<_, i64>(1)).unwrap_or(0)))
        .collect())
}

struct Row {
    person: String,
    key: String,
    psid: String,
    files: Files,
    released: bool,
}

async fn row(imp: &Importer, id: Uuid) -> Result<Option<Row>> {
    let r = imp
        .magpie
        .pool
        .get()
        .await?
        .query_opt(
            "select person, user_key, provider_session, files, released_at is not null from import_sources
             where conversation_id = $1",
            &[&id],
        )
        .await?;
    let Some(r) = r else { return Ok(None) };
    Ok(Some(Row {
        person: r.get(0),
        key: r.get(1),
        psid: r.get::<_, Option<String>>(2).unwrap_or_default(),
        files: serde_json::from_value(r.get(3))?,
        released: r.get(4),
    }))
}

async fn compare(imp: &Importer, id: Uuid, person: &str, sources: &BTreeMap<String, Source>) -> Result<Vec<Mismatch>> {
    let mut out = Vec::new();
    let mine = magpie_files(imp, id).await?;
    for (path, src) in sources {
        let (sha, n) = tree::hash_prefix(&src.abs, src.stat.size)?;
        let got = reconstruct(imp, id, person, path).await?;
        if got.as_ref() != Some(&(sha, n)) {
            out.push(Mismatch {
                path: path.clone(),
                source_size: Some(n),
                magpie_size: got.map(|g| g.1),
            });
        }
    }
    for (path, size) in &mine {
        if !sources.contains_key(path) {
            out.push(Mismatch {
                path: path.clone(),
                source_size: None,
                magpie_size: Some(*size),
            });
        }
    }
    Ok(out)
}

/// Verifies one conversation (cutover 7.7) and records the outcome.
///
/// # Errors
/// [`Error::Source`] for a conversation that was not imported; a read error.
pub async fn verify(imp: &Importer, id: Uuid) -> Result<Verdict> {
    let Some(r) = row(imp, id).await? else {
        return Err(Error::Source(format!("{id} was not imported")));
    };
    if r.released {
        let mut mismatches = Vec::new();
        for (path, rec) in &r.files {
            let (sha, n) = tree::hash_prefix(std::path::Path::new(&rec.src), u64::MAX).unwrap_or_default();
            if sha != rec.sha256 || n != rec.size {
                mismatches.push(Mismatch {
                    path: path.clone(),
                    source_size: Some(n),
                    magpie_size: None,
                });
            }
        }
        return Ok(Verdict {
            conversation: id,
            released: true,
            files: r.files.len(),
            reingested: Vec::new(),
            mismatches,
            notes: Vec::new(),
        });
    }
    let sources = tree::sources(&imp.jfs, &r.key, &r.psid)?;
    let first = compare(imp, id, &r.person, &sources).await?;
    let mut reingested = Vec::new();
    let mismatches = if first.is_empty() {
        first
    } else {
        let workspace = tree::workspace_uploads(&imp.jfs, id)?;
        let mut w = Writer::new(imp, id, &r.person, workspace);
        for m in &first {
            tracing::warn!(
                conversation = %id, path = %m.path, source = ?m.source_size, magpie = ?m.magpie_size,
                "reconstruct mismatch"
            );
            match sources.get(&m.path) {
                Some(src) => {
                    w.replace(&m.path, src).await?;
                }
                None => w.unlink(&m.path).await?,
            }
            reingested.push(m.path.clone());
        }
        compare(imp, id, &r.person, &sources).await?
    };
    let pool = &imp.magpie.pool;
    if mismatches.is_empty() {
        let stats: BTreeMap<&String, &tree::Stat> = sources.iter().map(|(p, s)| (p, &s.stat)).collect();
        let stats = serde_json::to_value(stats)?;
        pool.get()
            .await?
            .execute(
                "update import_sources set verified_at = now(), verified_stats = $2, last_error = null
                 where conversation_id = $1",
                &[&id, &stats],
            )
            .await?;
    } else {
        let paths: Vec<&str> = mismatches.iter().map(|m| m.path.as_str()).collect();
        let msg = format!("could not be verified: {}", paths.join(", "));
        pool.get()
            .await?
            .execute(
                "update import_sources set verified_at = null, verified_stats = null, last_error = $2
                 where conversation_id = $1",
                &[&id, &msg],
            )
            .await?;
    }
    let notes = sanity(imp, id, &sources, &tree::main_path(&r.psid)).await?;
    for n in &notes {
        tracing::warn!(conversation = %id, note = %n, "import sanity");
    }
    Ok(Verdict {
        conversation: id,
        released: false,
        files: sources.len(),
        reingested,
        mismatches,
        notes,
    })
}

/// The classifier's count of the source transcript's complete lines (events, prompts).
fn classified(abs: &std::path::Path, size: u64) -> Result<(i64, i64)> {
    use magpie::ccfs::classify::{self, Class, Context};
    use std::io::{BufRead as _, Read as _};
    let mut r = std::io::BufReader::new(std::fs::File::open(abs)?.take(size));
    let (mut events, mut prompts) = (0, 0);
    let mut line = Vec::new();
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line)? == 0 || !line.ends_with(b"\n") {
            break;
        }
        let v = serde_json::from_slice::<serde_json::Value>(&line).ok();
        if let Class::Event(e) = classify::classify(v.as_ref(), Context::default()) {
            events += 1;
            prompts += i64::from(e.is_prompt);
        }
    }
    Ok((events, prompts))
}

/// Cutover 7.7's sanity checks (logged, never a gate).
async fn sanity(imp: &Importer, id: Uuid, sources: &BTreeMap<String, Source>, main: &str) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    if let Some(m) = sources.get(main) {
        let (want_events, want_prompts) = classified(&m.abs, m.stat.size)?;
        let r = imp
            .magpie
            .pool
            .get()
            .await?
            .query_one(
                "select count(*), count(*) filter (where e.is_prompt) from events e
                 where e.conversation_id = $1 and e.kind in ('cc.user', 'cc.assistant')
                   and not exists (select 1 from events x where x.conversation_id = $1 and x.kind = 'retract'
                                   and x.data->'seqs' @> to_jsonb(e.seq))",
                &[&id],
            )
            .await?;
        let (events, prompts): (i64, i64) = (r.get(0), r.get(1));
        if (events, prompts) != (want_events, want_prompts) {
            notes.push(format!(
                "{events} events and {prompts} prompts; the source's lines classify as {want_events} and {want_prompts}"
            ));
        }
    }
    if !imp.jfs.join("ws").join(id.to_string()).is_dir() {
        notes.push("no workspace on disk (hermit creates it at the first start)".to_owned());
    }
    Ok(notes)
}

/// Every imported conversation, or `ids`.
///
/// # Errors
/// A statement error.
pub async fn targets(imp: &Importer, ids: &[Uuid]) -> Result<Vec<Uuid>> {
    if !ids.is_empty() {
        return Ok(ids.to_vec());
    }
    let rows = imp
        .magpie
        .pool
        .get()
        .await?
        .query(
            "select conversation_id from import_sources order by conversation_id",
            &[],
        )
        .await?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}
