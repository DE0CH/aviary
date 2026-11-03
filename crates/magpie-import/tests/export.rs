//! The reverse export (cutover 14.3, 7.12 item 7): a conversation imported and released, then grown on the new side
//! (turns, a tool result, a sent file, a failed turn, a new title; written here through the importer from a second
//! tree standing in for what the new side's sandboxes wrote), exported back: the old side's files equal magpie's
//! reconstruction, the sessions row and the failed-turn row match, the memory went to `aviary-memory/<id>/`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;

use common::{World, assistant, line, user};
use magpie_import::{control, export, pass, verify};
use time::OffsetDateTime;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

async fn set(w: &World, sql: &str, id: Uuid) {
    w.magpie().get().await.unwrap().execute(sql, &[&id]).await.unwrap();
}

#[tokio::test]
async fn an_owned_conversation_exports_back_to_the_old_side() {
    let w = World::new("export").await;
    let ka = w.person(ALICE).await;
    let (u1, p1) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, name_source, created_at,
                                       updated_at)
           values ('{u1}', '{p1}', 'Report', 'user', '2026-10-22 06:22:36', '2026-10-22T07:00:00.000Z');"
    ))
    .await;
    let main = format!("projects/-workspace/{p1}.jsonl");
    let first = [
        user("a1", "2026-10-22T06:22:40.000Z", "write the report"),
        assistant("a2", "2026-10-22T06:23:00.000Z", "here it is"),
    ]
    .concat();
    w.put(&ka, &main, &first);
    w.put(
        &ka,
        "projects/-workspace/memory/MEMORY.md",
        b"- prefers short answers\n",
    );
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    control::release_forced(&w.imp, u1).await.unwrap();
    let since = OffsetDateTime::now_utc();

    // the new side grows the conversation: a second turn with a tool result and a sent file, a failed turn
    let newside = magpie_import::Importer {
        jfs: w.jfs.0.join("new-side"),
        ..w.imp.clone()
    };
    let nclaude = magpie_import::tree::claude_dir(&newside.jfs, &ka);
    let put = |rel: &str, b: &[u8]| {
        let p = nclaude.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b).unwrap();
    };
    let second = [
        user("a3", "2030-01-01T00:00:00.000Z", "send it to me"),
        line(&serde_json::json!({
            "type": "assistant", "uuid": "a4", "timestamp": "2030-01-01T00:00:01.000Z",
            "message": {"id": "msg_a4", "role": "assistant", "model": "claude-test", "content": [
                {"type": "tool_use", "id": "toolu_s", "name": "SendUserFile",
                 "input": {"files": ["/workspace/out/report.pdf"], "caption": "the report"}}
            ]}
        })),
        assistant("a5", "2030-01-01T00:00:02.000Z", "sent"),
    ]
    .concat();
    let grown = [first.clone(), second].concat();
    put(&main, &grown);
    put(
        &format!("projects/-workspace/{p1}/tool-results/toolu_s.txt"),
        b"tool output\n",
    );
    put(
        "projects/-workspace/memory/MEMORY.md",
        b"- prefers short answers\n- and tables\n",
    );
    let sent = newside
        .jfs
        .join("sent")
        .join(u1.to_string())
        .join("toolu_s")
        .join("out");
    std::fs::create_dir_all(&sent).unwrap();
    std::fs::write(sent.join("report.pdf"), b"%PDF-1.4 the report").unwrap();
    w.cloudcli(&format!(
        "insert into cc_{ka}.jarvis_failed_turns (session_id, user_text, user_at, error, error_at)
           values ('{u1}', 'and a chart?', '2030-01-01 00:01:00', '所有電腦正在使用中，請過幾分鐘再試。',
                   '2030-01-01T00:01:05.000Z');"
    ))
    .await;
    set(&w, "update conversations set migrating = true where id = $1", u1).await;
    set(
        &w,
        "update import_sources set released_at = null where conversation_id = $1",
        u1,
    )
    .await;
    let only: BTreeSet<Uuid> = [u1].into();
    let r = pass::pass(&newside, Some(&only)).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    // the turns written by the new side are native (not imported), and the title changed there
    w.magpie()
        .get()
        .await
        .unwrap()
        .execute(
            "update turns set origin = 'person' where conversation_id = $1 and origin = 'import'
               and ended_at > $2",
            &[&u1, &since],
        )
        .await
        .unwrap();
    set(
        &w,
        "update conversations set migrating = false, title = 'Renamed', name_source = 'user' where id = $1",
        u1,
    )
    .await;
    set(
        &w,
        "update import_sources set released_at = now() where conversation_id = $1",
        u1,
    )
    .await;
    // the old side has none of it
    w.cloudcli(&format!("delete from cc_{ka}.jarvis_failed_turns")).await;

    let x = export::export(&w.imp, Some(since), &[]).await.unwrap();
    assert!(x.errors.is_empty(), "{:?}", x.errors);
    assert_eq!(x.conversations.len(), 1, "{x:?}");
    let c = &x.conversations[0];
    assert_eq!(c.conversation, u1);
    assert!(c.row);
    assert_eq!(c.memory, 1);
    assert_eq!(c.failed_turns, 1);
    assert_eq!(c.sent_files, 1);
    assert_eq!(x.shares_not_exported, Vec::<String>::new());

    // every exported file equals magpie's reconstruction
    let claude = w.claude(&ka);
    for (path, _) in verify::magpie_files(&w.imp, u1).await.unwrap() {
        let dest = path.strip_prefix("projects/-workspace/memory/").map_or_else(
            || claude.join(&path),
            |rest| claude.join("aviary-memory").join(u1.to_string()).join(rest),
        );
        let bytes = std::fs::read(&dest).unwrap_or_else(|e| panic!("{}: {e}", dest.display()));
        let (sha, size) = verify::reconstruct(&w.imp, u1, ALICE, &path).await.unwrap().unwrap();
        assert_eq!(
            (magpie_import::sha_hex(&bytes), u64::try_from(bytes.len()).unwrap()),
            (sha, size),
            "{path}"
        );
        assert_eq!(std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(std::fs::read(claude.join(&main)).unwrap(), grown);
    assert_eq!(
        std::fs::read(claude.join(format!("projects/-workspace/{p1}/tool-results/toolu_s.txt"))).unwrap(),
        b"tool output\n"
    );
    // the memory went to its own folder; the person's shared old memory is untouched
    assert_eq!(
        std::fs::read(claude.join(format!("aviary-memory/{u1}/MEMORY.md"))).unwrap(),
        b"- prefers short answers\n- and tables\n"
    );
    assert_eq!(
        std::fs::read(claude.join("projects/-workspace/memory/MEMORY.md")).unwrap(),
        b"- prefers short answers\n"
    );
    // the sent file from its blob
    assert_eq!(
        std::fs::read(w.jfs.0.join("sent").join(u1.to_string()).join("toolu_s/out/report.pdf")).unwrap(),
        b"%PDF-1.4 the report"
    );
    // the sessions row and the failed turn
    let c = w.imp.cloudcli.get().await.unwrap();
    let row = c
        .query_one(
            &format!(
                "select custom_name, name_source, provider_session_id, jsonl_path, \"isArchived\", deleted_at
                 from cc_{ka}.sessions where session_id = $1"
            ),
            &[&u1.to_string()],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<String>>(0).as_deref(), Some("Renamed"));
    assert_eq!(row.get::<_, Option<String>>(1).as_deref(), Some("user"));
    assert_eq!(row.get::<_, Option<String>>(2), Some(p1.to_string()));
    assert_eq!(
        row.get::<_, Option<String>>(3),
        Some(format!("/var/lib/cloudcli/home/{ka}/.claude/{main}"))
    );
    assert_eq!(row.get::<_, Option<i32>>(4), Some(0));
    assert_eq!(row.get::<_, Option<String>>(5), None);
    let f = c
        .query(
            &format!("select user_text, error from cc_{ka}.jarvis_failed_turns where session_id = $1"),
            &[&u1.to_string()],
        )
        .await
        .unwrap();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].get::<_, Option<String>>(0).as_deref(), Some("and a chart?"));
    assert_eq!(f[0].get::<_, String>(1), "所有電腦正在使用中，請過幾分鐘再試。");

    // the importer reads the exported files as the old system's: a fresh import of them is byte-exact
    let x2 = export::export(&w.imp, Some(since), &[]).await.unwrap();
    assert_eq!(x2.conversations[0].failed_turns, 0, "a failed turn is written once");
    set(&w, "update conversations set migrating = true where id = $1", u1).await;
    set(
        &w,
        "update import_sources set released_at = null where conversation_id = $1",
        u1,
    )
    .await;
    let r = pass::pass(&w.imp, Some(&only)).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let v = verify::verify(&w.imp, u1).await.unwrap();
    assert!(v.ok(), "{v:?}");
}
