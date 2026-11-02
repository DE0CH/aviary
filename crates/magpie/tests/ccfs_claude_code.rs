//! The Claude Code 2.1.304 fixture run (magpie 20, WP-12 acceptance): the real transcripts the FUSE gate's scripted
//! session produced (`tests/fixtures/claude-code-2.1.304`), replayed through the cc routes in the shape of the
//! gate's recorded calls (its `calls.log`): a session whose transcript starts in another project directory, the
//! relocation onto an occupied main path (set-aside, rename of the transcript and of its session folder), the
//! subagent's transcript and its meta file written by temp file and rename, appends in the recorded sizes (up to
//! 1 MiB), a tombstone removal, the compaction's temp file renamed over the transcript, and the turns after it. Every
//! prompt line is a person's: written as the echo of a new person-origin turn (the earlier turn's end flushes its
//! limbo), so the echo path runs through the promotion, the tombstone, the compaction and the appends.
//!
//! It passes when every file reads back byte for byte, the events equal the classification of the final
//! transcript's lines without the echoes (no duplicate, none missing, each event one line, no prompt event ever made
//! for an echo), the counts are the pinned ones (a classifier change for 2.1.304's lines shows here), and the
//! subagent transcript made no event.
//!
//! This is a replay of the gate's files in the shape of its calls, not a run of the binary: the run of Claude Code
//! itself against a mock Messages API through a FUSE supervisor and magpie (magpie 20) is WP-28's harness.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    clippy::cast_possible_wrap
)]

mod common;

use std::collections::BTreeSet;

use common::cc::{SID, Sandbox, flush_turn, main_path, person_turn, set_main};
use magpie::ccfs::classify::{Class, Context, classify};
use serde_json::{Value, json};

const ALICE: &str = "alice@example.org";
const MAIN: &[u8] = include_bytes!("fixtures/claude-code-2.1.304/main.jsonl");
const AGENT: &[u8] = include_bytes!("fixtures/claude-code-2.1.304/agent.jsonl");
const AGENT_META: &[u8] = include_bytes!("fixtures/claude-code-2.1.304/agent.meta.json");
/// Where the compaction's temp file ends in the final transcript (the gate's `compact.tmp` write: 30,654 bytes).
const COMPACTED: usize = 30_654;
/// The write sizes of the gate's appends after the compaction, cycled.
const SIZES: &[usize] = &[
    549, 4963, 303, 4032, 3589, 4004, 3514, 1_048_576, 10_215, 108, 2246, 29_172,
];

fn lines_of(b: &[u8]) -> Vec<&[u8]> {
    b.split_inclusive(|&c| c == b'\n')
        .filter(|l| l.ends_with(b"\n"))
        .collect()
}

fn line(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(v).unwrap();
    b.push(b'\n');
    b
}

/// `(kind, uuid)` of the events `b`'s lines classify into, the prompts left out (each is a person turn's echo).
fn classified(b: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = lines_of(b)
        .into_iter()
        .filter_map(
            |l| match classify(serde_json::from_slice::<Value>(l).ok().as_ref(), Context::default()) {
                Class::Event(e) if !e.is_prompt => Some((e.kind.to_owned(), e.uuid.unwrap_or_default())),
                _ => None,
            },
        )
        .collect();
    out.sort();
    out
}

/// A prompt a person typed (the echo of a turn's `user.sent` when its turn's echo is pending).
fn is_prompt(l: &[u8]) -> bool {
    matches!(classify(serde_json::from_slice::<Value>(l).ok().as_ref(), Context::default()),
        Class::Event(e) if e.is_prompt)
}

