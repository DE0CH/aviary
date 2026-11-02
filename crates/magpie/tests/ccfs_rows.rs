//! The timeline of a main transcript driven through the cc routes, checked against an oracle that does not share the
//! projection's rewrite handling (WP-13 review): random appends, truncates (their lines wait in limbo), rewrites of
//! a line with the same `uuid` and new content (by truncate-and-rewrite and by an overwrite inside the file), writes
//! of a removed line's original bytes (a limbo take-back), and quiet flushes. After every flush the conversation's
//! rows, per message, equal the rows a fresh conversation gets from only the transcript's surviving lines, appended
//! in file order with no `meta.replaces` (no rewrite, no retract, no takeover); every line's event still counts and
//! every counting `cc.*` event is a line's; and after every operation no two rows share a `(messageId, blockIndex)`,
//! no two tool rows a `callId`, and no two prompt rows a `messageId`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    clippy::many_single_char_names
)]

mod common;

use std::collections::{BTreeMap, HashSet};

use common::cc::{Sandbox, main_path, set_main};
use magpie::events::NewEvent;
use serde_json::{Value, json};
use uuid::Uuid;

const ALICE: &str = "alice@example.org";

/// One transcript line of the model: its uuid, what it is and its version (the content changes with it, the length
/// does not, so an overwrite in place can change it).
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Prompt,
    Text { message: String },
    ToolUse { message: String, call: String },
    Result { call: String },
}

#[derive(Debug, Clone, PartialEq)]
struct Line {
    uuid: String,
    kind: Kind,
    version: u32,
}

impl Line {
    fn bytes(&self) -> Vec<u8> {
        let v = format!("v{:04}", self.version);
        let j = match &self.kind {
            Kind::Prompt => json!({"type": "user", "uuid": self.uuid, "message": {"role": "user",
                "content": format!("prompt {v}")}}),
            Kind::Text { message } => json!({"type": "assistant", "uuid": self.uuid, "message": {"id": message,
                "model": "m-1", "content": [{"type": "text", "text": format!("text {v}")}]}}),
            Kind::ToolUse { message, call } => {
                json!({"type": "assistant", "uuid": self.uuid, "message": {"id": message,
                "model": "m-1", "content": [{"type": "tool_use", "id": call, "name": "Bash",
                "input": {"command": format!("echo {v}")}}]}})
            }
            Kind::Result { call } => json!({"type": "user", "uuid": self.uuid, "message": {"role": "user",
                "content": [{"type": "tool_result", "tool_use_id": call, "content": format!("out {v}")}]}}),
        };
        let mut b = serde_json::to_vec(&j).unwrap();
        b.push(b'\n');
        b
    }

    /// Where the version digits are in the line (an overwrite in place writes there).
    fn version_at(&self) -> usize {
        let b = self.bytes();
        let needle = format!("v{:04}", self.version);
        b.windows(needle.len()).position(|w| w == needle.as_bytes()).unwrap() + 1
    }
}

fn file_of(lines: &[Line]) -> Vec<u8> {
    lines.iter().flat_map(Line::bytes).collect()
}

async fn setup(test: &str) -> (common::TestDb, common::Replica, Sandbox) {
    let db = common::fresh_db(test).await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    set_main(r.pool(), s.conversation, &main_path()).await;
    (db, r, s)
}

async fn flush(pool: &aviary_pg::Pool, conv: Uuid) {
    aviary_pg::tx!(pool, |t| -> magpie::error::Result<usize> {
        t.execute("select 1 from conversations where id = $1 for update", &[&conv])
            .await?;
        magpie::ccfs::limbo::flush_scope(t, conv, None).await
    })
    .await
    .unwrap();
}

/// The rows a transcript makes: `(kind, item)` per message (`messageId`; a prompt's is its uuid), in block order with
/// the `blockIndex` itself left out. A block index is fixed when its row is made, and a line of the message that
/// arrives while an earlier line of it waits in limbo counts that line; when the limbo line is later retracted the
/// later rows keep their index (a gap, never a reorder or a duplicate: [`assert_unique`]), so the oracle compares the
/// order, not the numbers. The conversation's own first message (`user.sent`, with a `clientMessageId`) is left out.
async fn rows(sql: &aviary_pg::Pool, conv: Uuid) -> BTreeMap<String, Vec<(String, Value)>> {
    let rs = sql
        .get()
        .await
        .unwrap()
        .query(
            "select kind, message_id, item from rows where conversation_id = $1
              and not (kind = 'user' and item ? 'clientMessageId' and item->>'clientMessageId' is not null)
             order by message_id, block_index nulls first, row_id",
            &[&conv],
        )
        .await
        .unwrap();
    let mut out: BTreeMap<String, Vec<(String, Value)>> = BTreeMap::new();
    for r in rs {
        let kind: String = r.get(0);
        let mid: Option<String> = r.get(1);
        let mut item: Value = r.get(2);
        if let Some(o) = item.as_object_mut() {
            o.remove("blockIndex");
        }
        out.entry(mid.unwrap_or_default()).or_default().push((kind, item));
    }
    out
}

