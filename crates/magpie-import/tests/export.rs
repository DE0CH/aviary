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
    // file times come from a coarser clock than `now`: what the new side writes is after `since` on it too
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

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
    assert!(c.shares.written.is_empty() && x.purged.is_empty(), "{x:?}");

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

    // a share made on the new side (as magpie's M16 makes it, hermit's clone stood in by a folder) goes back in the
    // old layout, without messages.json; a second export leaves it; a revoke on the new side revokes it there
    let sid = new_side_share(&w, u1).await;
    let ws = w.jfs.0.join("ws").join(magpie::shares::workspace_of(&sid).to_string());
    std::fs::create_dir_all(ws.join("uploads")).unwrap();
    std::fs::create_dir_all(ws.join("out")).unwrap();
    std::fs::write(ws.join("uploads/photo.png"), b"PNG").unwrap();
    std::fs::write(ws.join("out/report.pdf"), b"%PDF-1.4 live").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", ws.join("uploads/link")).unwrap();
    let x = export::export(&w.imp, Some(since), &[]).await.unwrap();
    assert!(x.errors.is_empty(), "{:?}", x.errors);
    assert_eq!(x.conversations[0].shares.written, vec![sid.clone()]);
    let dir = w.jfs.0.join("users").join(&ka).join("shares").join(&sid);
    assert_eq!(std::fs::read(dir.join("transcript.jsonl")).unwrap(), grown);
    assert_eq!(
        std::fs::read(dir.join("session/tool-results/toolu_s.txt")).unwrap(),
        b"tool output\n"
    );
    assert_eq!(
        std::fs::read(dir.join("sent/toolu_s/out/report.pdf")).unwrap(),
        b"%PDF-1.4 the report"
    );
    assert_eq!(std::fs::read(dir.join("ws/uploads/photo.png")).unwrap(), b"PNG");
    assert!(
        std::fs::symlink_metadata(dir.join("ws/uploads/link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!dir.join("messages.json").exists());
    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    let row = c
        .query_one(
            "select owner, owner_name, session_id, title, dir, files::text, revoked_at is null
             from cc_shared.shares where id = $1",
            &[&sid],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), ALICE);
    assert_eq!(row.get::<_, String>(1), "alice");
    assert_eq!(row.get::<_, String>(2), u1.to_string());
    assert_eq!(row.get::<_, String>(3), "Renamed");
    assert_eq!(row.get::<_, String>(4), format!("/jfs/users/{ka}/shares/{sid}"));
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&row.get::<_, String>(5)).unwrap(),
        vec!["out/report.pdf".to_owned(), "uploads/photo.png".to_owned()]
    );
    assert!(row.get::<_, bool>(6));
    let again = export::export(&w.imp, Some(since), &[]).await.unwrap();
    assert!(again.conversations[0].shares.written.is_empty(), "{again:?}");
    w.magpie()
        .get()
        .await
        .unwrap()
        .execute(
            "update shares set revoked_at = now(), revoke_reason = 'owner' where id = $1",
            &[&sid],
        )
        .await
        .unwrap();
    let x = export::export(&w.imp, Some(since), &[]).await.unwrap();
    assert_eq!(x.conversations[0].shares.revoked, vec![sid.clone()]);
    assert!(!dir.exists());
    let live: bool = c
        .query_one("select revoked_at is null from cc_shared.shares where id = $1", &[&sid])
        .await
        .unwrap()
        .get(0);
    assert!(!live);

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

