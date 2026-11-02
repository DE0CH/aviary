//! magpie 20's `lines.rs` reference model: random sequences of appends (split at any byte, torn tails), torn-tail
//! repairs, tombstone removals, whole-file rewrites, overwrites, truncates, compaction rename-overs, set-asides with
//! a fresh file, renames away from and back onto the main path, and the main path being set late (promotion), run
//! against magpie with a plain byte vector as the oracle. After every step the reconstructed file equals the model;
//! at the end (limbo flushed) the visible events equal the classification of the model's main-transcript lines,
//! each event belongs to exactly one main-transcript line, and nothing is duplicated or missing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    clippy::many_single_char_names
)]

mod common;

use std::collections::BTreeSet;

use base64::Engine as _;
use common::cc::{SID, Sandbox, main_path, set_main};
use magpie::ccfs::classify::{Class, Context, classify};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::{Value, json};
use uuid::Uuid;

const ALICE: &str = "alice@example.org";

fn line(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(v).unwrap();
    b.push(b'\n');
    b
}

fn png(seed: u32) -> String {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&(seed + 1).to_be_bytes());
    bytes.extend_from_slice(&7u32.to_be_bytes());
    bytes.resize(1500, (seed % 251) as u8);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The pool of distinct lines a case draws from (each with its own uuid).
fn pool() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for i in 0..40u32 {
        let uuid = format!("u{i}");
        let l = match i % 8 {
            0 => line(&json!({"type": "user", "uuid": uuid, "message": {"content": format!("prompt {i}")}})),
            1 | 5 => line(&json!({"type": "assistant", "uuid": uuid, "message": {"model": "m",
                "content": [{"type": "text", "text": format!("reply {i} é\u{1F600}")}]}})),
            2 => line(
                &json!({"type": "assistant", "uuid": uuid, "message": {"content": [{"type": "tool_use",
                "id": format!("toolu_{i}"), "name": "Read", "input": {}}]}}),
            ),
            3 => line(
                &json!({"type": "user", "uuid": uuid, "message": {"content": [{"type": "tool_result",
                "tool_use_id": format!("toolu_{}", i - 1), "content": [{"type": "image", "source": {"data": png(i)}}]}]},
                "toolUseResult": {"file": {"base64": png(i)}}}),
            ),
            4 => line(
                &json!({"type": "user", "uuid": uuid, "message": {"content": [{"type": "tool_result",
                "tool_use_id": "toolu_x", "content": format!("{i}").repeat(17_000)}]}}),
            ),
            6 => line(&json!({"type": "attachment", "uuid": uuid, "n": i})),
            _ => format!("not json {i} {{\n").into_bytes(),
        };
        out.push(l);
    }
    out
}

#[derive(Debug, Clone)]
enum Step {
    /// Append these pool lines, written in chunks of these sizes.
    Append(Vec<usize>, Vec<usize>),
    /// Write the first half of a line (a torn tail), then Claude Code's repair: `\n` and the whole line.
    TornRepair(usize),
    /// Remove the line at this index (from the end) the way Claude Code removes a tombstone.
    Tombstone(usize),
    /// Rewrite the whole file without the lines at these indices, in chunks.
    Rewrite(Vec<usize>, usize),
    /// Overwrite bytes inside the file.
    Overwrite(usize, Vec<u8>),
    /// Truncate at a fraction of the size (thousandths).
    Truncate(u16),
    /// Compaction: a temp file with a boundary, these new lines and the last `k` lines, renamed over the main path.
    Compact(Vec<usize>, usize),
    /// Set the main transcript aside and write a fresh one with the same content and these new lines.
    SetAside(Vec<usize>),
    /// Rename away, append these lines there, rename back.
    AwayAndBack(Vec<usize>),
    /// The main path becomes known (promotion), if it is not yet.
    SetMain,
}

fn step() -> impl Strategy<Value = Step> {
    let idx = || proptest::collection::vec(0usize..40, 1..5);
    let chunks = || proptest::collection::vec(1usize..400, 1..4);
    prop_oneof![
        4 => (idx(), chunks()).prop_map(|(l, c)| Step::Append(l, c)),
        1 => (0usize..40).prop_map(Step::TornRepair),
        2 => (0usize..4).prop_map(Step::Tombstone),
        1 => (proptest::collection::vec(0usize..20, 0..3), 1usize..2000).prop_map(|(d, c)| Step::Rewrite(d, c)),
        1 => (any::<usize>(), proptest::collection::vec(b'a'..=b'z', 1..12)).prop_map(|(p, b)| Step::Overwrite(p, b)),
        1 => (500u16..1000).prop_map(Step::Truncate),
        1 => (proptest::collection::vec(0usize..40, 0..3), 0usize..4).prop_map(|(n, k)| Step::Compact(n, k)),
        1 => proptest::collection::vec(0usize..40, 0..2).prop_map(Step::SetAside),
        1 => proptest::collection::vec(0usize..40, 0..2).prop_map(Step::AwayAndBack),
        1 => Just(Step::SetMain),
    ]
}