/// No `(messageId, blockIndex)` twice, no `callId` twice, no prompt `messageId` twice.
async fn assert_unique(sql: &aviary_pg::Pool, conv: Uuid, when: &str) {
    let c = sql.get().await.unwrap();
    let dup = c
        .query(
            "select kind, message_id, block_index, tool_use_id from rows where conversation_id = $1
             order by row_id",
            &[&conv],
        )
        .await
        .unwrap();
    let mut blocks = HashSet::new();
    let mut calls = HashSet::new();
    let mut prompts = HashSet::new();
    for r in dup {
        let kind: String = r.get(0);
        let mid: Option<String> = r.get(1);
        let bi: Option<i32> = r.get(2);
        let call: Option<String> = r.get(3);
        if let (Some(m), Some(b)) = (&mid, bi) {
            assert!(blocks.insert((m.clone(), b)), "{when}: two rows at ({m}, {b})");
        }
        if let Some(call) = call {
            assert!(calls.insert(call.clone()), "{when}: two tool rows of {call}");
        }
        if kind == "user"
            && let Some(m) = mid
        {
            assert!(prompts.insert(m.clone()), "{when}: two user rows of {m}");
        }
    }
}

/// After a flush: every main-transcript line's event counts, every counting `cc.*` event is a line's, and the rows
/// equal a fresh projection of the lines' events in file order.
async fn check(r: &common::Replica, sql: &aviary_pg::Pool, conv: Uuid, when: &str) {
    let c = sql.get().await.unwrap();
    let on_lines: Vec<i64> = c
        .query(
            "select l.event_seq from cc_lines l join cc_files f on f.id = l.file_id
              where f.scope = $1 and f.path = $2 and l.event_seq is not null order by l.ord",
            &[&conv, &main_path()],
        )
        .await
        .unwrap()
        .iter()
        .map(|x| x.get(0))
        .collect();
    let counting: Vec<i64> = c
        .query(
            "select seq from events e where conversation_id = $1 and kind like 'cc.%'
               and not exists (select 1 from retracted x where x.conversation_id = $1 and x.seq = e.seq)
             order by seq",
            &[&conv],
        )
        .await
        .unwrap()
        .iter()
        .map(|x| x.get(0))
        .collect();
    let mut sorted = on_lines.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, counting, "{when}: the lines' events are the counting events");
    // the oracle: the same events, in file order, in a fresh conversation, with no rewrite
    let fresh = common::ev::conversation(&r.pigeon(), ALICE).await;
    let mut evs = Vec::new();
    for seq in &on_lines {
        let e = c
            .query_one(
                "select kind, data, meta, text, is_prompt from events where conversation_id = $1 and seq = $2",
                &[&conv, seq],
            )
            .await
            .unwrap();
        let mut meta: Option<Value> = e.get(2);
        if let Some(Value::Object(m)) = meta.as_mut() {
            m.remove("replaces");
        }
        evs.push(NewEvent {
            kind: e.get(0),
            data: e.get(1),
            meta,
            turn_id: None,
            text: e.get(3),
            is_prompt: e.get(4),
            viewer: None,
            at: time::OffsetDateTime::now_utc(),
        });
    }
    if !evs.is_empty() {
        common::ev::append(r.pool(), ALICE, fresh, evs).await;
    }
    let (got, want) = (rows(sql, conv).await, rows(sql, fresh).await);
    if got != want {
        let keys: std::collections::BTreeSet<&String> = got.keys().chain(want.keys()).collect();
        for k in keys {
            if got.get(k) != want.get(k) {
                eprintln!("{when}: {k}\n  got  {:?}\n  want {:?}", got.get(k), want.get(k));
            }
        }
        panic!("{when}: the rows differ from a fresh projection of the transcript");
    }
}

