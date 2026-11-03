//! Sync convergence (cutover 7.12 item 3, a property test): a reference source folder is changed by random sequences
//! of appends, partial-line appends and their completion, rewrites from a line, truncate-then-rewrite, compaction
//! (a temporary file renamed over), relocation, file creation and removal and sessions-row edits, with passes at
//! random points. After every pass, magpie's reconstruction equals the source byte for byte and the visible events
//! equal the classification of the source's current complete lines; the title and Recently deleted follow the row.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

mod common;

use std::collections::BTreeSet;

use common::{World, assistant, user};
use magpie_import::{pass, verify};
use proptest::prelude::*;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

#[derive(Debug, Clone)]
enum Op {
    Append(u8),
    Partial,
    Complete,
    RewriteFrom(u8, u8),
    TruncateRewrite(u8, u8),
    Compaction(u8),
    Create,
    Relocate,
    Remove,
    Rename,
    Delete,
    Restore,
    Pass,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (1u8..4).prop_map(Op::Append),
        1 => Just(Op::Partial),
        1 => Just(Op::Complete),
        1 => (0u8..8, 0u8..3).prop_map(|(k, n)| Op::RewriteFrom(k, n)),
        1 => (0u8..8, 0u8..3).prop_map(|(k, n)| Op::TruncateRewrite(k, n)),
        1 => (0u8..8).prop_map(Op::Compaction),
        1 => Just(Op::Create),
        1 => Just(Op::Relocate),
        1 => Just(Op::Remove),
        1 => Just(Op::Rename),
        1 => Just(Op::Delete),
        1 => Just(Op::Restore),
        3 => Just(Op::Pass),
    ]
}

/// The reference source of one conversation.
struct Model {
    key: String,
    id: Uuid,
    main: String,
    psid: Uuid,
    lines: Vec<Vec<u8>>,
    /// The bytes after the last newline (half a line) and the rest of that line.
    tail: Option<(Vec<u8>, Vec<u8>)>,
    files: BTreeSet<String>,
    next: u32,
    title: String,
    deleted: bool,
}

impl Model {
    fn line(&mut self) -> Vec<u8> {
        self.next += 1;
        let n = self.next;
        let ts = format!("2026-10-22T06:{:02}:{:02}.000Z", (n / 60) % 60, n % 60);
        if n.is_multiple_of(2) {
            assistant(&format!("u{n}"), &ts, &format!("answer {n}"))
        } else {
            user(&format!("u{n}"), &ts, &format!("question {n}"))
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut b: Vec<u8> = self.lines.concat();
        if let Some((t, _)) = &self.tail {
            b.extend_from_slice(t);
        }
        b
    }
}

async fn visible(w: &World, id: Uuid) -> i64 {
    w.magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from events e where e.conversation_id = $1 and e.kind in ('cc.user', 'cc.assistant')
               and not exists (select 1 from events r where r.conversation_id = $1 and r.kind = 'retract'
                               and r.data->'seqs' @> to_jsonb(e.seq))",
            &[&id],
        )
        .await
        .unwrap()
        .get(0)
}

async fn check(w: &World, m: &Model) {
    let only: BTreeSet<Uuid> = [m.id].into_iter().collect();
    let r = pass::pass(&w.imp, Some(&only)).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let v = verify::verify(&w.imp, m.id).await.unwrap();
    assert!(v.ok() && v.reingested.is_empty(), "{v:?}");
    assert!(v.notes.iter().all(|n| n.starts_with("no workspace")), "{v:?}");
    assert_eq!(
        visible(w, m.id).await,
        i64::try_from(m.lines.len()).unwrap(),
        "visible events"
    );
    let c = w.conv(m.id).await;
    assert_eq!(c["title"], m.title.as_str());
    assert_eq!(c["deleted"], m.deleted, "Recently deleted follows the row");
}