/// Appends `bytes` like [`append`]; each prompt line starts a new person turn (the earlier turn's end flushes its
/// limbo) and is written as its echo, in that turn's writes.
async fn append_turns(
    s: &mut Sandbox,
    sql: &aviary_pg::Pool,
    path: &str,
    at: &mut usize,
    bytes: &[u8],
    sizes: &[usize],
    turn: &mut Option<uuid::Uuid>,
) {
    let mut cuts = vec![0];
    let mut off = 0;
    for l in bytes.split_inclusive(|&c| c == b'\n') {
        if off > 0 && l.ends_with(b"\n") && is_prompt(l) {
            cuts.push(off);
        }
        off += l.len();
    }
    cuts.push(bytes.len());
    for w in cuts.windows(2) {
        let seg = &bytes[w[0]..w[1]];
        if seg.split_inclusive(|&c| c == b'\n').next().is_some_and(is_prompt) {
            flush_turn(sql, s.conversation, *turn).await;
            *turn = Some(person_turn(sql, s).await);
        }
        append(s, path, at, seg, sizes).await;
    }
}

async fn append(s: &Sandbox, path: &str, at: &mut usize, bytes: &[u8], sizes: &[usize]) {
    let mut rest = bytes;
    for size in sizes.iter().cycle() {
        if rest.is_empty() {
            break;
        }
        let n = (*size).min(rest.len());
        s.write(path, *at as u64, &rest[..n]).await.unwrap();
        *at += n;
        rest = &rest[n..];
    }
}

