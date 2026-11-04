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

use crate::sync::{Files, Recorded, Writer};
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
    /// A conversation released by force (`release_reason = forced`): what the old side wrote after the release (an
    /// old turn the operator cut off kept writing). Expected and reported apart, so `verify --all` does not fail on
    /// it and the hand-over is not held; those writes are lost, as the forced release accepted.
    pub forced_writes: Vec<Mismatch>,
    /// What the sources have that the last pass did not sync yet (a sessions row change, a failed turn, a send):
    /// verification waits for the next pass.
    pub pending: Vec<String>,
    /// Sanity notes, not a gate (cutover 7.7): event and prompt counts against the classifier's count of the
    /// source's lines, a workspace not on disk yet.
    pub notes: Vec<String>,
}

impl Verdict {
    /// Every file equal.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty() && self.pending.is_empty()
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

/// What the importer has of a conversation (`import_sources`).
#[derive(Debug, Clone)]
pub struct Row {
    /// The person.
    pub person: String,
    /// Their key.
    pub key: String,
    /// Their schema (`cc_u…`).
    pub schema: String,
    /// Claude Code's session id.
    pub psid: String,
    /// What the last sync recorded of each file.
    pub files: Files,
    /// The fingerprints the last sync recorded.
    pub synced: Fingerprints,
    /// Released.
    pub released: bool,
    /// What verification recorded (`verified_stats`), when it passed.
    pub verified: Option<Fingerprints>,
    /// Released by `release --force`.
    pub forced: bool,
    /// When the last sync recorded it (`last_synced_at`).
    pub synced_at: time::OffsetDateTime,
}

/// A conversation's sources as fingerprints (cutover 7.3 `verified_stats`: every source file's stat and every
/// fingerprint): what a release re-stats against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprints {
    /// Every source file's (size, mtime, inode), by cc path.
    pub files: BTreeMap<String, (u64, i64, u64)>,
    /// The sessions row's fingerprint (empty: the row is gone).
    pub row_fp: String,
    /// The failed-turn ids.
    pub failed_turns: std::collections::BTreeSet<i32>,
    /// The sent folders' fingerprint.
    pub sent_fp: String,
}

impl Fingerprints {
    /// What `now` has that differs from `self` (empty: nothing changed). A failed turn removed on the old side is
    /// no change (an imported failure stays); one added is.
    #[must_use]
    pub fn changes(&self, now: &Fingerprints) -> Vec<String> {
        let mut out = Vec::new();
        for (p, s) in &now.files {
            match self.files.get(p) {
                None => out.push(format!("{p} is new")),
                Some(was) if was != s => out.push(format!("{p} changed")),
                Some(_) => {}
            }
        }
        for p in self.files.keys().filter(|p| !now.files.contains_key(*p)) {
            out.push(format!("{p} is gone"));
        }
        if now.row_fp.is_empty() {
            out.push("the sessions row is gone".to_owned());
        } else if now.row_fp != self.row_fp {
            out.push("the sessions row changed".to_owned());
        }
        if !now.failed_turns.is_subset(&self.failed_turns) {
            out.push("a failed turn was added".to_owned());
        }
        if now.sent_fp != self.sent_fp {
            out.push("the sent files changed".to_owned());
        }
        out
    }

    /// Without the files.
    #[must_use]
    pub fn rows_only(mut self) -> Self {
        self.files.clear();
        self
    }
}

fn stat_of(s: &tree::Stat) -> (u64, i64, u64) {
    (s.size, s.mtime_ns, s.ino)
}

