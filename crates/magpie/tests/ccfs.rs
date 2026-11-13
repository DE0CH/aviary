//! magpie 20's ccfs suite (WP-12): the cc routes against a real Postgres: files, lines and pages, the lease, `opId`
//! replays, rule 0, offsets, quotas and errno codes, the main transcript's events (classification, promotion,
//! limbo, retraction, rename-over, set-aside), blobs staged with the write.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    clippy::many_single_char_names,
    clippy::similar_names
)]

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use common::cc::{Sandbox, code, conversation, flush_turn, hermit_names, main_path, person_turn, set_main};
use magpie_api::sandbox::CcKind;
use serde_json::{Value, json};
use uuid::Uuid;

const ALICE: &str = "alice@example.org";

fn line(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(v).unwrap();
    b.push(b'\n');
    b
}

fn assistant(uuid: &str, text: &str) -> Vec<u8> {
    line(
        &json!({"type": "assistant", "uuid": uuid, "message": {"id": format!("msg_{uuid}"), "model": "m-1",
        "content": [{"type": "text", "text": text}], "usage": {"input_tokens": 3}}}),
    )
}

fn prompt(uuid: &str, text: &str) -> Vec<u8> {
    line(&json!({"type": "user", "uuid": uuid, "message": {"role": "user", "content": text}}))
}

fn internal(n: u32) -> Vec<u8> {
    line(&json!({"type": "attachment", "n": n}))
}

fn png_b64(len: usize) -> String {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&640u32.to_be_bytes());
    bytes.extend_from_slice(&480u32.to_be_bytes());
    let mut x: u32 = 7;
    while bytes.len() < len {
        // incompressible filler, like a real image
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        bytes.push((x >> 16) as u8);
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn image_result(uuid: &str, png: &str) -> Vec<u8> {
    line(
        &json!({"type": "user", "uuid": uuid, "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_img",
        "content": [{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png}}]}]},
        "toolUseResult": {"file": {"base64": png}}}),
    )
}

async fn setup(test: &str) -> (common::TestDb, common::Replica, Sandbox) {
    let db = common::fresh_db(test).await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    (db, r, s)
}

/// `(seq, kind, uuid)` of the conversation's `cc.*` events not retracted.
async fn visible(pool: &aviary_pg::Pool, conv: Uuid) -> Vec<(i64, String, Option<String>)> {
    let rows = pool
        .get()
        .await
        .unwrap()
        .query(
            "select seq, kind, data->>'uuid' from events e where conversation_id = $1 and kind like 'cc.%'
              and not exists (select 1 from events r where r.conversation_id = $1 and r.kind = 'retract'
                               and r.data->'seqs' @> to_jsonb(e.seq))
              order by seq",
            &[&conv],
        )
        .await
        .unwrap();
    rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect()
}

async fn kinds(pool: &aviary_pg::Pool, conv: Uuid) -> Vec<String> {
    let rows = pool
        .get()
        .await
        .unwrap()
        .query(
            "select kind from events where conversation_id = $1 order by seq",
            &[&conv],
        )
        .await
        .unwrap();
    rows.iter().map(|r| r.get(0)).collect()
}

