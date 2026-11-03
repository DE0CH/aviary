//! The importer end to end against fixtures made in the test (cutover 7.12 items 1, 2, 4 and 9 for the sync, and
//! the ownership rule of 6.2): mapping, byte-exact verification, idempotent re-runs, appends, rewrites, relocation,
//! removal, the display fixes, failed turns, auto-memory per conversation, id conflicts, vanished rows, the forced
//! release and reset.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use common::{World, assistant, user, user_image};
use magpie_import::{control, pass, plan, verify};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";
const BOB: &str = "bob@example.com";

fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend((0..6000u32).map(|i| u8::try_from((i * 7 + 3) % 251).unwrap()));
    b
}

async fn verify_all(w: &World) {
    for id in verify::targets(&w.imp, &[]).await.unwrap() {
        let v = verify::verify(&w.imp, id).await.unwrap();
        assert!(v.ok(), "{id} did not verify: {v:?}");
        assert!(v.reingested.is_empty(), "{id} needed a re-ingest: {v:?}");
        assert!(
            v.notes.iter().all(|n| n.starts_with("no workspace")),
            "{id}: the event counts are not the classifier's: {:?}",
            v.notes
        );
    }
}

#[tokio::test]
async fn a_run_maps_rows_and_copies_every_file_byte_exact() {
    let w = World::new("run").await;
    let ka = w.person(ALICE).await;
    let kb = w.person(BOB).await;
    let (u1, p1) = (Uuid::new_v4(), Uuid::new_v4());
    let u2 = Uuid::new_v4(); // disk-discovered: session id = provider session id
    let u3 = Uuid::new_v4(); // never answered: no transcript
    let (u4, p4) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, name_source, jsonl_path,
                                       created_at, updated_at, model)
           values ('{u1}', '{p1}', 'Report', 'user', '/var/lib/cloudcli/home/{ka}/.claude/projects/-workspace/{p1}.jsonl',
                   '2026-10-22 06:22:36', '2026-10-22T07:00:00.000Z', 'claude-test');
         insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, \"isArchived\", deleted_at,
                                       created_at, updated_at)
           values ('{u2}', '{u2}', 'Old', 1, '2026-10-23 00:00:00', '1970-01-01T00:00:00.000Z', '2026-10-21 10:00:00');
         insert into cc_{ka}.sessions (session_id, provider_session_id, created_at, updated_at)
           values ('{u3}', '{u3}', '2026-10-24 09:00:00', '2026-10-24 09:00:00');
         insert into cc_{kb}.sessions (session_id, provider_session_id, custom_name, created_at, updated_at)
           values ('{u4}', '{p4}', 'Bob''s', '2026-10-25 01:00:00', '2026-10-25 01:00:00');
         insert into cc_{ka}.jarvis_failed_turns (session_id, user_text, user_at, error, error_at)
           values ('{u1}', 'are you there?', '2026-10-22 06:24:30', '暫時連接不到這個對話的電腦，請稍後再試。',
                   '2026-10-22T06:24:31.000Z');
         insert into cc_{ka}.jarvis_failed_turns (session_id, user_text, user_at, error, error_at)
           values ('{u1}', null, null, 'something nobody mapped', '2026-10-22T09:00:00.000Z');
         insert into cc_{ka}.jarvis_failed_turns (session_id, user_text, user_at, error, error_at)
           values ('{u3}', 'hello?', '2026-10-24 09:00:01', '所有電腦正在使用中，請過幾分鐘再試。', '2026-10-24 09:00:05');"
    ))
    .await;
    // conversation 1: two exchanges around a failed turn, an image, a torn tail; a tool result and a subagent
    let mut t1 = Vec::new();
    t1.extend(user("a1", "2026-10-22T06:22:40.000Z", "write the report"));
    t1.extend(assistant("a2", "2026-10-22T06:23:00.000Z", "here it is"));
    t1.extend(user_image("a3", "2026-10-22T06:25:00.000Z", &png()));
    t1.extend(assistant("a4", "2026-10-22T06:25:10.000Z", "a picture"));
    t1.extend(b"{\"type\":\"assistant\",\"uuid\":\"a5\",\"mess");
    w.put(&ka, &format!("projects/-workspace/{p1}.jsonl"), &t1);
    w.put(
        &ka,
        &format!("projects/-workspace/{p1}/tool-results/toolu_1.txt"),
        b"tool output\n",
    );
    let mut sub = user("s1", "2026-10-22T06:23:01.000Z", "sub task");
    sub.extend(assistant("s2", "2026-10-22T06:23:02.000Z", "sub done"));
    w.put(&ka, &format!("projects/-workspace/{p1}/subagents/agent-x.jsonl"), &sub);
    // conversation 2: a 1.1 MB line; its row's created_at is the 1970 placeholder
    let big = "x".repeat(1_100_000);
    let mut t2 = user("b1", "2026-10-20T08:00:00.000Z", "a long one");
    t2.extend(assistant("b2", "2026-10-20T08:00:05.000Z", &big));
    w.put(&ka, &format!("projects/-workspace/{u2}.jsonl"), &t2);
    // Alice's auto-memory: copied into each of her conversations
    w.put(
        &ka,
        "projects/-workspace/memory/MEMORY.md",
        b"- prefers short answers\n",
    );
    w.put(
        &kb,
        &format!("projects/-workspace/{p4}.jsonl"),
        &user("c1", "2026-10-25T01:00:01.000Z", "hi"),
    );

    let p = plan::plan(&w.imp).await.unwrap();
    assert!(p.blockers.is_empty(), "{:?}", p.blockers);
    assert_eq!(p.conversations, 4);
    assert_eq!(p.memory.len(), 1);
    assert_eq!(p.memory[0].conversations, 3);
    assert!(p.largest_lines[0].bytes > 1_100_000);

    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.created, 4);
    verify_all(&w).await;

    let c1 = w.conv(u1).await;
    assert_eq!(c1["migrating"], true);
    assert_eq!(c1["title"], "Report");
    assert_eq!(c1["nameSource"], "user");
    assert_eq!(c1["origin"]["kind"], "import");
    assert_eq!(c1["psid"], p1.to_string());
    assert_eq!(c1["main"], format!("projects/-workspace/{p1}.jsonl"));
    assert_eq!(c1["created"], 1_792_650_156);
    // the failed turn with text sits between the first exchange and the image; the one without text after all
    assert_eq!(
        w.kinds(u1).await,
        [
            "conversation.created",
            "cc.user",
            "cc.assistant",
            "user.sent",
            "turn.ended",
            "cc.user",
            "cc.assistant",
            "turn.ended"
        ]
    );
    let c2 = w.conv(u2).await;
    assert_eq!(c2["deleted"], true);
    assert_eq!(
        c2["created"], 1_792_483_200,
        "the placeholder becomes the first record's time"
    );
    assert_eq!(w.kinds(u3).await, ["conversation.created", "user.sent", "turn.ended"]);
    let errors: Vec<(String, String)> = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query(
            "select error_code, coalesce(error_detail, '') from turns where conversation_id = $1 order by created_at",
            &[&u1],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(errors[0].0, "sandbox.unreachable");
    assert_eq!(
        errors[1],
        ("sandbox.failed".to_owned(), "something nobody mapped".to_owned())
    );
    // memory in each of Alice's conversations, not in Bob's
    for id in [u1, u2, u3] {
        assert!(
            w.versions(id)
                .await
                .iter()
                .any(|(p, _)| p == "projects/-workspace/memory/MEMORY.md"),
            "{id} has no memory copy"
        );
    }
    assert!(!w.versions(u4).await.iter().any(|(p, _)| p.contains("memory")));
    let tail = verify::reconstruct(&w.imp, u1, ALICE, &format!("projects/-workspace/{p1}.jsonl"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tail.1, t1.len() as u64, "the torn tail is stored");

    // a second pass changes nothing
    let v1 = w.versions(u1).await;
    let k1 = w.kinds(u1).await;
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert_eq!((r.created, r.changed, r.errors.len()), (0, 0, 0));
    assert_eq!(w.versions(u1).await, v1);
    assert_eq!(w.kinds(u1).await, k1);

    let s = control::status(&w.imp).await.unwrap();
    assert_eq!(s.conversations.imported, 4);
    assert_eq!(s.conversations.legacy, 4);
    assert_eq!(s.phase, "sync");
}

#[tokio::test]
async fn appends_rewrites_relocations_and_removals_converge() {
    let w = World::new("converge").await;
    let k = w.person(ALICE).await;
    let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{k}.sessions (session_id, provider_session_id, created_at, updated_at)
           values ('{u}', '{p}', '2026-10-22 06:00:00', '2026-10-22 06:00:00')"
    ))
    .await;
    let main = format!("projects/-workspace/{p}.jsonl");
    let mut t = user("l1", "2026-10-22T06:00:01.000Z", "one");
    t.extend(assistant("l2", "2026-10-22T06:00:02.000Z", "two"));
    w.put(&k, &main, &t);
    w.put(&k, &format!("projects/-workspace/{p}/tool-results/a.txt"), b"alpha\n");
    w.put(
        &k,
        &format!("projects/-workspace/{p}/subagents/agent-1.jsonl"),
        &user("x", "2026-10-22T06:00:01.500Z", "s"),
    );
    pass::pass(&w.imp, None).await.unwrap();
    verify_all(&w).await;

    // an append with a half-written line, then its completion
    let l3 = user("l3", "2026-10-22T06:01:00.000Z", "three");
    w.append(&k, &main, &l3[..10]);
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert_eq!(r.changed, 1);
    verify_all(&w).await;
    w.append(&k, &main, &l3[10..]);
    pass::pass(&w.imp, None).await.unwrap();
    verify_all(&w).await;
    assert_eq!(
        w.kinds(u).await,
        ["conversation.created", "cc.user", "cc.assistant", "cc.user"]
    );

    // a compaction: the file rewritten through a temporary file (a new inode), the second line replaced
    let mut c = user("l1", "2026-10-22T06:00:01.000Z", "one");
    c.extend(user("sum", "2026-10-22T06:02:00.000Z", "summary of the rest"));
    w.rename_over(&k, &main, &c);
    pass::pass(&w.imp, None).await.unwrap();
    verify_all(&w).await;

    // a relocation (the same bytes under another name) and a removal
    let from = w.claude(&k).join(format!("projects/-workspace/{p}/tool-results/a.txt"));
    std::fs::rename(&from, from.with_file_name("b.txt")).unwrap();
    std::fs::remove_file(
        w.claude(&k)
            .join(format!("projects/-workspace/{p}/subagents/agent-1.jsonl")),
    )
    .unwrap();
    pass::pass(&w.imp, None).await.unwrap();
    verify_all(&w).await;
    let paths: Vec<String> = w.versions(u).await.into_iter().map(|(p, _)| p).collect();
    assert!(paths.iter().any(|x| x.ends_with("tool-results/b.txt")));
    assert!(!paths.iter().any(|x| x.ends_with("tool-results/a.txt")));
    assert!(!paths.iter().any(|x| x.contains("subagents/agent-1.jsonl")));

    // a file rewritten back to bytes it held before is written again (not replayed)
    w.put(&k, &format!("projects/-workspace/{p}/tool-results/b.txt"), b"beta\n");
    pass::pass(&w.imp, None).await.unwrap();
    verify_all(&w).await;
    w.put(&k, &format!("projects/-workspace/{p}/tool-results/b.txt"), b"alpha\n");
    pass::pass(&w.imp, None).await.unwrap();
    verify_all(&w).await;
}