/// One conversation's `import_sources` row.
///
/// # Errors
/// A statement error, a record that does not parse.
pub async fn row(imp: &Importer, id: Uuid) -> Result<Option<Row>> {
    let r = imp
        .magpie
        .pool
        .get()
        .await?
        .query_opt(
            "select person, user_key, provider_session, files, released_at is not null, source_schema, row_fp,
                    failed_turns_fp, sent_fp, verified_stats, release_reason is not distinct from 'forced',
                    last_synced_at
             from import_sources where conversation_id = $1",
            &[&id],
        )
        .await?;
    let Some(r) = r else { return Ok(None) };
    let files: Files = serde_json::from_value(r.get(3))?;
    let synced = Fingerprints {
        files: files
            .iter()
            .map(|(p, f)| (p.clone(), (f.size, f.mtime_ns, f.ino)))
            .collect(),
        row_fp: r.get(6),
        failed_turns: crate::failed::ids_of(&r.get::<_, String>(7)),
        sent_fp: r.get(8),
    };
    let verified = r
        .get::<_, Option<serde_json::Value>>(9)
        .map(serde_json::from_value::<Fingerprints>)
        .transpose()
        .ok()
        .flatten();
    Ok(Some(Row {
        person: r.get(0),
        key: r.get(1),
        psid: r.get::<_, Option<String>>(2).unwrap_or_default(),
        files,
        released: r.get(4),
        schema: r.get(5),
        synced,
        verified,
        forced: r.get(10),
        synced_at: r.get(11),
    }))
}

/// The conversation's sources now: a stat of every source file, and the fingerprints of its sessions row, failed
/// turns and sent folders from one read of its person's schema.
///
/// # Errors
/// The read; a schema the importer cannot map.
pub async fn current(imp: &Importer, id: Uuid, r: &Row) -> Result<(BTreeMap<String, Source>, Fingerprints)> {
    let snap = crate::source::read_schema(&imp.cloudcli, &r.schema).await?;
    if let Some((_, why)) = snap.problems.first() {
        return Err(Error::Source(format!("schema {}: {why}", r.schema)));
    }
    let found = crate::pass::find(&snap, id);
    let row_fp = found.map(|(_, s)| s.fingerprint()).unwrap_or_default();
    let failed_turns = found
        .and_then(|(p, s)| p.failed.get(&s.session_id))
        .map(|f| f.iter().map(|f| f.id).collect())
        .unwrap_or_default();
    let (_, sent_fp) = crate::sent::read(&imp.jfs, id)?;
    let sources = tree::sources(&imp.jfs, &r.key, &r.psid, Some(id))?;
    let fp = Fingerprints {
        files: sources.iter().map(|(p, s)| (p.clone(), stat_of(&s.stat))).collect(),
        row_fp,
        failed_turns,
        sent_fp,
    };
    Ok((sources, fp))
}

/// What changed in the conversation's sources since its last sync recorded them (the release's re-stat inside the
/// fence, cutover 7.8, after the final sync). Empty: nothing.
///
/// # Errors
/// As [`current`]; [`Error::Source`] for a conversation that was not imported.
pub async fn changed_since_sync(imp: &Importer, id: Uuid) -> Result<Vec<String>> {
    let Some(r) = row(imp, id).await? else {
        return Err(Error::Source(format!("{id} was not imported")));
    };
    let (_, now) = current(imp, id, &r).await?;
    Ok(r.synced.changes(&now))
}

/// What changed in the conversation's sources since verification passed (`verified_stats`, cutover 7.8's re-stat
/// for a release that does not sync again). Not verified: one change saying so.
///
/// # Errors
/// As [`changed_since_sync`].
pub async fn changed_since_verify(imp: &Importer, id: Uuid) -> Result<Vec<String>> {
    let Some(r) = row(imp, id).await? else {
        return Err(Error::Source(format!("{id} was not imported")));
    };
    let Some(v) = &r.verified else {
        return Ok(vec!["not verified".to_owned()]);
    };
    let (_, now) = current(imp, id, &r).await?;
    Ok(v.changes(&now))
}