async fn flush(pool: &aviary_pg::Pool, conv: Uuid) -> usize {
    aviary_pg::tx!(pool, |t| -> magpie::error::Result<usize> {
        t.execute("select 1 from conversations where id = $1 for update", &[&conv])
            .await?;
        magpie::ccfs::limbo::flush_scope(t, conv, None).await
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn files_directories_and_the_tree() {
    let (_db, _r, s) = setup("cc_files").await;
    let p = main_path();
    let mut content = Vec::new();
    for i in 0..5 {
        let l = internal(i);
        let at = content.len() as u64;
        let r = s.write(&p, at, &l).await.unwrap();
        content.extend_from_slice(&l);
        assert_eq!(r.size, content.len() as u64);
    }
    // a torn tail, completed by the next write
    s.write(&p, content.len() as u64, b"{\"type\":\"att").await.unwrap();
    content.extend_from_slice(b"{\"type\":\"att");
    assert_eq!(s.read(&p).await.unwrap(), content);
    s.write(&p, content.len() as u64, b"achment\"}\n").await.unwrap();
    content.extend_from_slice(b"achment\"}\n");
    assert_eq!(s.read(&p).await.unwrap(), content);
    let (status, part) = s.read_range(&p, 3, 10).await.unwrap();
    assert_eq!((status, part.as_slice()), (206, &content[3..13]));
    let (_, past) = s.read_range(&p, content.len() as u64 - 4, 100).await.unwrap();
    assert_eq!(past, &content[content.len() - 4..]);

    // byte files: pages, zero-fill past the end, truncate
    s.mkdirs("todos").await;
    s.write("todos/a.json", 0, b"hello").await.unwrap();
    s.write("todos/a.json", 300_000, b"far").await.unwrap();
    let a = s.read("todos/a.json").await.unwrap();
    assert_eq!(a.len(), 300_003);
    assert_eq!(&a[..5], b"hello");
    assert!(a[5..300_000].iter().all(|&b| b == 0));
    assert_eq!(&a[300_000..], b"far");
    s.truncate("todos/a.json", 4).await.unwrap();
    assert_eq!(s.read("todos/a.json").await.unwrap(), b"hell");
    s.truncate("todos/a.json", 6).await.unwrap();
    assert_eq!(s.read("todos/a.json").await.unwrap(), b"hell\0\0");

    let tree = s.tree().await.unwrap();
    let by: BTreeMap<String, _> = tree.files.iter().map(|e| (e.path.clone(), e)).collect();
    assert_eq!(by["projects"].kind, CcKind::Dir);
    assert_eq!(by[&p].kind, CcKind::Lines);
    assert_eq!(by[&p].size, content.len() as u64);
    assert_eq!(by[&p].mode, 0o600);
    assert_eq!(by["todos/a.json"].kind, CcKind::Bytes);
    assert_eq!(by["todos/a.json"].size, 6);
    assert_eq!(tree.quota_bytes, 2 << 30);
    assert!(tree.cc_bytes >= content.len() as u64 && tree.cc_bytes < content.len() as u64 + 100);

    // setattr bumps the version and keeps the supervisor's mtime
    let v = s.setattr("todos/a.json", Some(0o644), Some(42)).await.unwrap();
    let tree = s.tree().await.unwrap();
    let e = tree.files.iter().find(|e| e.path == "todos/a.json").unwrap();
    assert_eq!((e.mode, e.mtime_ns, e.version), (0o644, 42, v.version));

    // a rename of a temp file over another byte file (Claude Code's atomic writes)
    s.write("todos/a.json.tmp.1.ab", 0, b"new").await.unwrap();
    s.rename("todos/a.json.tmp.1.ab", "todos/a.json").await.unwrap();
    assert_eq!(s.read("todos/a.json").await.unwrap(), b"new");
    assert_eq!(code(&s.read("todos/a.json.tmp.1.ab").await.unwrap_err()), "not_found");

    // directories: mkdir-as-lock, rmdir, rename of a directory with its files
    assert_eq!(code(&s.mkdir("todos").await.unwrap_err()), "exists");
    assert_eq!(code(&s.rmdir("todos").await.unwrap_err()), "not_empty");
    s.mkdirs("projects/-workspace/x/sub").await;
    s.write("projects/-workspace/x/sub/f.jsonl", 0, &internal(9))
        .await
        .unwrap();
    s.rename("projects/-workspace/x", "projects/-workspace/y")
        .await
        .unwrap();
    assert_eq!(s.read("projects/-workspace/y/sub/f.jsonl").await.unwrap(), internal(9));
    assert_eq!(
        code(
            &s.rename("projects/-workspace/y", "projects/-workspace/y/sub/z")
                .await
                .unwrap_err()
        ),
        "bad_path"
    );
    s.unlink("projects/-workspace/y/sub/f.jsonl").await.unwrap();
    s.rmdir("projects/-workspace/y/sub").await.unwrap();

    // errno codes
    assert_eq!(code(&s.write("nope/f", 0, b"x").await.unwrap_err()), "not_found");
    assert_eq!(code(&s.write("../f", 0, b"x").await.unwrap_err()), "bad_path");
    assert_eq!(code(&s.write("a//b", 0, b"x").await.unwrap_err()), "bad_path");
    assert_eq!(
        code(&s.write(&p, content.len() as u64 + 1, b"x").await.unwrap_err()),
        "offset_beyond_eof"
    );
    assert_eq!(
        code(&s.truncate(&p, content.len() as u64 + 1).await.unwrap_err()),
        "offset_beyond_eof"
    );
    assert_eq!(code(&s.unlink("todos").await.unwrap_err()), "not_found");
    assert_eq!(code(&s.rmdir("todos/a.json").await.unwrap_err()), "not_found");
    let big = vec![b'x'; (16 << 20) + 1];
    assert_eq!(code(&s.write("todos/big", 0, &big).await.unwrap_err()), "too_large");
}

#[tokio::test]
async fn op_ids_replay_and_rule_zero() {
    let (_db, _r, s) = setup("cc_opid").await;
    let p = main_path();
    let op = Uuid::new_v4();
    let l = internal(1);
    let first = s.calls().cc_write(&s.wq(&p, 0, op), l.clone()).await.unwrap();
    // the same opId again (a lost response): the stored result, nothing written twice
    let again = s.calls().cc_write(&s.wq(&p, 0, op), l.clone()).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(s.read(&p).await.unwrap(), l);
    // the sentry's resend after a checkpoint: the same bytes at the same offset under a new opId (rule 0)
    let resent = s.write(&p, 0, &l).await.unwrap();
    assert_eq!(resent, first, "no change, the current size and version");
    // also a prefix of the file, and inside a later range
    s.write(&p, l.len() as u64, &internal(2)).await.unwrap();
    let both = [l.clone(), internal(2)].concat();
    let r = s.write(&p, 0, &both[..5]).await.unwrap();
    assert_eq!(r.size, both.len() as u64);
    let r2 = s.write(&p, 3, &both[3..20]).await.unwrap();
    assert_eq!(r2, r);
    // different bytes inside the file overwrite (pwrite), as 6.6 rule 4 says
    let mut changed = both.clone();
    changed[2] = b' ';
    s.write(&p, 2, b" ").await.unwrap();
    assert_eq!(s.read(&p).await.unwrap(), changed);
    // a truncate replayed
    let t = Uuid::new_v4();
    let tq = magpie_api::sandbox::CcTruncate {
        path: common::cc::home(&p),
        size: 3,
        mtime_ns: 5,
        op_id: t,
    };
    let a = s.calls().cc_truncate(&tq).await.unwrap();
    s.write(&p, 3, b"abc").await.unwrap();
    let b = s.calls().cc_truncate(&tq).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(
        s.read(&p).await.unwrap(),
        [&changed[..3], b"abc"].concat(),
        "the replay did not truncate again"
    );
    // a mkdir replayed answers its stored success, not `exists`
    let m = magpie_api::sandbox::CcMkdir {
        path: "claude/plans".into(),
        mode: 0o700,
        mtime_ns: 1,
        op_id: Uuid::new_v4(),
    };
    s.calls().cc_mkdir(&m).await.unwrap();
    s.calls().cc_mkdir(&m).await.unwrap();
}

#[tokio::test]
async fn the_lease() {
    let db = common::fresh_db("cc_lease").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let conv = conversation(&r, ALICE).await;
    let a_id = Uuid::new_v4();
    let mut a = Sandbox::new(&r, conv, a_id, ALICE);
    // hermit's feed names no sandbox and hermit cannot be reached: 503, never a lease on missing evidence
    assert_eq!(code(&a.take_lease().await.unwrap_err()), "unavailable");
    hermit_names(r.pool(), conv, Some(a_id), "starting").await;
    let first = a.take_lease().await.unwrap();
    assert_eq!(first.conversation_id, conv);
    assert!(!first.resync);
    a.mkdirs("projects/-workspace").await;
    // a call without the lease, or with an old one
    let none = Sandbox::new(&r, conv, a_id, ALICE);
    assert_eq!(code(&none.mkdir("plans").await.unwrap_err()), "lease_lost");
    let second = a.take_lease().await.unwrap();
    assert_eq!(second.epoch, first.epoch + 1);
    let mut old = Sandbox::new(&r, conv, a_id, ALICE);
    old.lease = Some(first.lease_id);
    assert_eq!(code(&old.mkdir("plans").await.unwrap_err()), "lease_lost");
    a.mkdir("plans").await.unwrap();
    // another sandbox for the same conversation: hermit names `a`, so it never gets the lease (hermit is asked and
    // cannot be reached: 503), and `a` keeps it
    let mut b = Sandbox::new(&r, conv, Uuid::new_v4(), ALICE);
    assert_eq!(code(&b.take_lease().await.unwrap_err()), "unavailable");
    a.mkdir("tasks").await.unwrap();
    // hermit moves on to `b` (a migration recorded): `b` takes the lease, `a` loses it at once
    hermit_names(r.pool(), conv, Some(b.id), "ready").await;
    b.take_lease().await.unwrap();
    assert_eq!(code(&a.mkdir("todos").await.unwrap_err()), "lease_lost");
    assert_eq!(code(&a.take_lease().await.unwrap_err()), "unavailable");
    // a paused or gone sandbox gets no lease
    hermit_names(r.pool(), conv, Some(b.id), "gone").await;
    assert_eq!(code(&b.take_lease().await.unwrap_err()), "unavailable");
    hermit_names(r.pool(), conv, Some(b.id), "ready").await;
    // the sandbox of someone else's conversation, or of no conversation
    let mut eve = Sandbox::new(&r, conv, b.id, "eve@example.org");
    assert_eq!(code(&eve.take_lease().await.unwrap_err()), "not_found");
    let mut ghost = Sandbox::new(&r, Uuid::new_v4(), b.id, ALICE);
    assert_eq!(code(&ghost.take_lease().await.unwrap_err()), "not_found");
    // resync: set by a stuck write, answered once by the next lease
    let sql = common::sql(&db.url);
    aviary_pg::tx!(&sql, |t| -> magpie::error::Result<()> {
        magpie::ccfs::set_resync(t, conv).await
    })
    .await
    .unwrap();
    assert!(b.take_lease().await.unwrap().resync);
    assert!(!b.take_lease().await.unwrap().resync);
}

#[tokio::test]
async fn the_lease_asks_hermit() {
    let db = common::fresh_db("cc_lease_hermit").await;
    let hermit = aviary_fakes::hermit::FakeHermit::unguarded(aviary_fakes::hermit::HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let conv = conversation(&r, ALICE).await;
    // hermit has no sandbox: not_current_sandbox
    let mut x = Sandbox::new(&r, conv, Uuid::new_v4(), ALICE);
    assert_eq!(code(&x.take_lease().await.unwrap_err()), "not_current_sandbox");
    // hermit starts one: only that sandbox gets the lease, and magpie records it
    let hc = hermit_api::client::Client::new(common::http(), running.url()).bearer("any");
    hc.put_sandbox(conv, &hermit_api::sandbox::PutSandboxRequest { person: ALICE.into() })
        .await
        .unwrap();
    let live = hermit.live(conv).unwrap();
    assert_eq!(code(&x.take_lease().await.unwrap_err()), "not_current_sandbox");
    let mut s = Sandbox::new(&r, conv, live.id, ALICE);
    s.take_lease().await.unwrap();
    let row = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select hermit_sandbox from conversations where id = $1", &[&conv])
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<Uuid>>(0), Some(live.id));
}

#[tokio::test]
async fn the_quota_and_staging_limits() {
    let (db, r, s) = setup("cc_quota").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    s.write(&p, 0, &internal(1)).await.unwrap();
    sql.get()
        .await
        .unwrap()
        .execute(
            "update conversations set cc_bytes = (2::bigint << 30) - 10 where id = $1",
            &[&s.conversation],
        )
        .await
        .unwrap();
    let n = internal(1).len() as u64;
    assert_eq!(code(&s.write(&p, n, &internal(2)).await.unwrap_err()), "quota_exceeded");
    // shrinking is always allowed
    s.truncate(&p, 0).await.unwrap();
    sql.get()
        .await
        .unwrap()
        .execute(
            "update conversations set cc_bytes = 0 where id = $1",
            &[&s.conversation],
        )
        .await
        .unwrap();
    // staging full (the bucket unreachable for long): writes that add a blob answer 503, others go on
    let png = png_b64(3000);
    s.write(&p, 0, &image_result("u1", &png)).await.unwrap();
    let staged: i64 = sql
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from blob_staging", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(staged, 1, "the image is staged once although the line holds it twice");
    sql.get()
        .await
        .unwrap()
        .execute("update blobs set size = 3::bigint << 30", &[])
        .await
        .unwrap();
    let at = s.read(&p).await.unwrap().len() as u64;
    let other = png_b64(3001);
    assert_eq!(
        code(&s.write(&p, at, &image_result("u2", &other)).await.unwrap_err()),
        "unavailable"
    );
    s.write(&p, at, &internal(3)).await.unwrap();
    // the same blob again is no new blob
    let at = s.read(&p).await.unwrap().len() as u64;
    s.write(&p, at, &image_result("u3", &png)).await.unwrap();
    drop(r);
}

#[tokio::test]
async fn blobs_are_staged_and_spliced_back() {
    let (db, _r, s) = setup("cc_blobs").await;
    let p = main_path();
    set_main(&common::sql(&db.url), s.conversation, &p).await;
    let png = png_b64(200_000);
    let long = "x".repeat(20_000);
    let mut content = image_result("u1", &png);
    content.extend(line(
        &json!({"type": "user", "uuid": "u2", "message": {"content": [{"type": "tool_result",
        "tool_use_id": "t", "content": long}]}}),
    ));
    // in two writes, split inside the image's base64
    s.write(&p, 0, &content[..100_000]).await.unwrap();
    s.write(&p, 100_000, &content[100_000..]).await.unwrap();
    assert_eq!(s.read(&p).await.unwrap(), content);
    let sql = common::sql(&db.url);
    let c = sql.get().await.unwrap();
    let blobs = c
        .query(
            "select kind, encoding, mime, width, height, stored, lines from blobs order by size",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(blobs.len(), 2);
    assert_eq!(blobs[0].get::<_, String>(0), "text");
    assert_eq!(blobs[0].get::<_, String>(1), "json-string");
    assert_eq!(blobs[0].get::<_, Option<i64>>(6), Some(1));
    assert_eq!(blobs[1].get::<_, String>(0), "media");
    assert_eq!(blobs[1].get::<_, String>(1), "base64-json");
    assert_eq!(blobs[1].get::<_, Option<String>>(2).as_deref(), Some("image/png"));
    assert_eq!(
        (blobs[1].get::<_, Option<i32>>(3), blobs[1].get::<_, Option<i32>>(4)),
        (Some(640), Some(480))
    );
    assert!(!blobs[1].get::<_, bool>(5), "staged, not yet in the bucket");
    let refs: i64 = c
        .query_one("select count(*) from blob_refs where owner = $1", &[&s.conversation])
        .await
        .unwrap()
        .get(0);
    assert_eq!(refs, 2);
    // the event holds references, not the bytes
    let data: Value = c
        .query_one(
            "select data from events where conversation_id = $1 and kind = 'cc.user' order by seq limit 1",
            &[&s.conversation],
        )
        .await
        .unwrap()
        .get(0);
    let pointer = &data["toolUseResult"]["file"]["base64"]["$blob"];
    assert_eq!(pointer["kind"], "media");
    assert_eq!(pointer["width"], 640);
    assert_eq!(pointer["bytes"], png.len());
    let stub_len: i32 = c
        .query_one("select max(octet_length(stub)) from cc_lines", &[])
        .await
        .unwrap()
        .get(0);
    assert!(stub_len < 1000, "the stub holds no blob content");
}

#[tokio::test]
async fn main_transcript_events() {
    let (db, r, s) = setup("cc_events").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    // lines written before the main path is known: stored without events, promoted when it is set
    let mut content = [prompt("p1", "hello"), internal(1), assistant("a1", "hi")].concat();
    s.write(&p, 0, &content).await.unwrap();
    assert_eq!(visible(&sql, conv).await, vec![]);
    set_main(&sql, conv, &p).await;
    let v = visible(&sql, conv).await;
    assert_eq!(
        v.iter().map(|e| (e.1.as_str(), e.2.as_deref())).collect::<Vec<_>>(),
        vec![("cc.user", Some("p1")), ("cc.assistant", Some("a1"))]
    );
    // setting it again promotes nothing twice
    set_main(&sql, conv, &p).await;
    set_main(&sql, conv, "projects/-workspace/other.jsonl").await;
    set_main(&sql, conv, &p).await;
    assert_eq!(visible(&sql, conv).await.len(), 2);
    // appends make events at once; the title record and the model
    let more = [
        assistant("a2", "second"),
        line(&json!({"type": "custom-title", "customTitle": "Q3 report"})),
    ]
    .concat();
    s.write(&p, content.len() as u64, &more).await.unwrap();
    content.extend(&more);
    let c = r.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.title.as_deref(), Some("Q3 report"));
    assert_eq!(c.model.as_deref(), Some("m-1"));
    assert_eq!(visible(&sql, conv).await.len(), 3);

    // tombstone removal: truncate at a line start, write the tail back: no new event, nothing retracted
    let cut = prompt("p1", "hello").len() + internal(1).len();
    let rest = content[cut..].to_vec();
    s.truncate(&p, cut as u64).await.unwrap();
    s.write(&p, cut as u64, &rest).await.unwrap();
    assert_eq!(s.read(&p).await.unwrap(), content);
    assert_eq!(flush(&sql, conv).await, 0);
    assert_eq!(visible(&sql, conv).await.len(), 3);
    assert!(!kinds(&sql, conv).await.contains(&"retract".to_owned()));

    // the same, removing a line (the real tombstone): that line's event is retracted at the flush
    let a1 = assistant("a1", "hi");
    let cut = prompt("p1", "hello").len() + internal(1).len();
    let rest = content[cut + a1.len()..].to_vec();
    s.truncate(&p, cut as u64).await.unwrap();
    s.write(&p, cut as u64, &rest).await.unwrap();
    content = [&content[..cut], &rest[..]].concat();
    assert_eq!(s.read(&p).await.unwrap(), content);
    assert_eq!(flush(&sql, conv).await, 1);
    let uuids: Vec<_> = visible(&sql, conv).await.into_iter().map(|e| e.2.unwrap()).collect();
    assert_eq!(uuids, vec!["p1", "a2"]);

    // a whole-file rewrite (O_TRUNC, then writes from 0 in chunks) keeps every event
    s.truncate(&p, 0).await.unwrap();
    for chunk in content.chunks(7) {
        let at = s.read(&p).await.unwrap().len() as u64;
        s.write(&p, at, chunk).await.unwrap();
    }
    assert_eq!(s.read(&p).await.unwrap(), content);
    assert_eq!(flush(&sql, conv).await, 0);
    assert_eq!(visible(&sql, conv).await.len(), 2);

    // a line rewritten with other content and the same uuid: the old event retracted, the new one replaces it
    let a2 = assistant("a2", "second");
    let pos = content.windows(a2.len()).position(|w| w == a2.as_slice()).unwrap();
    let a2b = assistant("a2", "second, edited");
    let tail = [&a2b[..], &content[pos + a2.len()..]].concat();
    s.truncate(&p, pos as u64).await.unwrap();
    s.write(&p, pos as u64, &tail).await.unwrap();
    // the rewritten line's old event leaves limbo and is retracted with the rewrite (it can no longer come back)
    assert_eq!(kinds(&sql, conv).await.last().map(String::as_str), Some("retract"));
    assert_eq!(flush(&sql, conv).await, 0);
    let v = visible(&sql, conv).await;
    assert_eq!(v.len(), 2);
    let replaces: Option<i64> = sql
        .get()
        .await
        .unwrap()
        .query_one(
            "select (meta->>'replaces')::bigint from events where conversation_id = $1 and seq = $2",
            &[&conv, &v[1].0],
        )
        .await
        .unwrap()
        .get(0);
    assert!(replaces.is_some(), "meta.replaces names the retracted line's event");

    // the main transcript cannot be unlinked
    assert_eq!(code(&s.unlink(&p).await.unwrap_err()), "protected");
}

#[tokio::test]
async fn compaction_and_set_aside() {
    let (db, _r, s) = setup("cc_compact").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    let old = [
        prompt("p1", "one"),
        assistant("a1", "two"),
        prompt("p2", "three"),
        assistant("a2", "four"),
    ]
    .concat();
    s.write(&p, 0, &old).await.unwrap();
    let before = visible(&sql, conv).await;
    assert_eq!(before.len(), 4);
    // compaction: a temp file with a boundary, a summary, two kept lines; renamed over the main transcript
    let tmp = format!("projects/-workspace/{}.jsonl.compact.tmp.ab12", common::cc::SID);
    let compacted = [
        line(&json!({"type": "system", "subtype": "compact_boundary", "uuid": "b1"})),
        line(&json!({"type": "user", "uuid": "s1", "isCompactSummary": true, "message": {"content": "summary"}})),
        prompt("p2", "three"),
        assistant("a2", "four"),
    ]
    .concat();
    s.write(&tmp, 0, &compacted).await.unwrap();
    assert_eq!(visible(&sql, conv).await.len(), 4, "the temp file makes no events");
    s.rename(&tmp, &p).await.unwrap();
    assert_eq!(s.read(&p).await.unwrap(), compacted);
    let after = visible(&sql, conv).await;
    let names: Vec<_> = after.iter().map(|e| e.2.clone().unwrap()).collect();
    assert_eq!(
        names,
        vec!["p2", "a2", "b1", "s1"],
        "kept lines keep their events, dropped ones are retracted at once (the boundary and the summary are events)"
    );
    assert_eq!(after[0].0, before[2].0);
    assert_eq!(after[1].0, before[3].0);

    // relocation with an occupied destination: the main transcript is set aside, a fresh one written
    let aside = format!("projects/-workspace/{}.jsonl.superseded-1793623733649", common::cc::SID);
    s.rename(&p, &aside).await.unwrap();
    s.write(&p, 0, &compacted).await.unwrap();
    let more = assistant("a3", "five");
    s.write(&p, compacted.len() as u64, &more).await.unwrap();
    let v = visible(&sql, conv).await;
    assert_eq!(
        v.len(),
        5,
        "no duplicate: the fresh file's lines take the moved lines' events"
    );
    assert_eq!(&v[..4], &after[..]);
    assert_eq!(s.read(&aside).await.unwrap(), compacted);
    // the set-aside file is an ordinary lines file: it can be removed
    s.unlink(&aside).await.unwrap();
    assert_eq!(visible(&sql, conv).await.len(), 5);

    // away and back onto the main path
    let away = format!("projects/-workspace/{}.jsonl.bak", common::cc::SID);
    s.rename(&p, &away).await.unwrap();
    s.write(
        &away,
        (compacted.len() + more.len()) as u64,
        &prompt("p9", "written while away"),
    )
    .await
    .unwrap();
    s.rename(&away, &p).await.unwrap();
    let v = visible(&sql, conv).await;
    assert_eq!(
        v.len(),
        6,
        "the lines it had keep their events; the one written away is classified on return"
    );
    assert_eq!(v[5].2.as_deref(), Some("p9"));
}

#[tokio::test]
async fn limbo_waits_for_a_quiet_file() {
    let (db, r, s) = setup("cc_limbo").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    let lines: Vec<Vec<u8>> = (0..6).map(|i| assistant(&format!("a{i}"), "x")).collect();
    let all = lines.concat();
    s.write(&p, 0, &all).await.unwrap();
    // a slow whole-file rewrite: each step 9 s after the last, with the sweeper running in between
    s.truncate(&p, 0).await.unwrap();
    let mut at = 0u64;
    for l in &lines {
        sql.get()
            .await
            .unwrap()
            .execute(
                "update cc_files set last_op_at = now() - interval '9 seconds' where scope = $1",
                &[&conv],
            )
            .await
            .unwrap();
        assert_eq!(magpie::ccfs::limbo::sweep(r.pool()).await.unwrap(), 0);
        s.write(&p, at, l).await.unwrap();
        at += l.len() as u64;
    }
    assert!(
        !kinds(&sql, conv).await.contains(&"retract".to_owned()),
        "a slow rewrite retracts nothing"
    );
    // a truncate nothing rewrites: retracted once the file has been quiet for 10 s
    let keep = lines[..4].concat().len() as u64;
    s.truncate(&p, keep).await.unwrap();
    assert_eq!(magpie::ccfs::limbo::sweep(r.pool()).await.unwrap(), 0);
    sql.get()
        .await
        .unwrap()
        .execute(
            "update cc_files set last_op_at = now() - interval '11 seconds' where scope = $1",
            &[&conv],
        )
        .await
        .unwrap();
    assert_eq!(magpie::ccfs::limbo::sweep(r.pool()).await.unwrap(), 2);
    assert_eq!(visible(&sql, conv).await.len(), 4);
    let ks = kinds(&sql, conv).await;
    assert_eq!(ks.iter().filter(|k| *k == "retract").count(), 1);
}

#[tokio::test]
async fn an_overwrite_inside_the_file() {
    let (db, _r, s) = setup("cc_overwrite").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    let a = assistant("a1", "aaaa");
    let b = assistant("a2", "bbbb");
    let c = assistant("a3", "cccc");
    let all = [a.clone(), b.clone(), c.clone()].concat();
    s.write(&p, 0, &all).await.unwrap();
    // overwrite inside the second line, same length: that line's event is retracted at once, a new one made
    let at = a.len() + b.windows(4).position(|w| w == b"bbbb").unwrap();
    s.write(&p, at as u64, b"BBBB").await.unwrap();
    let mut expect = all.clone();
    expect[at..at + 4].copy_from_slice(b"BBBB");
    assert_eq!(s.read(&p).await.unwrap(), expect);
    let v = visible(&sql, conv).await;
    let names: Vec<_> = v.iter().map(|e| e.2.clone().unwrap()).collect();
    assert_eq!(names, vec!["a1", "a3", "a2"]);
    // the later line kept its event and its place
    let more = assistant("a4", "dddd");
    s.write(&p, expect.len() as u64, &more).await.unwrap();
    expect.extend(&more);
    assert_eq!(s.read(&p).await.unwrap(), expect);
    // an overwrite that runs past the end
    let cut = expect.len() - 3;
    s.write(&p, cut as u64, b"xyz\nmore").await.unwrap();
    expect.truncate(cut);
    expect.extend(b"xyz\nmore");
    assert_eq!(s.read(&p).await.unwrap(), expect);
    let tree = s.tree().await.unwrap();
    assert_eq!(
        tree.files.iter().find(|e| e.path == p).unwrap().size,
        expect.len() as u64
    );
}

#[tokio::test]
async fn a_long_streamed_read() {
    let (_db, _r, s) = setup("cc_stream").await;
    let p = main_path();
    let mut content = Vec::new();
    // more lines than one read batch, with images
    let png = png_b64(5000);
    for i in 0..700 {
        let l = if i % 50 == 0 {
            image_result(&format!("i{i}"), &png)
        } else {
            internal(i)
        };
        content.extend(l);
    }
    for chunk in content.chunks(1 << 20) {
        let at = s.read(&p).await.map_or(0, |b| b.len()) as u64;
        s.write(&p, at, chunk).await.unwrap();
    }
    assert_eq!(s.read(&p).await.unwrap(), content);
    let (_, part) = s.read_range(&p, 1000, 50_000).await.unwrap();
    assert_eq!(part, &content[1000..51_000]);
    let _ = Duration::ZERO;
}

/// `(turn_id, has an event)` of each main-transcript line, in order.
async fn line_turns(pool: &aviary_pg::Pool, conv: Uuid) -> Vec<(Option<Uuid>, bool)> {
    let rows = pool
        .get()
        .await
        .unwrap()
        .query(
            "select l.turn_id, l.event_seq is not null from cc_lines l join cc_files f on f.id = l.file_id
              where f.scope = $1 and f.path = $2 order by l.ord",
            &[&conv, &main_path()],
        )
        .await
        .unwrap();
    rows.iter().map(|r| (r.get(0), r.get(1))).collect()
}

#[tokio::test]
async fn a_persons_prompt_echo_is_never_a_prompt_again() {
    let (db, _r, mut s) = setup("cc_echo").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    // a person's turn: Claude Code writes the prompt (the echo of user.sent: no event) and the reply
    let t1 = person_turn(&sql, &mut s).await;
    let content = [prompt("p1", "hello"), assistant("a1", "hi")].concat();
    s.write(&p, 0, &content).await.unwrap();
    let only_reply = vec![("cc.assistant".to_owned(), Some("a1".to_owned()))];
    let names = |v: Vec<(i64, String, Option<String>)>| v.into_iter().map(|e| (e.1, e.2)).collect::<Vec<_>>();
    assert_eq!(names(visible(&sql, conv).await), only_reply);

    // a whole-file rewrite (O_TRUNC, the same bytes again in chunks): the echo stays an echo
    s.truncate(&p, 0).await.unwrap();
    for (i, chunk) in content.chunks(13).enumerate() {
        s.write(&p, (i * 13) as u64, chunk).await.unwrap();
    }
    assert_eq!(s.read(&p).await.unwrap(), content);
    assert_eq!(flush_turn(&sql, conv, Some(t1)).await, 0);
    assert_eq!(names(visible(&sql, conv).await), only_reply);

    // the next turn's echo is pending while an old prompt is written again: the old one is not taken as its echo
    let t2 = person_turn(&sql, &mut s).await;
    s.truncate(&p, 0).await.unwrap();
    s.write(&p, 0, &content).await.unwrap();
    assert_eq!(names(visible(&sql, conv).await), only_reply);
    // the rewritten lines keep the turn they were first credited to (6.3)
    assert_eq!(line_turns(&sql, conv).await, vec![(Some(t1), false), (Some(t1), true)]);
    let p2 = prompt("p2", "and then");
    let all = [content.clone(), p2.clone()].concat();
    s.write(&p, content.len() as u64, &p2).await.unwrap();
    assert_eq!(names(visible(&sql, conv).await), only_reply, "p2 is t2's echo");
    let matched: bool = sql
        .get()
        .await
        .unwrap()
        .query_one("select echo_matched from turns where id = $1", &[&t2])
        .await
        .unwrap()
        .get(0);
    assert!(matched);

    // a tombstone-style truncate before the echo, the rest written back
    let cut = prompt("p1", "hello").len();
    s.truncate(&p, cut as u64).await.unwrap();
    s.write(&p, cut as u64, &all[cut..]).await.unwrap();
    assert_eq!(flush_turn(&sql, conv, Some(t2)).await, 0);
    assert_eq!(names(visible(&sql, conv).await), only_reply);

    // a compaction that keeps both prompts: the temp file renamed over the transcript classifies neither again
    let tmp = format!("projects/-workspace/{}.jsonl.compact.tmp.e1", common::cc::SID);
    // (a system line Claude Code writes that makes no event: the boundary itself is an event, compaction_and_set_aside)
    let compacted = [
        line(&json!({"type": "system", "subtype": "turn_duration"})),
        all.clone(),
    ]
    .concat();
    s.write(&tmp, 0, &compacted).await.unwrap();
    s.rename(&tmp, &p).await.unwrap();
    assert_eq!(s.read(&p).await.unwrap(), compacted);
    assert_eq!(names(visible(&sql, conv).await), only_reply);

    // a set-aside and a fresh file with the same content, and away and back
    let aside = format!("projects/-workspace/{}.jsonl.superseded-1", common::cc::SID);
    s.rename(&p, &aside).await.unwrap();
    s.write(&p, 0, &compacted).await.unwrap();
    s.unlink(&aside).await.unwrap();
    let away = format!("projects/-workspace/{}.jsonl.bak", common::cc::SID);
    s.rename(&p, &away).await.unwrap();
    s.rename(&away, &p).await.unwrap();
    assert_eq!(names(visible(&sql, conv).await), only_reply);
    let prompts: i64 = sql
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from events where conversation_id = $1 and kind = 'cc.user'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(prompts, 0, "no cc.user prompt event was ever made for an echo");
}

#[tokio::test]
async fn an_echo_rewritten_after_its_limbo_flushed_is_still_no_prompt() {
    let (db, _r, mut s) = setup("cc_echo_flushed").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    let names = |v: Vec<(i64, String, Option<String>)>| v.into_iter().map(|e| (e.1, e.2)).collect::<Vec<_>>();
    // turn 1 writes its echo and a reply, then truncates; the turn ends (limbo flushed) before the rewrite lands
    let t1 = person_turn(&sql, &mut s).await;
    let content = [prompt("p1", "hello"), assistant("a1", "hi")].concat();
    s.write(&p, 0, &content).await.unwrap();
    s.truncate(&p, 0).await.unwrap();
    assert_eq!(flush_turn(&sql, conv, Some(t1)).await, 1, "a1 is retracted");
    // turn 2 (a person's) rewrites the old lines, then appends its own prompt's echo
    let t2 = person_turn(&sql, &mut s).await;
    s.write(&p, 0, &content).await.unwrap();
    let p2 = prompt("p2", "and then");
    s.write(&p, content.len() as u64, &p2).await.unwrap();
    assert_eq!(
        names(visible(&sql, conv).await),
        vec![("cc.assistant".to_owned(), Some("a1".to_owned()))],
        "p1 is internal again and p2 is turn 2's echo"
    );
    let conn = sql.get().await.unwrap();
    let matched: bool = conn
        .query_one("select echo_matched from turns where id = $1", &[&t2])
        .await
        .unwrap()
        .get(0);
    assert!(matched, "p2 matched turn 2's echo");
    let prompts: i64 = conn
        .query_one(
            "select count(*) from events where conversation_id = $1 and kind = 'cc.user'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(prompts, 0);

    // again one turn later, with turn 2's echo among the old lines
    s.truncate(&p, 0).await.unwrap();
    assert_eq!(flush_turn(&sql, conv, Some(t2)).await, 1);
    let t3 = person_turn(&sql, &mut s).await;
    let all = [content.clone(), p2.clone()].concat();
    s.write(&p, 0, &all).await.unwrap();
    let p3 = prompt("p3", "third");
    s.write(&p, all.len() as u64, &p3).await.unwrap();
    assert_eq!(names(visible(&sql, conv).await).len(), 1, "only the reply, once");
    let matched: bool = conn
        .query_one("select echo_matched from turns where id = $1", &[&t3])
        .await
        .unwrap()
        .get(0);
    assert!(matched, "p3 matched turn 3's echo");

    // a prompt typed in the terminal (an event) retracted and written again in a person turn is that prompt again,
    // not the turn's echo
    let t4 = person_turn(&sql, &mut s).await;
    flush_turn(&sql, conv, Some(t3)).await;
    let cut = s.read(&p).await.unwrap().len();
    // turn 4's echo is matched already, so its next prompt line is a cc.user prompt event
    let typed = prompt("q1", "typed in the terminal");
    conn.execute("update turns set echo_matched = true where id = $1", &[&t4])
        .await
        .unwrap();
    s.write(&p, cut as u64, &typed).await.unwrap();
    let before = names(visible(&sql, conv).await);
    assert!(before.contains(&("cc.user".to_owned(), Some("q1".to_owned()))));
    s.truncate(&p, cut as u64).await.unwrap();
    flush_turn(&sql, conv, Some(t4)).await;
    let t5 = person_turn(&sql, &mut s).await;
    s.write(&p, cut as u64, &typed).await.unwrap();
    let p5 = prompt("p5", "fifth");
    s.write(&p, (cut + typed.len()) as u64, &p5).await.unwrap();
    assert_eq!(
        names(visible(&sql, conv).await),
        before,
        "q1 is back as itself, p5 is the echo"
    );
    let matched: bool = conn
        .query_one("select echo_matched from turns where id = $1", &[&t5])
        .await
        .unwrap()
        .get(0);
    assert!(matched);
}

#[tokio::test]
async fn an_echo_written_again_with_other_bytes_is_still_no_prompt() {
    let (db, _r, mut s) = setup("cc_echo_uuid").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    let names = |v: Vec<(i64, String, Option<String>)>| v.into_iter().map(|e| (e.1, e.2)).collect::<Vec<_>>();
    let prompt_events = async |sql: &aviary_pg::Pool| -> i64 {
        sql.get()
            .await
            .unwrap()
            .query_one(
                "select count(*) from events where conversation_id = $1 and kind = 'cc.user'",
                &[&conv],
            )
            .await
            .unwrap()
            .get(0)
    };
    // turn 1: its echo and a reply
    let t1 = person_turn(&sql, &mut s).await;
    let content = [prompt("p1", "hello"), assistant("a1", "hi")].concat();
    s.write(&p, 0, &content).await.unwrap();
    flush_turn(&sql, conv, Some(t1)).await;
    let only_reply = vec![("cc.assistant".to_owned(), Some("a1".to_owned()))];
    assert_eq!(names(visible(&sql, conv).await), only_reply);
    // the same message with other bytes (another sessionId, the fields in another order): never retracted, so no
    // line with its uuid ever had an event
    let p1_again = line(
        &json!({"sessionId": "another", "message": {"content": "hello", "role": "user"},
        "type": "user", "uuid": "p1"}),
    );
    assert_ne!(p1_again, prompt("p1", "hello"));
    // turn 2 (a person's, its echo pending) writes it, then its own echo: p1 does not take turn 2's echo
    let t2 = person_turn(&sql, &mut s).await;
    let mut at = content.len() as u64;
    s.write(&p, at, &p1_again).await.unwrap();
    at += p1_again.len() as u64;
    let p2 = prompt("p2", "and then");
    s.write(&p, at, &p2).await.unwrap();
    at += p2.len() as u64;
    assert_eq!(
        names(visible(&sql, conv).await),
        only_reply,
        "p1 is internal, p2 is turn 2's echo"
    );
    let matched: bool = sql
        .get()
        .await
        .unwrap()
        .query_one("select echo_matched from turns where id = $1", &[&t2])
        .await
        .unwrap()
        .get(0);
    assert!(matched, "p2 matched turn 2's echo");
    assert_eq!(prompt_events(&sql).await, 0);
    // with no echo pending (turn 2's is matched), p2 with other bytes is no prompt event either
    let p2_again = line(
        &json!({"uuid": "p2", "type": "user", "message": {"role": "user", "content": "and then"},
        "cwd": "/workspace"}),
    );
    s.write(&p, at, &p2_again).await.unwrap();
    assert_eq!(names(visible(&sql, conv).await), only_reply);
    assert_eq!(
        prompt_events(&sql).await,
        0,
        "no cc.user prompt event was made for an echo"
    );
}

#[tokio::test]
async fn a_uuid_less_prompt_written_again_is_itself_and_one_with_the_same_text_is_the_echo() {
    let (db, _r, mut s) = setup("cc_uuidless").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    let users = async |sql: &aviary_pg::Pool| -> Vec<i64> {
        visible(sql, conv)
            .await
            .into_iter()
            .filter(|e| e.1 == "cc.user")
            .map(|e| e.0)
            .collect()
    };
    // a uuid-less user line with no echo pending: a prompt event of its own
    let u1 = line(&json!({"type": "user", "message": {"role": "user", "content": "same"}, "timestamp": "t1"}));
    s.write(&p, 0, &u1).await.unwrap();
    let first = users(&sql).await;
    assert_eq!(first.len(), 1, "u1 is a cc.user event");
    // a truncate whose limbo is flushed: retracted
    s.truncate(&p, 0).await.unwrap();
    flush_turn(&sql, conv, None).await;
    assert!(users(&sql).await.is_empty(), "u1 retracted");
    // a person's turn (its echo pending) writes u1 back (the same line again: an event, not the turn's echo), then a
    // prompt with the same text and other data (its own timestamp): that one is the turn's echo
    let t = person_turn(&sql, &mut s).await;
    s.write(&p, 0, &u1).await.unwrap();
    assert_eq!(users(&sql).await.len(), 1, "u1 written again is itself");
    let e2 = line(&json!({"type": "user", "message": {"role": "user", "content": "same"}, "timestamp": "t2"}));
    s.write(&p, u1.len() as u64, &e2).await.unwrap();
    assert_eq!(users(&sql).await.len(), 1, "e2 is no prompt event");
    let matched: bool = sql
        .get()
        .await
        .unwrap()
        .query_one("select echo_matched from turns where id = $1", &[&t])
        .await
        .unwrap()
        .get(0);
    assert!(matched, "e2 matched the turn's echo");
    // the lookup is by index: the retracted line is recorded by its data's key
    let keys: i64 = sql
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from cc_retracted where conversation_id = $1 and uuid like 'data:%'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(keys, 1);
}

#[tokio::test]
async fn the_end_of_a_turn_flushes_only_its_own_limbo() {
    let (db, _r, mut s) = setup("cc_flush_turn").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    let t1 = person_turn(&sql, &mut s).await;
    let lines: Vec<Vec<u8>> = (0..4).map(|i| assistant(&format!("a{i}"), "x")).collect();
    s.write(&p, 0, &lines.concat()).await.unwrap();
    // turn 1 cuts the last line; turn 2 starts before anything flushed and cuts another
    let keep = lines[..3].concat().len() as u64;
    s.truncate(&p, keep).await.unwrap();
    let t2 = person_turn(&sql, &mut s).await;
    s.truncate(&p, lines[..2].concat().len() as u64).await.unwrap();
    // turn 1's end retracts only what turn 1's truncate removed
    assert_eq!(flush_turn(&sql, conv, Some(t1)).await, 1);
    assert_eq!(visible(&sql, conv).await.len(), 3);
    // turn 2's line can still come back
    s.write(&p, lines[..2].concat().len() as u64, &lines[2]).await.unwrap();
    assert_eq!(flush_turn(&sql, conv, Some(t2)).await, 0);
    assert_eq!(visible(&sql, conv).await.len(), 3);
}

#[tokio::test]
async fn an_append_expects_the_files_size() {
    let (_db, _r, s) = setup("cc_append").await;
    let p = main_path();
    let a = internal(1);
    s.write(&p, 0, &a).await.unwrap();
    let append = |off: u64| magpie_api::sandbox::CcWriteQuery {
        append: true,
        ..s.wq(&p, off, Uuid::new_v4())
    };
    // a stale append (its offset is no longer the size) with other bytes: refused, nothing written
    let b = internal(2);
    s.calls().cc_write(&append(a.len() as u64), b.clone()).await.unwrap();
    let both = [a.clone(), b.clone()].concat();
    let e = s
        .calls()
        .cc_write(&append(a.len() as u64), internal(3))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "offset_mismatch");
    assert_eq!(s.read(&p).await.unwrap(), both);
    // the same append resent under a new opId: rule 0 answers it first
    let r = s.calls().cc_write(&append(a.len() as u64), b.clone()).await.unwrap();
    assert_eq!(r.size, both.len() as u64);
    // past the end is still offset_beyond_eof; a plain write inside the file still overwrites
    let e = s.calls().cc_write(&append(1000), internal(4)).await.unwrap_err();
    assert_eq!(code(&e), "offset_beyond_eof");
    s.write(&p, 2, b" ").await.unwrap();
}

#[tokio::test]
async fn a_lease_never_records_hermits_answer_over_a_newer_feed_record() {
    use aviary_fakes::fault::{FaultKind, Hold};
    let db = common::fresh_db("cc_lease_race").await;
    let hermit = aviary_fakes::hermit::FakeHermit::unguarded(aviary_fakes::hermit::HermitConfig::default());
    let running = hermit.serve().await.unwrap();
    let t = common::Tokens::new();
    let r = common::replica_with(&db.url, &t, &running.url(), common::idle()).await;
    let conv = conversation(&r, ALICE).await;
    let hc = hermit_api::client::Client::new(common::http(), running.url()).bearer("any");
    hc.put_sandbox(conv, &hermit_api::sandbox::PutSandboxRequest { person: ALICE.into() })
        .await
        .unwrap();
    let a = hermit.live(conv).unwrap();
    // hermit answers `a`, but before the grant locks the row the feed records `b` (a migration)
    let hold = Hold::new();
    hermit.faults().times("get_sandbox", FaultKind::Hold(hold.clone()), 1);
    let mut sa = Sandbox::new(&r, conv, a.id, ALICE);
    let asking = tokio::spawn(async move { sa.take_lease().await });
    assert!(hold.wait_held(1, Duration::from_secs(5)).await);
    let b = Uuid::new_v4();
    hermit_names(r.pool(), conv, Some(b), "ready").await;
    hold.release();
    let got = asking.await.unwrap();
    assert_eq!(code(&got.unwrap_err()), "not_current_sandbox");
    let row = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select hermit_sandbox, lease_sandbox from conversations where id = $1",
            &[&conv],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<Uuid>>(0), Some(b), "the feed's newer record stays");
    assert_eq!(row.get::<_, Option<Uuid>>(1), None, "nobody got the lease");
    // the sandbox the feed names takes it without asking hermit
    let mut sb = Sandbox::new(&r, conv, b, ALICE);
    sb.take_lease().await.unwrap();
}

#[tokio::test]
async fn a_large_transcript_reads_and_rewrites_in_bounded_time() {
    let (db, _r, s) = setup("cc_large").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    set_main(&sql, s.conversation, &p).await;
    // 30,000 lines (Claude Code's transcripts reach tens of thousands), every 100th an image, every 30th a reply
    let png = png_b64(3000);
    let mut content = Vec::new();
    for i in 0..30_000u32 {
        let l = if i % 100 == 0 {
            image_result(&format!("i{i}"), &png)
        } else if i % 30 == 1 {
            assistant(&format!("a{i}"), &format!("reply {i}"))
        } else {
            internal(i)
        };
        content.extend(l);
    }
    for (i, chunk) in content.chunks(1 << 20).enumerate() {
        s.write(&p, (i << 20) as u64, chunk).await.unwrap();
    }
    let events = visible(&sql, s.conversation).await.len();
    // a whole read: batches seek with the index (a read that fetched every row to the end per batch took seconds)
    let t0 = std::time::Instant::now();
    assert_eq!(s.read(&p).await.unwrap(), content);
    let read = t0.elapsed();
    // Claude Code's whole-file rewrite: O_TRUNC (one statement), then the same bytes in 1 MiB writes
    let t0 = std::time::Instant::now();
    s.truncate(&p, 0).await.unwrap();
    let truncate = t0.elapsed();
    for (i, chunk) in content.chunks(1 << 20).enumerate() {
        s.write(&p, (i << 20) as u64, chunk).await.unwrap();
    }
    let rewrite = t0.elapsed();
    assert_eq!(s.read(&p).await.unwrap(), content);
    assert_eq!(flush(&sql, s.conversation).await, 0);
    assert_eq!(visible(&sql, s.conversation).await.len(), events);
    eprintln!(
        "{} lines, {} bytes: read {read:?}, truncate(0) {truncate:?}, truncate and rewrite {rewrite:?}",
        30_000,
        content.len()
    );
    assert!(read < Duration::from_secs(10), "read {read:?}");
    assert!(truncate < Duration::from_secs(3), "truncate {truncate:?}");
    assert!(rewrite < Duration::from_secs(60), "rewrite {rewrite:?}");
}

#[tokio::test]
async fn renames_between_kinds_keep_the_main_transcripts_events_right() {
    let (db, _r, s) = setup("cc_rename_kinds").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    set_main(&sql, conv, &p).await;
    let names = |v: &[(i64, String, Option<String>)]| v.iter().map(|e| e.2.clone().unwrap()).collect::<Vec<_>>();
    let content = [assistant("a1", "one"), assistant("a2", "two")].concat();
    s.write(&p, 0, &content).await.unwrap();
    let before = visible(&sql, conv).await;
    assert_eq!(names(&before), ["a1", "a2"]);

    // the main transcript renamed to a byte-file name (pages): its lines still enter moved limbo, so the same bytes
    // written again at the main path make no new events
    let old = format!("projects/-workspace/{}.json.old", common::cc::SID);
    s.rename(&p, &old).await.unwrap();
    assert_eq!(s.read(&old).await.unwrap(), content);
    s.write(&p, 0, &content).await.unwrap();
    assert_eq!(visible(&sql, conv).await, before, "the same events, no copies");

    // a byte file renamed over the main transcript: matched like a rename-over (a1 keeps its event), the dropped
    // line's event (a2) retracted at once, the new line (a9) classified
    let tmp = format!("projects/-workspace/{}.tmp", common::cc::SID);
    let next = [assistant("a1", "one"), assistant("a9", "nine")].concat();
    s.write(&tmp, 0, &next).await.unwrap();
    s.rename(&tmp, &p).await.unwrap();
    assert_eq!(s.read(&p).await.unwrap(), next);
    let after = visible(&sql, conv).await;
    assert_eq!(names(&after), ["a1", "a9"]);
    assert_eq!(after[0].0, before[0].0, "a1 kept its event");
}

#[tokio::test]
async fn the_main_transcript_is_the_provider_sessions_path() {
    let (db, _r, s) = setup("cc_main_path").await;
    let sql = common::sql(&db.url);
    let p = main_path();
    let conv = s.conversation;
    // a path other than projects/-workspace/<providerSessionId>.jsonl is refused (a subagent's transcript, another
    // session's), and nothing changes
    let set = |path: String| {
        let sql = sql.clone();
        async move {
            let path: &str = &path;
            aviary_pg::tx!(&sql, |t| -> magpie::error::Result<()> {
                t.execute(
                    "update conversations set provider_session_id = $2 where id = $1",
                    &[&conv, &common::cc::SID],
                )
                .await?;
                magpie::ccfs::lines::set_main_transcript(t, conv, path).await
            })
            .await
        }
    };
    for bad in [
        format!("projects/-workspace/{}/subagents/agent-1.jsonl", common::cc::SID),
        "projects/-workspace/22222222-2222-4333-8444-555555555555.jsonl".to_owned(),
        format!("projects/-other/{}.jsonl", common::cc::SID),
    ] {
        let e = set(bad.clone()).await.unwrap_err();
        assert!(e.is(&magpie_api::errors::ErrorCode::BadRequest), "{bad}: {e}");
    }
    let main: Option<String> = sql
        .get()
        .await
        .unwrap()
        .query_one("select main_transcript from conversations where id = $1", &[&conv])
        .await
        .unwrap()
        .get(0);
    assert_eq!(main, None);
    set(p.clone()).await.unwrap();

    // a changed main path (a new provider session): the old file keeps its events, is no longer protected, and the
    // same lines at the new path make no new events
    let content = [assistant("a1", "one"), assistant("a2", "two")].concat();
    s.write(&p, 0, &content).await.unwrap();
    let before = visible(&sql, conv).await;
    assert_eq!(before.len(), 2);
    let other = "projects/-workspace/33333333-2222-4333-8444-555555555555.jsonl".to_owned();
    set_main(&sql, conv, &other).await;
    s.write(&other, 0, &content).await.unwrap();
    assert_eq!(visible(&sql, conv).await, before);
    s.unlink(&p).await.unwrap();
    s.write(&other, content.len() as u64, &assistant("a3", "three"))
        .await
        .unwrap();
    assert_eq!(visible(&sql, conv).await.len(), 3);
    let e = s.unlink(&other).await.unwrap_err();
    assert_eq!(code(&e), "protected");
}