async fn case(w: &World, key: &str, ops: Vec<Op>) {
    let (id, psid) = (Uuid::new_v4(), Uuid::new_v4());
    let mut m = Model {
        key: key.to_owned(),
        id,
        main: format!("projects/-workspace/{psid}.jsonl"),
        psid,
        lines: Vec::new(),
        tail: None,
        files: BTreeSet::new(),
        next: 0,
        title: "first".to_owned(),
        deleted: false,
    };
    w.cloudcli(&format!(
        "insert into cc_{key}.sessions (session_id, provider_session_id, custom_name, created_at, updated_at)
           values ('{id}', '{psid}', 'first', '2026-10-22 06:00:00', '2026-10-22 06:00:00')"
    ))
    .await;
    let first = m.line();
    m.lines.push(first);
    w.put(&m.key, &m.main, &m.bytes());
    check(w, &m).await;
    for o in ops {
        match o {
            Op::Append(n) => {
                if m.tail.is_none() {
                    for _ in 0..n {
                        let l = m.line();
                        w.append(&m.key, &m.main, &l);
                        m.lines.push(l);
                    }
                }
            }
            Op::Partial => {
                if m.tail.is_none() {
                    let l = m.line();
                    let (a, b) = l.split_at(l.len() / 2);
                    w.append(&m.key, &m.main, a);
                    m.tail = Some((a.to_vec(), b.to_vec()));
                }
            }
            Op::Complete => {
                if let Some((a, b)) = m.tail.take() {
                    w.append(&m.key, &m.main, &b);
                    m.lines.push([a, b].concat());
                }
            }
            Op::RewriteFrom(k, n) | Op::TruncateRewrite(k, n) => {
                let k = usize::from(k).min(m.lines.len());
                m.lines.truncate(k);
                m.tail = None;
                for _ in 0..n {
                    let l = m.line();
                    m.lines.push(l);
                }
                let path = w.claude(&m.key).join(&m.main);
                if matches!(o, Op::TruncateRewrite(..)) {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&path)
                        .unwrap();
                }
                std::fs::write(&path, m.bytes()).unwrap();
            }
            Op::Compaction(k) => {
                let k = usize::from(k).min(m.lines.len());
                m.lines.truncate(k);
                m.tail = None;
                let l = m.line();
                m.lines.push(l);
                w.rename_over(&m.key, &m.main, &m.bytes());
            }
            Op::Create => {
                let name = format!("projects/-workspace/{}/tool-results/t{}.txt", m.psid, m.next);
                m.next += 1;
                w.put(&m.key, &name, format!("output {}\n", m.next).as_bytes());
                m.files.insert(name);
            }
            Op::Relocate => {
                if let Some(f) = m.files.iter().next().cloned() {
                    let to = format!("{f}.moved");
                    let base = w.claude(&m.key);
                    std::fs::rename(base.join(&f), base.join(&to)).unwrap();
                    m.files.remove(&f);
                    m.files.insert(to);
                }
            }
            Op::Remove => {
                if let Some(f) = m.files.iter().next().cloned() {
                    std::fs::remove_file(w.claude(&m.key).join(&f)).unwrap();
                    m.files.remove(&f);
                }
            }
            Op::Rename => {
                m.title = format!("title {}", m.next);
                w.cloudcli(&format!(
                    "update cc_{}.sessions set custom_name = '{}' where session_id = '{}'",
                    m.key, m.title, m.id
                ))
                .await;
            }
            Op::Delete => {
                m.deleted = true;
                w.cloudcli(&format!(
                    "update cc_{}.sessions set \"isArchived\" = 1, deleted_at = '2026-10-23 00:00:{:02}'
                     where session_id = '{}'",
                    m.key,
                    m.next % 60,
                    m.id
                ))
                .await;
            }
            Op::Restore => {
                m.deleted = false;
                w.cloudcli(&format!(
                    "update cc_{}.sessions set \"isArchived\" = 0, deleted_at = null where session_id = '{}'",
                    m.key, m.id
                ))
                .await;
            }
            Op::Pass => check(w, &m).await,
        }
    }
    check(w, &m).await;
}

#[test]
fn random_source_changes_converge_after_every_pass() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let w = rt.block_on(World::new("converge_prop"));
    let key = rt.block_on(w.person(ALICE));
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig {
        cases: 48,
        failure_persistence: None,
        ..ProptestConfig::default()
    });
    runner
        .run(&proptest::collection::vec(op(), 1..16), |ops| {
            rt.block_on(case(&w, &key, ops));
            Ok(())
        })
        .unwrap();
}
