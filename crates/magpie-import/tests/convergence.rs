//! Sync convergence (cutover 7.12 item 3, a property test): a reference source folder is changed by random sequences
//! of appends, partial-line appends and their completion, rewrites from a line, truncate-then-rewrite, an in-place
//! rewrite (written over at an offset, then cut), compaction (a temporary file renamed over), relocation of a file
//! and of a folder with its files, file creation and removal and sessions-row edits, with whole passes at random
//! points. The lines are of several kinds (prompts, answers, tool calls and their results, meta and system
//! lines, which are not shown) and a line can be written again with another kind under the same uuid. After every
//! pass, magpie's reconstruction equals the source byte for byte and the visible events (every `cc.*` kind) equal magpie's classifier
//! run over the source's current complete lines (kind and uuid of each, as a multiset like magpie's own ccfs model
//! test: a line written again keeps or takes its event by sha, and a changed line's new event comes at the next seq,
//! magpie 6.6); the title and Recently deleted follow the row.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

mod common;

use std::collections::BTreeSet;

use common::{World, assistant, line, user};
use magpie_import::{pass, verify};
use proptest::prelude::*;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

#[derive(Debug, Clone)]
enum Op {
    Append(u8),
    Retype(u8),
    Partial,
    Complete,
    RewriteFrom(u8, u8),
    TruncateRewrite(u8, u8),
    InPlace(u8, u8),
    Compaction(u8),
    Create,
    Relocate,
    MoveFolder,
    Remove,
    Rename,
    Delete,
    Restore,
    Pass,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (1u8..4).prop_map(Op::Append),
        1 => (0u8..8).prop_map(Op::Retype),
        1 => Just(Op::Partial),
        1 => Just(Op::Complete),
        1 => (0u8..8, 0u8..3).prop_map(|(k, n)| Op::RewriteFrom(k, n)),
        1 => (0u8..8, 0u8..3).prop_map(|(k, n)| Op::TruncateRewrite(k, n)),
        1 => (0u8..8, 0u8..3).prop_map(|(k, n)| Op::InPlace(k, n)),
        1 => (0u8..8).prop_map(Op::Compaction),
        1 => Just(Op::Create),
        1 => Just(Op::Relocate),
        1 => Just(Op::MoveFolder),
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
        kind_line(n, n)
    }

    fn bytes(&self) -> Vec<u8> {
        let mut b: Vec<u8> = self.lines.concat();
        if let Some((t, _)) = &self.tail {
            b.extend_from_slice(t);
        }
        b
    }
}

/// Line `n` (uuid `u{n}`) of kind `k % 6`: a prompt, an answer, a tool call, its result, a meta line, a system line.
fn kind_line(n: u32, k: u32) -> Vec<u8> {
    let ts = format!("2026-10-22T06:{:02}:{:02}.000Z", (n / 60) % 60, n % 60);
    let uuid = format!("u{n}");
    match k % 6 {
        0 => assistant(&uuid, &ts, &format!("answer {n}")),
        1 => user(&uuid, &ts, &format!("question {n}")),
        2 => line(&serde_json::json!({
            "type": "assistant", "uuid": uuid, "timestamp": ts, "sessionId": "s",
            "message": {"id": format!("msg_{uuid}"), "role": "assistant", "model": "claude-test",
                        "content": [{"type": "tool_use", "id": format!("toolu_{n}"), "name": "Bash",
                                     "input": {"command": format!("echo {n}")}}]}
        })),
        3 => line(&serde_json::json!({
            "type": "user", "uuid": uuid, "timestamp": ts, "sessionId": "s",
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": format!("toolu_{}", n - 1),
                                                     "content": format!("{n}\n")}]}
        })),
        4 => line(&serde_json::json!({
            "type": "user", "uuid": uuid, "timestamp": ts, "sessionId": "s", "isMeta": true,
            "message": {"role": "user", "content": format!("<local-command-caveat>{n}</local-command-caveat>")}
        })),
        _ => line(&serde_json::json!({
            "type": "system", "uuid": uuid, "timestamp": ts, "sessionId": "s", "subtype": "info",
            "content": format!("note {n}")
        })),
    }
}