/// The complete lines of `b` and their start offsets.
fn lines_of(b: &[u8]) -> Vec<(usize, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(n) = b[at..].iter().position(|&c| c == b'\n') {
        out.push((at, &b[at..=at + n]));
        at += n + 1;
    }
    out
}

struct Case<'a> {
    s: &'a Sandbox,
    model: Vec<u8>,
    main_known: bool,
    n: usize,
}

impl Case<'_> {
    async fn write_chunks(&self, path: &str, start: usize, bytes: &[u8], chunk: usize) {
        let mut at = start;
        for c in bytes.chunks(chunk.max(1)) {
            self.s.write(path, at as u64, c).await.unwrap();
            at += c.len();
        }
    }

    async fn check(&self, what: &str) {
        let got = self.s.read(&main_path()).await.unwrap();
        assert!(
            got == self.model,
            "after {what}: the file differs from the model ({} vs {} bytes)",
            got.len(),
            self.model.len()
        );
    }

    async fn run(&mut self, step: &Step, pool: &[Vec<u8>]) {
        let p = main_path();
        self.n += 1;
        match step {
            Step::Append(idx, chunks) => {
                let bytes: Vec<u8> = idx.iter().flat_map(|&i| pool[i].clone()).collect();
                let mut at = self.model.len();
                let mut rest = bytes.as_slice();
                for c in chunks.iter().cycle() {
                    if rest.is_empty() {
                        break;
                    }
                    let take = (*c).min(rest.len());
                    self.s.write(&p, at as u64, &rest[..take]).await.unwrap();
                    at += take;
                    rest = &rest[take..];
                }
                self.model.extend(&bytes);
            }
            Step::TornRepair(i) => {
                let l = &pool[*i];
                let half = &l[..l.len() / 2];
                let at = self.model.len();
                self.s.write(&p, at as u64, half).await.unwrap();
                self.model.extend(half);
                let repair = [b"\n".as_slice(), l].concat();
                self.s.write(&p, self.model.len() as u64, &repair).await.unwrap();
                self.model.extend(&repair);
            }
            Step::Tombstone(k) => {
                let ls = lines_of(&self.model);
                if ls.len() <= *k {
                    return;
                }
                let (start, l) = ls[ls.len() - 1 - k];
                let rest = self.model[start + l.len()..].to_vec();
                self.s.truncate(&p, start as u64).await.unwrap();
                self.s.write(&p, start as u64, &rest).await.unwrap();
                self.model.truncate(start);
                self.model.extend(rest);
            }
            Step::Rewrite(drop, chunk) => {
                let ls = lines_of(&self.model);
                let complete: usize = ls.iter().map(|l| l.1.len()).sum();
                let mut new: Vec<u8> = ls
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !drop.contains(i))
                    .flat_map(|(_, l)| l.1.to_vec())
                    .collect();
                new.extend_from_slice(&self.model[complete..]);
                self.s.truncate(&p, 0).await.unwrap();
                self.write_chunks(&p, 0, &new, *chunk).await;
                self.model = new;
            }
            Step::Overwrite(pos, bytes) => {
                if self.model.is_empty() {
                    return;
                }
                let at = pos % self.model.len();
                self.s.write(&p, at as u64, bytes).await.unwrap();
                let end = at + bytes.len();
                if self.model.len() < end {
                    self.model.resize(end, 0);
                }
                self.model[at..end].copy_from_slice(bytes);
            }
            Step::Truncate(frac) => {
                let to = self.model.len() * usize::from(*frac) / 1000;
                self.s.truncate(&p, to as u64).await.unwrap();
                self.model.truncate(to);
            }
            Step::Compact(new, k) => {
                let ls = lines_of(&self.model);
                let kept: Vec<u8> = ls.iter().rev().take(*k).rev().flat_map(|l| l.1.to_vec()).collect();
                let mut tmp = line(&json!({"type": "system", "subtype": "compact_boundary", "n": self.n}));
                tmp.extend(new.iter().flat_map(|&i| pool[i].clone()));
                tmp.extend(kept);
                let path = format!("projects/-workspace/{SID}.jsonl.compact.tmp.{}", self.n);
                self.write_chunks(&path, 0, &tmp, 1 << 20).await;
                self.s.rename(&path, &p).await.unwrap();
                self.model = tmp;
            }
            Step::SetAside(new) => {
                let aside = format!("projects/-workspace/{SID}.jsonl.superseded-{}", self.n);
                self.s.rename(&p, &aside).await.unwrap();
                let mut fresh = self.model.clone();
                fresh.extend(new.iter().flat_map(|&i| pool[i].clone()));
                self.s.write(&p, 0, &[]).await.unwrap();
                self.write_chunks(&p, 0, &fresh, 997).await;
                assert_eq!(self.s.read(&aside).await.unwrap(), self.model);
                self.s.unlink(&aside).await.unwrap();
                self.model = fresh;
            }
            Step::AwayAndBack(new) => {
                let away = format!("projects/-workspace/{SID}.jsonl.bak");
                self.s.rename(&p, &away).await.unwrap();
                let extra: Vec<u8> = new.iter().flat_map(|&i| pool[i].clone()).collect();
                self.s.write(&away, self.model.len() as u64, &extra).await.unwrap();
                self.model.extend(extra);
                self.s.rename(&away, &p).await.unwrap();
            }
            Step::SetMain => {
                if !self.main_known {
                    set_main(POOL.get().unwrap(), self.s.conversation, &p).await;
                    self.main_known = true;
                }
            }
        }
        self.check(&format!("{step:?}")).await;
    }
}