#[tokio::test]
async fn a_conversation_that_is_not_migrating_is_never_written() {
    let w = World::new("owned").await;
    let k = w.person(ALICE).await;
    let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{k}.sessions (session_id, provider_session_id, created_at, updated_at)
           values ('{u}', '{p}', '2026-10-22 06:00:00', '2026-10-22 06:00:00')"
    ))
    .await;
    let main = format!("projects/-workspace/{p}.jsonl");
    w.put(&k, &main, &user("l1", "2026-10-22T06:00:01.000Z", "one"));
    pass::pass(&w.imp, None).await.unwrap();
    // released behind the importer's back, between its read and its write
    w.magpie()
        .get()
        .await
        .unwrap()
        .execute("update conversations set migrating = false where id = $1", &[&u])
        .await
        .unwrap();
    let v = w.versions(u).await;
    let n = w.kinds(u).await.len();
    w.append(&k, &main, &assistant("l2", "2026-10-22T06:00:02.000Z", "two"));
    w.cloudcli(&format!(
        "update cc_{k}.sessions set custom_name = 'renamed' where session_id = '{u}'"
    ))
    .await;
    w.cloudcli(&format!(
        "insert into cc_{k}.jarvis_failed_turns (session_id, user_text, user_at, error, error_at)
           values ('{u}', 'x', '2026-10-22 06:00:03', 'e', '2026-10-22 06:00:04')"
    ))
    .await;
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(w.versions(u).await, v);
    assert_eq!(w.kinds(u).await.len(), n);
    assert_ne!(w.conv(u).await["title"], "renamed");
}

