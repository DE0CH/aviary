//! magpie 20's `lines.rs` reference model: random sequences of appends (split at any byte, torn tails), torn-tail
//! repairs, tombstone removals, whole-file rewrites, overwrites, truncates, compaction rename-overs, set-asides with
//! a fresh file, renames away from and back onto the main path, the main path being set late (promotion), and person
//! turns (each writes its prompt, the echo of `user.sent`, and ends the turn before it, flushing that turn's limbo),
//! run against magpie with a plain byte vector as the oracle. After every step the reconstructed file equals the
//! model and the visible events equal the classification of the model's lines (echoes left out) plus the events
//! waiting in limbo; at the end (limbo flushed) each event belongs to exactly one main-transcript line and nothing is
//! duplicated, missing or retracted twice.
//!
//! The seed changes every run and is printed (`AVIARY_TEST_CCFS_SEED=<n>` runs one again);
//! `AVIARY_TEST_CCFS_CASES` sets the number of cases (default 64).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    clippy::many_single_char_names
)]

mod common;

use std::collections::{BTreeSet, HashSet};

use base64::Engine as _;
use common::cc::{SID, Sandbox, flush_turn, main_path, person_turn, set_main};
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
    /// A person's turn (when the main path is known): the earlier turn ends (its limbo flushed), this one's prompt
    /// (its echo) and these pool lines are appended in its writes.
    PersonTurn(Vec<usize>),
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
        2 => proptest::collection::vec(0usize..40, 0..3).prop_map(Step::PersonTurn),
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
    s: &'a mut Sandbox,
    model: Vec<u8>,
    main_known: bool,
    n: usize,
    /// The person turn new writes are credited to.
    turn: Option<Uuid>,
    /// Lines written as a person turn's echo (no event while they keep these bytes).
    echoes: HashSet<Vec<u8>>,
    /// The case and the seed, for failure messages.
    ctx: String,
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
            "after {what} in {}: the file differs from the model ({} vs {} bytes)",
            self.ctx,
            got.len(),
            self.model.len()
        );
        // the visible events: the model's lines' (once the main path is known) and those waiting in limbo
        let conn = POOL.get().unwrap().get().await.unwrap();
        let mut visible = events_of(
            &conn
                .query(
                    "select kind, coalesce(data->>'uuid', '') from events e
                      where conversation_id = $1 and kind like 'cc.%'
                        and not exists (select 1 from events r where r.conversation_id = $1 and r.kind = 'retract'
                                         and r.data->'seqs' @> to_jsonb(e.seq))",
                    &[&self.s.conversation],
                )
                .await
                .unwrap(),
        );
        visible.sort();
        let mut want = if self.main_known {
            expected(&self.model, &self.echoes)
        } else {
            Vec::new()
        };
        want.extend(events_of(
            &conn
                .query(
                    "select e.kind, coalesce(e.data->>'uuid', '') from cc_limbo l
                       join events e on e.conversation_id = l.scope and e.seq = l.event_seq
                      where l.scope = $1 and l.reason = 'truncated'",
                    &[&self.s.conversation],
                )
                .await
                .unwrap(),
        ));
        want.sort();
        assert_eq!(visible, want, "after {what} in {}: the visible events", self.ctx);
    }

    async fn run(&mut self, step: &Step, pool: &[Vec<u8>]) {
        let p = main_path();
        self.n += 1;
        match step {
            Step::PersonTurn(idx) => {
                if !self.main_known {
                    return;
                }
                let sql = POOL.get().unwrap();
                flush_turn(sql, self.s.conversation, self.turn).await;
                self.turn = Some(person_turn(sql, self.s).await);
                let echo = line(&json!({"type": "user", "uuid": format!("echo{}", self.n),
                    "message": {"role": "user", "content": format!("typed {}", self.n)}}));
                // a torn tail before it gets Claude Code's `\n` first, so the prompt is a line of its own
                let mut bytes = if self.model.is_empty() || self.model.ends_with(b"\n") {
                    Vec::new()
                } else {
                    b"\n".to_vec()
                };
                bytes.extend(&echo);
                bytes.extend(idx.iter().flat_map(|&i| pool[i].clone()));
                self.echoes.insert(echo);
                let at = self.model.len();
                self.write_chunks(&p, at, &bytes, 1 << 20).await;
                self.model.extend(&bytes);
            }
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

fn events_of(rows: &[aviary_pg::Row]) -> Vec<(String, String)> {
    rows.iter().map(|r| (r.get(0), r.get(1))).collect()
}

/// `(kind, uuid)` of the events the model's main-transcript lines classify into (the echoes make none).
fn expected(model: &[u8], echoes: &HashSet<Vec<u8>>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = lines_of(model)
        .into_iter()
        .filter_map(|(_, l)| {
            if echoes.contains(l) {
                return None;
            }
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
    let seed: u64 = aviary_core::env::test_only("AVIARY_TEST_CCFS_SEED")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(7, |d| d.as_nanos() as u64)
        });
    let cases: usize = aviary_core::env::test_only("AVIARY_TEST_CCFS_CASES")
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    eprintln!("ccfs_model: AVIARY_TEST_CCFS_SEED={seed} AVIARY_TEST_CCFS_CASES={cases}");
    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&seed.to_le_bytes());
    let mut runner = TestRunner::new_with_rng(Config::default(), TestRng::from_seed(RngAlgorithm::ChaCha, &key));
    let strategy = proptest::collection::vec(step(), 4..24);
    let all: Vec<Vec<Step>> = (0..cases)
        .map(|_| strategy.new_tree(&mut runner).unwrap().current())
        .collect();
    let (mut events, mut retracts, mut echoes) = (0usize, 0usize, 0usize);
    // eight cases at a time, each its own conversation
    for (chunk_no, chunk) in all.chunks(8).enumerate() {
        let runs = chunk.iter().enumerate().map(|(k, steps)| {
            let case = chunk_no * 8 + k;
            run_case(&r, &sql, &pool, case, steps, seed)
        });
        for (e, x, n) in futures_util::future::join_all(runs).await {
            events += e;
            retracts += x;
            echoes += n;
        }
    }
    // the cases exercised what they are for
    assert!(
        events > 100 && retracts > 10 && echoes > 10,
        "{events} events, {retracts} retracted, {echoes} echoes (AVIARY_TEST_CCFS_SEED={seed})"
    );
}

/// One case; `(events, retracted, echoes)` at its end.
async fn run_case(
    r: &common::Replica,
    sql: &aviary_pg::Pool,
    pool: &[Vec<u8>],
    case: usize,
    steps: &[Step],
    seed: u64,
) -> (usize, usize, usize) {
    let mut s = Sandbox::leased(r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    let main_known = case.is_multiple_of(3);
    if main_known {
        set_main(sql, s.conversation, &main_path()).await;
    }
    s.write(&main_path(), 0, &[]).await.unwrap();
    let conversation = s.conversation;
    let ctx = format!("case {case} (AVIARY_TEST_CCFS_SEED={seed}): {steps:?}");
    let mut c = Case {
        s: &mut s,
        model: Vec::new(),
        main_known,
        n: 0,
        turn: None,
        echoes: HashSet::new(),
        ctx: ctx.clone(),
    };
    for st in steps {
        c.run(st, pool).await;
    }
    c.run(&Step::SetMain, pool).await;
    // the end of the last turn, and of operations credited to none, flushes limbo
    flush_turn(sql, conversation, c.turn).await;
    flush_turn(sql, conversation, None).await;
    c.check(&format!("the final flush in {ctx}")).await;
    let conn = sql.get().await.unwrap();
    // every visible event is exactly one main-transcript line's, and every such line's event is visible
    let visible = conn
        .query(
            "select seq from events e
              where conversation_id = $1 and kind like 'cc.%'
                and not exists (select 1 from events r where r.conversation_id = $1 and r.kind = 'retract'
                                 and r.data->'seqs' @> to_jsonb(e.seq))",
            &[&conversation],
        )
        .await
        .unwrap();
    let seqs: BTreeSet<i64> = visible.iter().map(|r| r.get(0)).collect();
    let rows = conn
        .query(
            "select l.event_seq from cc_lines l join cc_files f on f.id = l.file_id
              where f.scope = $1 and f.path = $2 and l.event_seq is not null",
            &[&conversation, &main_path()],
        )
        .await
        .unwrap();
    let line_seqs: Vec<i64> = rows.iter().map(|r| r.get(0)).collect();
    let distinct: BTreeSet<i64> = line_seqs.iter().copied().collect();
    assert_eq!(distinct.len(), line_seqs.len(), "{ctx}: two lines share an event");
    assert_eq!(distinct, seqs, "{ctx}");
    // retracts only name events that exist, once
    let retracted: Vec<i64> = conn
        .query(
            "select jsonb_array_elements(data->'seqs')::bigint from events where conversation_id = $1 and kind = 'retract'",
            &[&conversation],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    let once: BTreeSet<i64> = retracted.iter().copied().collect();
    assert_eq!(once.len(), retracted.len(), "{ctx}: an event retracted twice");
    // no event was ever made for an echo line as it was written, retracted or not
    for e in &c.echoes {
        let v: Value = serde_json::from_slice(e).unwrap();
        let n: i64 = conn
            .query_one(
                "select count(*) from events where conversation_id = $1 and kind like 'cc.%' and data = $2",
                &[&conversation, &v],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 0, "{ctx}: an echo became an event: {v}");
    }
    // the quota counts what is stored
    let tree = c.s.tree().await.unwrap();
    assert_eq!(
        tree.files.iter().find(|f| f.path == main_path()).unwrap().size,
        c.model.len() as u64
    );
    (seqs.len(), retracted.len(), c.echoes.len())
}