static POOL: std::sync::OnceLock<aviary_pg::Pool> = std::sync::OnceLock::new();

/// `(kind, uuid)` of the events the model's main-transcript lines classify into.
fn expected(model: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = lines_of(model)
        .into_iter()
        .filter_map(|(_, l)| {
            let v: Value = serde_json::from_slice(l).ok()?;
            match classify(Some(&v), Context::default()) {
                Class::Event(e) => Some((e.kind.to_owned(), e.uuid.unwrap_or_default())),
                _ => None,
            }
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn random_operation_sequences_match_the_model() {
    let db = common::fresh_db("cc_model").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let _ = POOL.set(sql.clone());
    let pool = pool();
    let mut runner = TestRunner::new_with_rng(Config::default(), TestRng::from_seed(RngAlgorithm::ChaCha, &[7; 32]));
    let strategy = proptest::collection::vec(step(), 4..16);
    let (mut events, mut retracts) = (0usize, 0usize);
    for case in 0..24 {
        let steps = strategy.new_tree(&mut runner).unwrap().current();
        let s = Sandbox::leased(&r, ALICE).await;
        s.mkdirs("projects/-workspace").await;
        let mut c = Case {
            s: &s,
            model: Vec::new(),
            main_known: case % 3 == 0,
            n: 0,
        };
        if c.main_known {
            set_main(&sql, s.conversation, &main_path()).await;
        }
        s.write(&main_path(), 0, &[]).await.unwrap();
        for st in &steps {
            c.run(st, &pool).await;
        }
        c.run(&Step::SetMain, &pool).await;
        // the end of the turn flushes limbo
        aviary_pg::tx!(&sql, |t| -> magpie::error::Result<usize> {
            t.execute(
                "select 1 from conversations where id = $1 for update",
                &[&s.conversation],
            )
            .await?;
            magpie::ccfs::limbo::flush_scope(t, s.conversation, None).await
        })
        .await
        .unwrap();
        let conn = sql.get().await.unwrap();
        let visible = conn
            .query(
                "select seq, kind, coalesce(data->>'uuid', '') from events e
                  where conversation_id = $1 and kind like 'cc.%'
                    and not exists (select 1 from events r where r.conversation_id = $1 and r.kind = 'retract'
                                     and r.data->'seqs' @> to_jsonb(e.seq))",
                &[&s.conversation],
            )
            .await
            .unwrap();
        let mut got: Vec<(String, String)> = visible.iter().map(|r| (r.get(1), r.get(2))).collect();
        got.sort();
        assert_eq!(got, expected(&c.model), "case {case}: {steps:?}");
        // every visible event is exactly one main-transcript line's, and every such line's event is visible
        let seqs: BTreeSet<i64> = visible.iter().map(|r| r.get(0)).collect();
        let rows = conn
            .query(
                "select l.event_seq from cc_lines l join cc_files f on f.id = l.file_id
                  where f.scope = $1 and f.path = $2 and l.event_seq is not null",
                &[&s.conversation, &main_path()],
            )
            .await
            .unwrap();
        let line_seqs: Vec<i64> = rows.iter().map(|r| r.get(0)).collect();
        let distinct: BTreeSet<i64> = line_seqs.iter().copied().collect();
        assert_eq!(distinct.len(), line_seqs.len(), "case {case}: two lines share an event");
        assert_eq!(distinct, seqs, "case {case}: {steps:?}");
        // retracts only name events that exist, once
        let retracted: Vec<i64> = conn
            .query(
                "select jsonb_array_elements(data->'seqs')::bigint from events where conversation_id = $1 and kind = 'retract'",
                &[&s.conversation],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        let once: BTreeSet<i64> = retracted.iter().copied().collect();
        events += got.len();
        retracts += retracted.len();
        assert_eq!(once.len(), retracted.len(), "case {case}: an event retracted twice");
        // the quota counts what is stored
        let tree = s.tree().await.unwrap();
        assert_eq!(
            tree.files.iter().find(|f| f.path == main_path()).unwrap().size,
            c.model.len() as u64
        );
        let _ = Uuid::nil();
    }
    // the cases exercised what they are for
    assert!(events > 50 && retracts > 5, "{events} events, {retracts} retracted");
}