#[tokio::test]
async fn imported_prompts_get_the_display_fixes() {
    let w = World::new("display").await;
    let k = w.person(ALICE).await;
    let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{k}.sessions (session_id, provider_session_id, created_at, updated_at)
           values ('{u}', '{p}', '2026-10-22 06:00:00', '2026-10-22 06:00:00')"
    ))
    .await;
    w.workspace_upload(u, "a.png", &[7u8; 100]);
    let prompt = format!(
        "{}\n\nhello <pasted_content id=\"p1\">\npasted\n</pasted_content id=\"p1\">\n\n<image_uploads>\nThe 1 image(s) \
         attached to this message, which you can see, are also saved as files (the originals, full size):\n1. \
         /workspace/uploads/a.png (original name: a.png)\nUse these paths when you need the image files \
         themselves.\n</image_uploads>\n\n<files_input>\nThe user attached 1 file(s) to this message.\n1. \
         /workspace/uploads/r.pdf (original name: r.pdf)\n</files_input>",
        magpie::ccfs::display::LEGACY_SANDBOX_RESET_NOTE
    );
    w.put(
        &k,
        &format!("projects/-workspace/{p}.jsonl"),
        &user("l1", "2026-10-22T06:00:01.000Z", &prompt),
    );
    pass::pass(&w.imp, None).await.unwrap();
    verify_all(&w).await;
    assert!(
        verify::verify(&w.imp, u).await.unwrap().notes.is_empty(),
        "the workspace exists"
    );
    let r = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select meta, extract(epoch from at)::bigint from events where conversation_id = $1 and kind = 'cc.user'",
            &[&u],
        )
        .await
        .unwrap();
    let meta: serde_json::Value = r.get(0);
    assert_eq!(meta["displayText"], "hello pasted");
    assert_eq!(
        meta["attachments"],
        serde_json::json!([
            {"path": "uploads/a.png", "name": "a.png", "mimeType": "", "size": 100},
            {"path": "uploads/r.pdf", "name": "r.pdf", "mimeType": "", "size": 0, "missing": true}
        ])
    );
    assert_eq!(r.get::<_, i64>(1), 1_792_648_801, "the line's own time");
}