/// A share of `conversation` frozen up to its head, as M16 makes it (`magpie::shares::create` without hermit's clone).
async fn new_side_share(w: &World, conversation: Uuid) -> String {
    let id = magpie::shares::new_id().unwrap();
    let snapshot = Uuid::new_v4();
    let workspace = magpie::shares::workspace_of(&id);
    let idr = &id;
    aviary_pg::tx!(w.magpie(), |t| -> magpie::error::Result<()> {
        t.execute(
            "insert into shares (id, conversation_id, owner, owner_name, title, upto_seq, snapshot,
                                 provider_session_id, workspace_id, model)
             select $1, c.id, c.person, split_part(c.person, '@', 1), c.title, c.head_seq, $3, c.provider_session_id,
                    $4, c.model
             from conversations c where c.id = $2",
            &[idr, &conversation, &snapshot, &workspace],
        )
        .await?;
        magpie::shares::copy_scope(t, conversation, snapshot, None).await?;
        magpie::shares::copy_refs(t, conversation, snapshot, "share").await?;
        Ok(())
    })
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn a_conversation_deleted_for_good_on_the_new_side_is_deleted_on_the_old_side() {
    let w = World::new("export_purge").await;
    let ka = w.person(ALICE).await;
    let (u1, p1, u2, p2) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, name_source, created_at,
                                       updated_at)
           values ('{u1}', '{p1}', 'Gone', 'user', '2026-10-22 06:22:36', '2026-10-22T07:00:00.000Z'),
                  ('{u2}', '{p2}', 'Kept', 'user', '2026-10-22 06:22:36', '2026-10-22T07:00:00.000Z');
         insert into cc_{ka}.jarvis_failed_turns (session_id, user_text, user_at, error, error_at)
           values ('{u1}', 'and a chart?', '2026-10-22 06:30:00', 'x', '2026-10-22T06:30:05.000Z');"
    ))
    .await;
    let turn = [
        user("a1", "2026-10-22T06:22:40.000Z", "write the report"),
        assistant("a2", "2026-10-22T06:23:00.000Z", "here it is"),
    ]
    .concat();
    for p in [p1, p2] {
        w.put(&ka, &format!("projects/-workspace/{p}.jsonl"), &turn);
        w.put(&ka, &format!("projects/-workspace/{p}/tool-results/t.txt"), b"out\n");
    }
    w.put(&ka, "projects/-workspace/memory/MEMORY.md", b"- shared\n");
    let old_share = "AAAAAAAAAAAAAAAAAAAAAg";
    w.share(old_share, ALICE, u1, &turn, false).await;
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    for u in [u1, u2] {
        control::release_forced(&w.imp, u).await.unwrap();
    }
    // what the old side holds besides: an exported memory folder and a sent file
    let claude = w.claude(&ka);
    std::fs::create_dir_all(claude.join(format!("aviary-memory/{u1}"))).unwrap();
    std::fs::write(claude.join(format!("aviary-memory/{u1}/MEMORY.md")), b"- mine\n").unwrap();
    let sent = w.jfs.0.join("sent").join(u1.to_string()).join("t1");
    std::fs::create_dir_all(&sent).unwrap();
    std::fs::write(sent.join("a.pdf"), b"%PDF").unwrap();
    let since = OffsetDateTime::now_utc();

    // deleted for good on the new side: the tombstone records its Claude Code files
    set(&w, "update conversations set deleted_at = now() where id = $1", u1).await;
    aviary_pg::tx!(w.magpie(), |t| -> magpie::error::Result<()> {
        t.execute("select 1 from conversations where id = $1 for update", &[&u1])
            .await?;
        magpie::conversations::delete::purge_locked(t, ALICE, u1, magpie::conversations::delete::ShareRevoke::Deleted)
            .await
    })
    .await
    .unwrap();
    let paths: Vec<String> = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one("select cc_paths from import_purged where conversation_id = $1", &[&u1])
        .await
        .unwrap()
        .get(0);
    assert!(paths.contains(&format!("projects/-workspace/{p1}.jsonl")), "{paths:?}");
    assert!(
        paths.contains(&"projects/-workspace/memory/MEMORY.md".to_owned()),
        "{paths:?}"
    );
    // a tombstone of a conversation that is in magpie again is left alone
    w.magpie()
        .get()
        .await
        .unwrap()
        .execute(
            "insert into import_purged (conversation_id, person, provider_session_id, cc_paths)
             values ($1, $2, $3, array[$4])",
            &[&u2, &ALICE, &p2.to_string(), &format!("projects/-workspace/{p2}.jsonl")],
        )
        .await
        .unwrap();

    let x = export::export(&w.imp, Some(since), &[]).await.unwrap();
    assert!(x.errors.is_empty(), "{:?}", x.errors);
    assert_eq!(x.purged.len(), 1, "{x:?}");
    assert_eq!(x.purged[0].conversation, u1);
    assert!(x.purged[0].row);
    assert_eq!(x.purged[0].shares_revoked, 1);
    assert!(!claude.join(format!("projects/-workspace/{p1}.jsonl")).exists());
    assert!(!claude.join(format!("projects/-workspace/{p1}")).exists());
    assert!(!claude.join(format!("aviary-memory/{u1}")).exists());
    assert!(!w.jfs.0.join("sent").join(u1.to_string()).exists());
    assert!(!w.jfs.0.join("users").join(&ka).join("shares").join(old_share).exists());
    // the person's shared memory and the other conversation stay
    assert_eq!(
        std::fs::read(claude.join("projects/-workspace/memory/MEMORY.md")).unwrap(),
        b"- shared\n"
    );
    assert_eq!(
        std::fs::read(claude.join(format!("projects/-workspace/{p2}.jsonl"))).unwrap(),
        turn
    );
    assert!(
        claude
            .join(format!("projects/-workspace/{p2}/tool-results/t.txt"))
            .exists()
    );
    let c = w.imp.cloudcli.get().await.unwrap();
    let sessions: Vec<String> = c
        .query(
            &format!("select session_id from cc_{ka}.sessions order by session_id"),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(sessions, vec![u2.to_string()]);
    let failed: i64 = c
        .query_one(&format!("select count(*) from cc_{ka}.jarvis_failed_turns"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(failed, 0);
    let live: bool = c
        .query_one(
            "select revoked_at is null from cc_shared.shares where id = $1",
            &[&old_share],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!live);
    // the importer does not bring it back
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let back: i64 = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from conversations where id = $1", &[&u1])
        .await
        .unwrap()
        .get(0);
    assert_eq!(back, 0);
}