/// The source's sha256 and size per path, and the files that differ from magpie's reconstruction.
async fn compare(
    imp: &Importer,
    id: Uuid,
    person: &str,
    sources: &BTreeMap<String, Source>,
) -> Result<(Vec<Mismatch>, BTreeMap<String, (String, u64)>)> {
    let mut out = Vec::new();
    let mut hashes = BTreeMap::new();
    let mine = magpie_files(imp, id).await?;
    for (path, src) in sources {
        let (sha, n) = tree::hash_prefix(&src.abs, src.stat.size)?;
        let got = reconstruct(imp, id, person, path).await?;
        if got.as_ref() != Some(&(sha.clone(), n)) {
            out.push(Mismatch {
                path: path.clone(),
                source_size: Some(n),
                magpie_size: got.map(|g| g.1),
            });
        }
        hashes.insert(path.clone(), (sha, n));
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
    Ok((out, hashes))
}

fn is_memory(path: &str) -> bool {
    path == tree::MEMORY || path.starts_with(&format!("{}/", tree::MEMORY))
}

/// A released conversation's source files as they are recorded (`files`, from the sync inside its release) and as a
/// stat of them finds them now (each recorded file at its recorded `src`, and any new one in its folders), by cc path, the person's shared auto-memory left out (their legacy conversations
/// rightly change it): equal when the old side has written nothing of the conversation since its release. The
/// hand-over loop's cheap check before [`released_check`] hashes anything.
///
/// # Errors
/// A source folder that cannot be read.
pub fn released_stats(imp: &Importer, id: Uuid, key: &str, psid: &str, files: &Files) -> Result<(Stats, Stats)> {
    let recorded: Stats = files
        .iter()
        .filter(|(p, _)| !is_memory(p))
        .map(|(p, f)| (p.clone(), (f.size, f.mtime_ns, f.ino)))
        .collect();
    // each recorded file where the record says it is (what the released check hashes), a gone one as no stat; then
    // any file the conversation's folders hold that the record does not
    let mut now = Stats::new();
    for (p, f) in files.iter().filter(|(p, _)| !is_memory(p)) {
        let st = tree::stat_opt(std::path::Path::new(&f.src))?;
        now.insert(p.clone(), st.as_ref().map_or((u64::MAX, i64::MIN, 0), stat_of));
    }
    for (p, s) in tree::sources(&imp.jfs, key, psid, Some(id))? {
        if !is_memory(&p) && !files.contains_key(&p) {
            now.insert(p, stat_of(&s.stat));
        }
    }
    Ok((recorded, now))
}

/// (size, mtime, inode) by cc path.
pub type Stats = BTreeMap<String, (u64, i64, u64)>;

/// A released conversation's check (what `verify` runs for it), counting each file the old side wrote after a
/// hand-over's release in `magpie_import_verify_mismatch_total` on this process's registry.
///
/// # Errors
/// As [`verify`].
pub async fn released_check(imp: &Importer, id: Uuid) -> Result<Verdict> {
    let Some(r) = row(imp, id).await? else {
        return Err(Error::Source(format!("{id} was not imported")));
    };
    released(id, imp, &r)
}

/// A released conversation: the old side wrote nothing of its own after the hand-over. Every recorded file still
/// has its size and sha256, and no new file appeared in its session folder or as its transcript. The person's
/// shared auto-memory is left out: their other conversations, still the old stack's, rightly change it. After a
/// forced release the differences are the cut-off turn's writes: [`Verdict::forced_writes`], not mismatches.
fn released(id: Uuid, imp: &Importer, r: &Row) -> Result<Verdict> {
    let mut mismatches = Vec::new();
    for (path, rec) in r.files.iter().filter(|(p, _)| !is_memory(p)) {
        let (sha, n) = tree::hash_prefix(std::path::Path::new(&rec.src), u64::MAX).unwrap_or_default();
        if sha != rec.sha256 || n != rec.size {
            mismatches.push(Mismatch {
                path: path.clone(),
                source_size: Some(n),
                magpie_size: None,
            });
        }
    }
    for (path, src) in tree::sources(&imp.jfs, &r.key, &r.psid, Some(id))? {
        if !is_memory(&path) && !r.files.contains_key(&path) {
            mismatches.push(Mismatch {
                path,
                source_size: Some(src.stat.size),
                magpie_size: None,
            });
        }
    }
    let (mismatches, forced_writes) = if r.forced {
        if !mismatches.is_empty() {
            tracing::warn!(conversation = %id, files = ?mismatches, "the old side wrote after a forced release");
        }
        (Vec::new(), mismatches)
    } else {
        // the old side wrote a conversation magpie owns (import_verify_mismatch_total, DESIGN 9.9)
        if !mismatches.is_empty() {
            imp.magpie.legacy.verify_mismatch(mismatches.len());
        }
        (mismatches, Vec::new())
    };
    Ok(Verdict {
        conversation: id,
        released: true,
        files: r.files.len(),
        reingested: Vec::new(),
        mismatches,
        forced_writes,
        pending: Vec::new(),
        notes: Vec::new(),
    })
}

/// Verifies one conversation (cutover 7.7) and records the outcome. An open one passes only when its files equal
/// magpie's reconstruction and its sessions row, failed turns and sent folders are what the last pass synced; then
/// `verified_stats` holds every source's stat and every fingerprint from that same read.
///
/// # Errors
/// [`Error::Source`] for a conversation that was not imported; a read error.
#[allow(clippy::too_many_lines)] // compare, re-ingest, compare again, then one update of the record
pub async fn verify(imp: &Importer, id: Uuid) -> Result<Verdict> {
    let Some(r) = row(imp, id).await? else {
        return Err(Error::Source(format!("{id} was not imported")));
    };
    if r.released {
        return released(id, imp, &r);
    }
    let (sources, now) = current(imp, id, &r).await?;
    // as in a pass: a main transcript that vanished while its sessions row is there is never unlinked by a re-ingest
    let main = tree::main_path(&r.psid);
    if r.files.contains_key(&main) && !sources.contains_key(&main) && !now.row_fp.is_empty() {
        return Err(Error::Source(format!(
            "{main} was synced before and is not on the mount now (its sessions row is still there)"
        )));
    }
    let pending: Vec<String> = r
        .synced
        .clone()
        .rows_only()
        .changes(&now.clone().rows_only())
        .into_iter()
        .map(|c| format!("{c} since the last pass"))
        .collect();
    let (first, hashes) = compare(imp, id, &r.person, &sources).await?;
    let mut reingested = Vec::new();
    let mut files = r.files.clone();
    let (mismatches, hashes) = if first.is_empty() {
        (first, hashes)
    } else {
        let workspace = tree::workspace_uploads(&imp.jfs, id)?;
        let mut w = Writer::new(imp, id, &r.person, workspace);
        for m in &first {
            tracing::warn!(
                conversation = %id, path = %m.path, source = ?m.source_size, magpie = ?m.magpie_size,
                "reconstruct mismatch"
            );
            if let Some(src) = sources.get(&m.path) {
                let rec = w.replace(&m.path, src).await?;
                files.insert(m.path.clone(), rec);
            } else {
                w.unlink(&m.path).await?;
                files.remove(&m.path);
            }
            reingested.push(m.path.clone());
        }
        // the lines a rewrite took out become one retract now, as after a pass's writes
        crate::conv::flush_limbo(imp, id).await?;
        compare(imp, id, &r.person, &sources).await?
    };
    if !mismatches.is_empty() {
        // a reconstruction that is still not byte-exact after the re-ingest (import_verify_mismatch_total, DESIGN 9.9)
        imp.magpie.legacy.verify_mismatch(mismatches.len());
    }
    let ok = mismatches.is_empty() && pending.is_empty();
    if ok {
        // the record is what verification read: every source with its hash
        files = sources
            .iter()
            .filter_map(|(p, s)| {
                hashes.get(p).map(|(sha, n)| {
                    (
                        p.clone(),
                        Recorded {
                            src: s.abs.to_string_lossy().into_owned(),
                            size: *n,
                            mtime_ns: s.stat.mtime_ns,
                            ino: s.stat.ino,
                            sha256: sha.clone(),
                        },
                    )
                })
            })
            .collect();
    }
    let msg = (!ok).then(|| {
        let mut what: Vec<String> = mismatches.iter().map(|m| m.path.clone()).collect();
        what.extend(pending.iter().cloned());
        format!("could not be verified: {}", what.join(", "))
    });
    let stats = if ok { Some(serde_json::to_value(&now)?) } else { None };
    let files_json = serde_json::to_value(&files)?;
    let write_files = ok || !reingested.is_empty();
    let pool = &imp.magpie.pool;
    let synced_at = r.synced_at;
    let n = aviary_pg::tx!(pool, |t| -> Result<Option<bool>> {
        imp.check_lock_in(t).await?;
        let Some(c) = t
            .query_opt(
                "select c.migrating, s.last_synced_at from conversations c
                   join import_sources s on s.conversation_id = c.id
                  where c.id = $1 for update of c",
                &[&id],
            )
            .await?
        else {
            return Ok(None);
        };
        if !c.get::<_, bool>(0) {
            return Ok(None);
        }
        // a pass synced in between: this verification's read is older than the record, which it must not replace
        if c.get::<_, time::OffsetDateTime>(1) != synced_at {
            return Ok(Some(false));
        }
        t.execute(
            "update import_sources set files = case when $5 then $2 else files end,
                    verified_at = case when $3::jsonb is null then null else now() end, verified_stats = $3,
                    last_error = $4
             where conversation_id = $1",
            &[&id, &files_json, &stats, &msg, &write_files],
        )
        .await?;
        Ok(Some(true))
    })
    .await?;
    match n {
        None => return Err(Error::Released),
        Some(false) => {
            return Err(Error::Source(
                "a pass synced it during the verification: verify it again".to_owned(),
            ));
        }
        Some(true) => {}
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
        forced_writes: Vec::new(),
        pending,
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

/// After a clean `verify --all` (every conversation in `verified` equal): records the import complete in
/// `legacy_import_verified`, which hermit's `legacy-cleanup` asks for (`GET /v1/hermit/legacy-import`) before it
/// removes the old `users/` and `sent/` trees (cutover 10.6). Recorded only when the import holds at least one
/// conversation, every imported conversation is released to magpie and was verified in this run, and every sessions
/// row of the source (read now) is imported or purged on the new side; otherwise the reason, and the record stays as
/// it was. The caller holds the importer's lock, so no pass ran during the verification.
///
/// # Errors
/// A database statement.
pub async fn record_complete(imp: &Importer, verified: &std::collections::BTreeSet<Uuid>) -> Result<Option<String>> {
    let snap = crate::source::read(&imp.cloudcli).await?;
    if let Some((schema, why)) = snap.problems.first() {
        return Ok(Some(format!("the source has problems ({schema}: {why})")));
    }
    let db = imp.magpie.pool.get().await?;
    let rows = db
        .query(
            "select conversation_id, released_at is not null as released from import_sources",
            &[],
        )
        .await?;
    if rows.is_empty() {
        return Ok(Some("no conversation is imported".to_owned()));
    }
    let mut imported = std::collections::BTreeSet::new();
    for r in &rows {
        let id: Uuid = r.get(0);
        if !r.get::<_, bool>(1) {
            return Ok(Some(format!("conversation {id} is not released to magpie")));
        }
        if !verified.contains(&id) {
            return Ok(Some(format!("conversation {id} was not verified in this run")));
        }
        imported.insert(id);
    }
    let purged: std::collections::BTreeSet<Uuid> = db
        .query("select conversation_id from import_purged", &[])
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    for p in &snap.persons {
        for s in &p.sessions {
            let id = s.id()?;
            if !imported.contains(&id) && !purged.contains(&id) {
                return Ok(Some(format!(
                    "the source's sessions row {id} ({}) is not imported",
                    p.schema
                )));
            }
        }
    }
    db.execute(
        "insert into legacy_import_verified (verified_at, conversations) values (now(), $1)
         on conflict (id) do update set verified_at = excluded.verified_at, conversations = excluded.conversations",
        &[&i64::try_from(imported.len()).unwrap_or(i64::MAX)],
    )
    .await?;
    tracing::info!(conversations = imported.len(), "the import is recorded complete");
    Ok(None)
}