/// The visible events magpie's classifier makes of the source's complete lines (kind, uuid), in order. An imported
/// conversation has no `user.sent`, so no line is an echo.
fn expected(m: &Model) -> Vec<(String, Option<String>)> {
    let mut v: Vec<(String, Option<String>)> = m
        .lines
        .iter()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_slice(l).unwrap();
            match magpie::ccfs::classify::classify(Some(&v), magpie::ccfs::classify::Context::default()) {
                magpie::ccfs::classify::Class::Event(e) => Some((e.kind.to_owned(), e.uuid.clone())),
                _ => None,
            }
        })
        .collect();
    v.sort();
    v
}

/// The conversation's visible Claude Code events (kind, uuid), sorted: every `cc.*` kind, so an event of a kind the
/// classifier does not make (a leak) fails the comparison too.
async fn visible(w: &World, id: Uuid) -> Vec<(String, Option<String>)> {
    let mut v: Vec<(String, Option<String>)> = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query(
            "select e.kind, e.data->>'uuid' from events e
              where e.conversation_id = $1 and e.kind like 'cc.%'
                and not exists (select 1 from events r where r.conversation_id = $1 and r.kind = 'retract'
                                and r.data->'seqs' @> to_jsonb(e.seq))
              order by e.seq",
            &[&id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    v.sort();
    v
}

async fn check(w: &World, m: &Model) {
    // a whole pass (the purge, the shares and the view-as rows run too), over the earlier cases' conversations as well
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let v = verify::verify(&w.imp, m.id).await.unwrap();
    assert!(v.ok() && v.reingested.is_empty(), "{v:?}");
    assert!(v.notes.iter().all(|n| n.starts_with("no workspace")), "{v:?}");
    assert_eq!(visible(w, m.id).await, expected(m), "visible events");
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
            Op::Retype(k) => {
                // line k written again under its uuid as another kind (a prompt becomes a tool result, …)
                if m.tail.is_none() && !m.lines.is_empty() {
                    let k = usize::from(k) % m.lines.len();
                    let v: serde_json::Value = serde_json::from_slice(&m.lines[k]).unwrap();
                    let n: u32 = v["uuid"].as_str().unwrap()[1..].parse().unwrap();
                    m.next += 1;
                    m.lines[k] = kind_line(n, m.next);
                    std::fs::write(w.claude(&m.key).join(&m.main), m.bytes()).unwrap();
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
            Op::InPlace(k, n) => {
                // Claude Code's in-place rewrite (`performRemoveByUuid`): the lines from k on written at their offset
                // over the old bytes (no truncate first), then the file cut to its new length
                let k = usize::from(k).min(m.lines.len());
                let at: usize = m.lines[..k].iter().map(Vec::len).sum();
                m.lines.truncate(k);
                m.tail = None;
                for _ in 0..n {
                    let l = m.line();
                    m.lines.push(l);
                }
                let bytes = m.bytes();
                let f = std::fs::OpenOptions::new()
                    .write(true)
                    .open(w.claude(&m.key).join(&m.main))
                    .unwrap();
                std::os::unix::fs::FileExt::write_all_at(&f, &bytes[at..], at as u64).unwrap();
                f.set_len(bytes.len() as u64).unwrap();
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
                // in one of two folders of saved tool output, so a folder can move with several files in it
                let name = format!(
                    "projects/-workspace/{}/tool-results/d{}/t{}.txt",
                    m.psid,
                    m.next % 2,
                    m.next
                );
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
            Op::MoveFolder => {
                // a folder renamed with its files (rule 6 pairs each gone file with its new path by its bytes)
                if let Some(f) = m.files.iter().find(|f| f.contains("/tool-results/d")).cloned() {
                    let (dir, _) = f.rsplit_once('/').unwrap();
                    m.next += 1;
                    let to = format!("projects/-workspace/{}/subagents/moved{}", m.psid, m.next);
                    let base = w.claude(&m.key);
                    std::fs::create_dir_all(base.join(&to).parent().unwrap()).unwrap();
                    std::fs::rename(base.join(dir), base.join(&to)).unwrap();
                    let prefix = format!("{dir}/");
                    let moved: Vec<String> = m.files.iter().filter(|p| p.starts_with(&prefix)).cloned().collect();
                    for p in moved {
                        m.files.remove(&p);
                        m.files.insert(format!("{to}/{}", &p[prefix.len()..]));
                    }
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