#[tokio::test]
async fn conflicts_vanished_rows_forced_release_and_reset() {
    let w = World::new("control").await;
    let k = w.person(ALICE).await;
    let (native, gone, kept, forced) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    for id in [native, gone, kept, forced] {
        w.cloudcli(&format!(
            "insert into cc_{k}.sessions (session_id, provider_session_id, created_at, updated_at)
               values ('{id}', '{id}', '2026-10-22 06:00:00', '2026-10-22 06:00:00')"
        ))
        .await;
        w.put(
            &k,
            &format!("projects/-workspace/{id}.jsonl"),
            &user("l1", "2026-10-22T06:00:01.000Z", "one"),
        );
    }
    // a native conversation with an imported row's id
    let c = w.magpie().get().await.unwrap();
    magpie::conversations::ensure_person(&c, ALICE).await.unwrap();
    c.execute(
        "insert into conversations (id, person, created_at, activity_at) values ($1, $2, now(), now())",
        &[&native, &ALICE],
    )
    .await
    .unwrap();
    drop(c);
    let p = plan::plan(&w.imp).await.unwrap();
    assert_eq!(p.blockers.len(), 1, "{:?}", p.blockers);
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert_eq!(r.conflicts, vec![native]);
    assert_eq!(r.created, 3);
    assert_eq!(w.conv(native).await["migrating"], false);

    // a sessions row that vanished: its conversation goes
    w.cloudcli(&format!("delete from cc_{k}.sessions where session_id = '{gone}'"))
        .await;
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert_eq!(r.purged, 1);
    let left: i64 = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from conversations where id = $1", &[&gone])
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);

    // release --force: native from now on, never written by the importer again
    let f = control::release_forced(&w.imp, forced).await.unwrap();
    assert!(f.fence_seconds >= 0.0);
    assert_eq!(w.conv(forced).await["migrating"], false);
    let reason: Option<String> = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select release_reason from import_sources where conversation_id = $1",
            &[&forced],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(reason.as_deref(), Some("forced"));
    let v = verify::verify(&w.imp, forced).await.unwrap();
    assert!(v.released && v.ok());
    w.append(
        &k,
        &format!("projects/-workspace/{forced}.jsonl"),
        &assistant("l2", "2026-10-22T06:00:02.000Z", "x"),
    );
    let before = w.versions(forced).await;
    pass::pass(&w.imp, None).await.unwrap();
    assert_eq!(w.versions(forced).await, before);
    let v = verify::verify(&w.imp, forced).await.unwrap();
    assert!(!v.ok(), "the old side wrote after the release: verify says so");

    // reset removes the migrating imports only
    assert_eq!(control::reset(&w.imp).await.unwrap(), 1);
    assert_eq!(w.conv(forced).await["migrating"], false);
    let n: i64 = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from conversations where id = $1", &[&kept])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
}