#[tokio::test]
async fn claude_code_2_1_304_produces_no_duplicate_and_no_missing_event() {
    let db = common::fresh_db("cc_2_1_304").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let sql = common::sql(&db.url);
    let mut s = Sandbox::leased(&r, ALICE).await;
    let mut turn = None;
    let conv = s.conversation;
    let main = main_path();
    let old_dir = "projects/-old";
    let old = format!("{old_dir}/{SID}.jsonl");
    // the supervisor's running report names the main path before anything is written
    set_main(&sql, conv, &main).await;

    // the occupant at the main path (the gate's 18-byte file), made the way Claude Code makes files: create, O_TRUNC,
    // write
    s.mkdirs("projects/-workspace").await;
    s.write(&main, 0, &[]).await.unwrap();
    s.truncate(&main, 0).await.unwrap();
    s.write(&main, 0, b"{\"occupant\":true}\n").await.unwrap();

    // the session starts in another project directory
    s.mkdirs(&format!("{old_dir}/memory")).await;
    let kept: Vec<&[u8]> = lines_of(&MAIN[..COMPACTED]);
    let history: Vec<Vec<u8>> = (0..30)
        .map(|i| {
            if i % 2 == 0 {
                line(&json!({"type": "user", "uuid": format!("h{i}"), "message": {"content": format!("old prompt {i}")}}))
            } else {
                line(&json!({"type": "assistant", "uuid": format!("h{i}"), "message": {"model": "m",
                    "content": [{"type": "text", "text": format!("old reply {i}")}]}}))
            }
        })
        .collect();
    let tombstone = line(
        &json!({"type": "assistant", "uuid": "tomb", "message": {"content": [{"type": "text",
        "text": "a streamed reply that failed"}]}}),
    );
    let mut pre: Vec<u8> = Vec::new();
    for h in &history[..10] {
        pre.extend(h);
    }
    let mut at = 0;
    s.write(&old, 0, &[]).await.unwrap();
    append_turns(&mut s, &sql, &old, &mut at, &pre, &[141_646, 437, 116_065], &mut turn).await;

    // the subagent: its folder, the meta file by temp + rename, its transcript in two writes
    let agent_dir = format!("{old_dir}/{SID}/subagents");
    s.mkdirs(&agent_dir).await;
    for (n, meta) in [(1, &AGENT_META[..162.min(AGENT_META.len())]), (2, AGENT_META)] {
        let tmp = format!("{agent_dir}/agent-a6b07cb0ea181b635.meta.json.tmp.{n}");
        s.write(&tmp, 0, &[]).await.unwrap();
        s.write(&tmp, 0, meta).await.unwrap();
        s.truncate(&tmp, meta.len() as u64).await.unwrap();
        s.rename(&tmp, &format!("{agent_dir}/agent-a6b07cb0ea181b635.meta.json"))
            .await
            .unwrap();
    }
    let agent = format!("{agent_dir}/agent-a6b07cb0ea181b635.jsonl");
    s.write(&agent, 0, &[]).await.unwrap();
    let mut agent_at = 0;
    append(&s, &agent, &mut agent_at, AGENT, &[71_730]).await;
    assert_eq!(
        super_events(&sql, conv).await,
        0,
        "nothing on the main path has events yet"
    );

    // the relocation: the occupant is set aside, the transcript and its folder move onto the main path
    let aside = format!("projects/-workspace/{SID}.jsonl.superseded-1793623733649");
    s.rename(&main, &aside).await.unwrap();
    s.setattr(&aside, None, Some(1_793_623_733_649_000_000)).await.unwrap();
    s.rename(&old, &main).await.unwrap();
    s.rename(&format!("{old_dir}/{SID}"), &format!("projects/-workspace/{SID}"))
        .await
        .unwrap();
    let promoted = super_events(&sql, conv).await;
    assert_eq!(
        promoted, 5,
        "the moved transcript's lines are classified once, on arrival, its prompts as their turns' echoes"
    );

    // more turns, with a tombstone removal in the middle (truncate at its start, the tail written back)
    let mut more = Vec::new();
    for h in &history[10..20] {
        more.extend(h);
    }
    more.extend(&tombstone);
    for h in &history[20..] {
        more.extend(h);
    }
    for k in &kept {
        more.extend(*k);
    }
    append_turns(
        &mut s,
        &sql,
        &main,
        &mut at,
        &more,
        &[5200, 13_258, 7411, 5230, 1216, 1_048_576],
        &mut turn,
    )
    .await;
    let full = [pre.clone(), more.clone()].concat();
    assert_eq!(s.read(&main).await.unwrap(), full);
    let cut = full
        .windows(tombstone.len())
        .position(|w| w == tombstone.as_slice())
        .unwrap();
    let tail = full[cut + tombstone.len()..].to_vec();
    s.truncate(&main, cut as u64).await.unwrap();
    s.write(&main, cut as u64, &tail).await.unwrap();
    let full = [&full[..cut], &tail[..]].concat();
    assert_eq!(s.read(&main).await.unwrap(), full);
    // that turn ends: the removed line is retracted
    assert_eq!(flush_turn(&sql, conv, turn).await, 1);

    // the compaction: the temp file (create, O_TRUNC, one write), renamed over the transcript
    let tmp = format!("projects/-workspace/{SID}.jsonl.compact.tmp.c966ffe5");
    s.write(&tmp, 0, &[]).await.unwrap();
    s.truncate(&tmp, 0).await.unwrap();
    s.write(&tmp, 0, &MAIN[..COMPACTED]).await.unwrap();
    s.rename(&tmp, &main).await.unwrap();
    assert_eq!(s.read(&main).await.unwrap(), &MAIN[..COMPACTED]);

    // the turns after it, in the gate's write sizes (the torn line among them)
    let mut at = COMPACTED;
    append_turns(&mut s, &sql, &main, &mut at, &MAIN[COMPACTED..], SIZES, &mut turn).await;
    // the cleanup sweep cannot delete history; the set-aside occupant can go
    assert_eq!(common::cc::code(&s.unlink(&main).await.unwrap_err()), "protected");
    s.unlink(&aside).await.unwrap();

    // every file byte for byte
    assert_eq!(s.read(&main).await.unwrap(), MAIN);
    let agent = format!("projects/-workspace/{SID}/subagents/agent-a6b07cb0ea181b635.jsonl");
    assert_eq!(s.read(&agent).await.unwrap(), AGENT);
    assert_eq!(
        s.read(&format!(
            "projects/-workspace/{SID}/subagents/agent-a6b07cb0ea181b635.meta.json"
        ))
        .await
        .unwrap(),
        AGENT_META
    );

    // the last turn ends
    flush_turn(&sql, conv, turn).await;
    flush_turn(&sql, conv, None).await;

    let c = sql.get().await.unwrap();
    let visible = c
        .query(
            "select seq, kind, coalesce(data->>'uuid', '') from events e
              where conversation_id = $1 and kind like 'cc.%'
                and not exists (select 1 from events r where r.conversation_id = $1 and r.kind = 'retract'
                                 and r.data->'seqs' @> to_jsonb(e.seq))",
            &[&conv],
        )
        .await
        .unwrap();
    let mut got: Vec<(String, String)> = visible.iter().map(|r| (r.get(1), r.get(2))).collect();
    got.sort();
    assert_eq!(
        got,
        classified(MAIN),
        "the visible events are the final transcript's, once each, no echo among them"
    );
    let seqs: BTreeSet<i64> = visible.iter().map(|r| r.get(0)).collect();
    let line_seqs: Vec<i64> = c
        .query(
            "select l.event_seq from cc_lines l join cc_files f on f.id = l.file_id
              where f.scope = $1 and f.path = $2 and l.event_seq is not null",
            &[&conv, &main],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(line_seqs.iter().copied().collect::<BTreeSet<_>>(), seqs);
    assert_eq!(line_seqs.len(), seqs.len());

    // the pinned counts: 2.1.304's lines in this session, and what the rewrites did
    let counts = c
        .query(
            "select kind, count(*) from events where conversation_id = $1 group by kind order by kind",
            &[&conv],
        )
        .await
        .unwrap();
    let counts: Vec<(String, i64)> = counts.iter().map(|r| (r.get(0), r.get(1))).collect();
    let kinds = |k: &str| counts.iter().find(|c| c.0 == k).map_or(0, |c| c.1);
    let final_user = got.iter().filter(|g| g.0 == "cc.user").count() as i64;
    let final_assistant = got.iter().filter(|g| g.0 == "cc.assistant").count() as i64;
    assert_eq!(
        (final_user, final_assistant),
        (PINNED_USER, PINNED_ASSISTANT),
        "2.1.304's classification moved"
    );
    // every history reply, the tombstone and each final line made exactly one event; kept lines made none twice
    assert_eq!(
        kinds("cc.user") + kinds("cc.assistant"),
        16 + final_user + final_assistant
    );
    // no prompt event was made for an echo, through the promotion, the tombstone, the compaction or the appends
    let prompts: i64 = c
        .query_one(
            "select count(*) from events where conversation_id = $1 and is_prompt",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(prompts, 0);
    let turns: i64 = c
        .query_one(
            "select count(*) from turns where conversation_id = $1 and origin = 'person' and echo_matched",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(turns, 15 + PINNED_PROMPTS, "every person turn's echo was matched");
    // the compaction's retract (the history) and the tombstone's
    assert_eq!(kinds("retract"), 2);
    let retracted: i64 = c
        .query_one(
            "select coalesce(sum(jsonb_array_length(data->'seqs')), 0)::bigint from events
              where conversation_id = $1 and kind = 'retract'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(retracted, 16);
    let subagent_events: i64 = c
        .query_one(
            "select count(*) from events where conversation_id = $1 and data->>'agentId' is not null",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(subagent_events, 0, "the subagent transcript is not the main transcript");
}

/// The classification of the fixture's final transcript: its `cc.user` and `cc.assistant` lines that are not
/// prompts, and its prompts (each a person turn's echo here).
const PINNED_USER: i64 = 2;
/// See [`PINNED_USER`].
const PINNED_PROMPTS: i64 = 5;
/// See [`PINNED_USER`].
const PINNED_ASSISTANT: i64 = 5;

async fn super_events(pool: &aviary_pg::Pool, conv: uuid::Uuid) -> i64 {
    pool.get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from events where conversation_id = $1 and kind like 'cc.%'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0)
}