/// Claude Code's truncate-and-rewrite of line `i` with new content (the same uuid): the cut, then the tail written
/// back with that line changed.
async fn rewrite_by_truncate(s: &Sandbox, lines: &mut [Line], i: usize) {
    let p = main_path();
    let cut = file_of(&lines[..i]).len() as u64;
    s.truncate(&p, cut).await.unwrap();
    lines[i].version += 1;
    s.write(&p, cut, &file_of(&lines[i..])).await.unwrap();
}

/// The reviewer's sequence: truncate (R in limbo), write X with R's uuid and new content (X replaces R and takes over
/// its rows), truncate again (X in limbo), write R's original bytes back, quiet flush. R's event must not come back
/// from limbo with no rows: the transcript's line shows in the timeline.
#[tokio::test]
async fn a_limbo_take_back_after_a_takeover_keeps_the_line_on_the_timeline() {
    let (db, r, s) = setup("ccrows_takeback").await;
    let sql = common::sql(&db.url);
    let conv = s.conversation;
    let p = main_path();
    for (round, kind) in [
        Kind::Text {
            message: "msg_a".into(),
        },
        Kind::Prompt,
        Kind::ToolUse {
            message: "msg_b".into(),
            call: "toolu_1".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let mut lines = vec![
            Line {
                uuid: "p0".into(),
                kind: Kind::Prompt,
                version: 0,
            },
            Line {
                uuid: format!("r{round}"),
                kind: kind.clone(),
                version: 0,
            },
        ];
        // each round starts from an empty file
        if round > 0 {
            s.truncate(&p, 0).await.unwrap();
            flush(&sql, conv).await;
        }
        s.write(&p, 0, &file_of(&lines)).await.unwrap();
        let cut = lines[0].bytes().len() as u64;
        let original = lines[1].clone();
        // truncate: R waits in limbo
        s.truncate(&p, cut).await.unwrap();
        // X: the same uuid, new content
        lines[1].version = 1;
        s.write(&p, cut, &lines[1].bytes()).await.unwrap();
        assert_unique(&sql, conv, "after X").await;
        // truncate again: X waits in limbo; R's original bytes come back
        s.truncate(&p, cut).await.unwrap();
        s.write(&p, cut, &original.bytes()).await.unwrap();
        assert_unique(&sql, conv, "after R's bytes").await;
        flush(&sql, conv).await;
        lines[1] = original;
        assert_eq!(s.read(&p).await.unwrap(), file_of(&lines));
        check(&r, &sql, conv, &format!("{kind:?}")).await;
        let shown = rows(&sql, conv).await;
        let uuid = &lines[1].uuid;
        let mid = match &lines[1].kind {
            Kind::Prompt => uuid.clone(),
            Kind::Text { message } | Kind::ToolUse { message, .. } => message.clone(),
            Kind::Result { .. } => unreachable!(),
        };
        assert!(shown.contains_key(&mid), "{kind:?}: the line's row shows");
    }
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

#[tokio::test]
async fn rows_after_rewrites_truncates_and_take_backs_equal_a_fresh_projection_of_the_transcript() {
    let (db, r, s) = setup("ccrows_oracle").await;
    let sql = common::sql(&db.url);
    let conv = s.conversation;
    let p = main_path();
    let mut lines: Vec<Line> = Vec::new();
    // lines a truncate removed, as they were (their bytes may be written back: a take-back)
    let mut removed: Vec<Line> = Vec::new();
    let mut n = 0u32;
    let mut message;
    let mut checks = 0;
    let mut counts = BTreeMap::<&str, usize>::new();
    for seed in 1..=8u64 {
        let mut g = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        for step in 0..40 {
            let when = format!("seed {seed} step {step}");
            // a truncate is mostly followed by its rewrite
            let pick = if !removed.is_empty() && g.chance(60) {
                60
            } else {
                g.below(100)
            };
            match pick {
                // append a line
                0..=44 => {
                    n += 1;
                    // a message's lines are consecutive (Claude Code writes one line per block, in order)
                    let last_message = match lines.last().map(|l| &l.kind) {
                        Some(Kind::Text { message } | Kind::ToolUse { message, .. }) => Some(message.clone()),
                        _ => None,
                    };
                    let open: Vec<String> = lines
                        .iter()
                        .filter_map(|l| match &l.kind {
                            Kind::ToolUse { call, .. } => Some(call.clone()),
                            _ => None,
                        })
                        .filter(|c| {
                            !lines
                                .iter()
                                .any(|l| matches!(&l.kind, Kind::Result { call } if call == c))
                        })
                        .collect();
                    let kind = match g.below(4) {
                        0 => Kind::Prompt,
                        1 if !open.is_empty() => Kind::Result {
                            call: open[g.below(open.len())].clone(),
                        },
                        k => {
                            message = match last_message {
                                Some(m) if g.chance(60) => m,
                                _ => format!("msg_{n}"),
                            };
                            if k == 2 {
                                Kind::Text {
                                    message: message.clone(),
                                }
                            } else {
                                Kind::ToolUse {
                                    message: message.clone(),
                                    call: format!("toolu_{n}"),
                                }
                            }
                        }
                    };
                    let l = Line {
                        uuid: format!("u{n}"),
                        kind,
                        version: 0,
                    };
                    let at = file_of(&lines).len() as u64;
                    s.write(&p, at, &l.bytes()).await.unwrap();
                    lines.push(l);
                    // Claude Code writes a rewritten line where the old one stood: removed lines come back only
                    // before anything new is appended
                    removed.clear();
                    *counts.entry("append").or_default() += 1;
                }
                // a truncate nothing rewrites yet
                45..=54 if !lines.is_empty() => {
                    let k = g.below(lines.len());
                    let cut = file_of(&lines[..k]).len() as u64;
                    s.truncate(&p, cut).await.unwrap();
                    removed = lines.split_off(k);
                    *counts.entry("truncate").or_default() += 1;
                }
                // the removed lines written back, one of them maybe with new content (or its original bytes again)
                55..=69 if !removed.is_empty() => {
                    let mut back = std::mem::take(&mut removed);
                    if g.chance(50) {
                        let i = g.below(back.len());
                        if back[i].version > 0 && g.chance(50) {
                            // an older version's bytes
                            back[i].version -= 1;
                        } else {
                            back[i].version += 1;
                            *counts.entry("rewrite after truncate").or_default() += 1;
                        }
                    }
                    let at = file_of(&lines).len() as u64;
                    s.write(&p, at, &file_of(&back)).await.unwrap();
                    lines.extend(back);
                    *counts.entry("write back").or_default() += 1;
                }
                // a truncate-and-rewrite of one line in one go
                70..=79 if !lines.is_empty() => {
                    removed.clear();
                    let i = g.below(lines.len());
                    rewrite_by_truncate(&s, &mut lines, i).await;
                    *counts.entry("truncate and rewrite").or_default() += 1;
                }
                // an overwrite inside the file: same uuid, new content, same length
                80..=87 if !lines.is_empty() => {
                    let i = g.below(lines.len());
                    if lines[i].version < 9999 {
                        let at = file_of(&lines[..i]).len() + lines[i].version_at();
                        lines[i].version += 1;
                        let digits = format!("{:04}", lines[i].version);
                        s.write(&p, at as u64, digits.as_bytes()).await.unwrap();
                        *counts.entry("overwrite").or_default() += 1;
                    }
                }
                // the review's sequence: truncate, a rewrite of the first removed line, truncate again, that line's
                // original bytes back (a take-back of the line the rewrite replaced)
                88..=92 if !lines.is_empty() => {
                    removed.clear();
                    let k = g.below(lines.len());
                    let cut = file_of(&lines[..k]).len() as u64;
                    let original = lines[k..].to_vec();
                    s.truncate(&p, cut).await.unwrap();
                    lines[k].version += 1;
                    s.write(&p, cut, &file_of(&lines[k..])).await.unwrap();
                    assert_unique(&sql, conv, &format!("{when} (rewritten)")).await;
                    s.truncate(&p, cut).await.unwrap();
                    s.write(&p, cut, &file_of(&original)).await.unwrap();
                    lines.truncate(k);
                    lines.extend(original);
                    *counts.entry("original bytes back").or_default() += 1;
                }
                // the quiet flush
                _ => {
                    flush(&sql, conv).await;
                    check(&r, &sql, conv, &when).await;
                    checks += 1;
                }
            }
            assert_eq!(s.read(&p).await.unwrap(), file_of(&lines), "{when}");
            assert_unique(&sql, conv, &when).await;
        }
        flush(&sql, conv).await;
        removed.clear();
        check(&r, &sql, conv, &format!("seed {seed} end")).await;
        checks += 1;
    }
    assert!(checks > 20, "{checks} checks");
    for k in [
        "append",
        "truncate",
        "write back",
        "rewrite after truncate",
        "original bytes back",
        "truncate and rewrite",
        "overwrite",
    ] {
        assert!(counts.get(k).copied().unwrap_or(0) > 2, "{k}: {counts:?}");
    }
}