#[tokio::test]
async fn sent_files_and_the_view_as_audit() {
    let w = World::new("sent").await;
    let k = w.person(ALICE).await;
    let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{k}.sessions (session_id, provider_session_id, created_at, updated_at)
           values ('{u}', '{p}', '2026-10-22 06:00:00', '2026-10-22 06:00:00')"
    ))
    .await;
    let mut t = user("l1", "2026-10-22T06:00:01.000Z", "send me the report");
    t.extend(common::line(&serde_json::json!({
        "type": "assistant", "uuid": "l2", "timestamp": "2026-10-22T06:00:02.000Z",
        "message": {"id": "msg_l2", "role": "assistant", "model": "claude-test", "content": [
            {"type": "tool_use", "id": "toolu_abc", "name": "SendUserFile",
             "input": {"files": ["/workspace/out/report.pdf"], "caption": " the report "}}
        ]}
    })));
    w.put(&k, &format!("projects/-workspace/{p}.jsonl"), &t);
    let dir = w.jfs.0.join("sent").join(u.to_string()).join("toolu_abc").join("out");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("report.pdf"), b"%PDF-1.4 a report").unwrap();
    w.jarvis(&format!(
        "insert into ops_events (kind, conversation_id, detail) values
           ('view_as', null, '{{\"admin\": \"Boss@Example.com\", \"user\": \"{ALICE}\", \"session\": \"s1\", \"event\": \"start\"}}'),
           ('view_as', '{u}', '{{\"admin\": \"boss@example.com\", \"user\": \"{ALICE}\", \"session\": \"s1\", \"event\": \"conversation\", \"chars\": 12}}'),
           ('view_as', null, '{{\"admin\": \"boss@example.com\", \"user\": \"{ALICE}\", \"session\": \"s1\", \"event\": \"peek\"}}'),
           ('other', null, '{{}}')"
    ))
    .await;
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!((r.view_as.copied, r.view_as.skipped), (2, 1));
    let c = w.magpie().get().await.unwrap();
    let row = c
        .query_one(
            "select s.path, s.name, s.caption, s.size, e.data->>'toolUseSeq', e.seq
             from sent_files s join events e on e.conversation_id = s.conversation_id and e.seq = s.seq
             where s.conversation_id = $1",
            &[&u],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "out/report.pdf");
    assert_eq!(row.get::<_, String>(1), "report.pdf");
    assert_eq!(row.get::<_, Option<String>>(2).as_deref(), Some("the report"));
    assert_eq!(row.get::<_, i64>(3), 17);
    let call: i64 = c
        .query_one(
            "select seq from events where conversation_id = $1 and kind = 'cc.assistant'",
            &[&u],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(row.get::<_, Option<String>>(4), Some(call.to_string()));
    let audit: Vec<(String, String, Option<i32>)> = c
        .query("select admin, event, chars from view_as_log order by id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    assert_eq!(
        audit,
        [
            ("boss@example.com".to_owned(), "start".to_owned(), None),
            ("boss@example.com".to_owned(), "send".to_owned(), Some(12))
        ]
    );
    drop(c);
    let n = w.kinds(u).await.len();
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert_eq!((r.changed, r.view_as.copied, r.view_as.present), (0, 0, 2));
    assert_eq!(w.kinds(u).await.len(), n);
}
