//! The importer's hand-over safety (cutover 7.3, 7.7, 7.8, 7.10): `release --force` syncs once more inside its
//! fence, `verified_stats` holds every fingerprint, verification's re-ingest leaves the record and the events
//! right, a released conversation's check leaves the person's shared memory out and sees new files, a mount that
//! is not there stops a pass before it unlinks anything, a send's folder waits until it is quiet, the lock lost
//! stops a command, and the old databases are read-only for every command but the export.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt as _;
use std::time::Duration;

use common::{World, assistant, user};
use magpie_import::{Error, control, lock, pass, sync, tree, verify};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

async fn one_conversation(w: &World, ka: &str) -> (Uuid, Uuid, String, Vec<u8>) {
    let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, name_source, created_at,
                                       updated_at)
           values ('{u}', '{p}', 'Report', 'user', '2026-10-22 06:22:36', '2026-10-22T07:00:00.000Z');"
    ))
    .await;
    let main = format!("projects/-workspace/{p}.jsonl");
    let first = [
        user("a1", "2026-10-22T06:22:40.000Z", "write the report"),
        assistant("a2", "2026-10-22T06:23:00.000Z", "here it is"),
    ]
    .concat();
    w.put(ka, &main, &first);
    (u, p, main, first)
}

async fn files_record(w: &World, id: Uuid) -> sync::Files {
    serde_json::from_value(
        w.magpie()
            .get()
            .await
            .unwrap()
            .query_one("select files from import_sources where conversation_id = $1", &[&id])
            .await
            .unwrap()
            .get(0),
    )
    .unwrap()
}

#[tokio::test]
async fn a_forced_release_syncs_inside_its_fence_and_verified_stats_hold_every_fingerprint() {
    let w = World::new("force_sync").await;
    let ka = w.person(ALICE).await;
    let (u1, _, main, first) = one_conversation(&w, &ka).await;
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let v = verify::verify(&w.imp, u1).await.unwrap();
    assert!(v.ok(), "{v:?}");
    assert_eq!(
        verify::changed_since_verify(&w.imp, u1).await.unwrap(),
        Vec::<String>::new()
    );

    // a rename on the old side after verification: the release-time check sees it (the release must not go)
    w.cloudcli(&format!(
        "update cc_{ka}.sessions set custom_name = 'Renamed' where session_id = '{u1}'"
    ))
    .await;
    let changes = verify::changed_since_verify(&w.imp, u1).await.unwrap();
    assert_eq!(changes, vec!["the sessions row changed".to_owned()]);
    // ... and verification does not pass until a pass has synced it
    let v = verify::verify(&w.imp, u1).await.unwrap();
    assert!(!v.ok() && v.mismatches.is_empty(), "{v:?}");
    assert_eq!(
        v.pending,
        vec!["the sessions row changed since the last pass".to_owned()]
    );
    // a failed turn and more transcript after the verification, then `release --force`: the final sync inside the
    // fence takes all of it, and nothing the old side wrote before the release is left out
    w.cloudcli(&format!(
        "insert into cc_{ka}.jarvis_failed_turns (session_id, user_text, user_at, error, error_at)
           values ('{u1}', 'and a chart?', '2026-10-22 06:30:00', 'x', '2026-10-22T06:30:05.000Z');"
    ))
    .await;
    let more = assistant("a3", "2026-10-22T06:31:00.000Z", "and one more thing");
    w.append(&ka, &main, &more);
    let f = control::release_forced(&w.imp, u1).await.unwrap();
    assert!(!f.verified && f.mismatches.is_empty(), "{f:?}");
    assert!(!f.pending.is_empty(), "{f:?}");
    assert_eq!(w.conv(u1).await["title"], "Renamed");
    let (sha, n) = verify::reconstruct(&w.imp, u1, ALICE, &main).await.unwrap().unwrap();
    let all = [first, more].concat();
    assert_eq!(
        (sha, n),
        (magpie_import::sha_hex(&all), u64::try_from(all.len()).unwrap())
    );
    assert!(w.kinds(u1).await.contains(&"turn.ended".to_owned()));
    // released: the record is what was there at the release, so a verify finds the old side unwritten since
    let v = verify::verify(&w.imp, u1).await.unwrap();
    assert!(v.released && v.ok(), "{v:?}");
}

#[tokio::test]
async fn a_change_after_the_last_sync_is_seen_by_the_release_check() {
    let w = World::new("since_sync").await;
    let ka = w.person(ALICE).await;
    let (u1, p1, main, _) = one_conversation(&w, &ka).await;
    pass::pass(&w.imp, None).await.unwrap();
    assert_eq!(
        verify::changed_since_sync(&w.imp, u1).await.unwrap(),
        Vec::<String>::new()
    );
    w.append(&ka, &main, &assistant("a3", "2026-10-22T06:31:00.000Z", "more"));
    w.put(&ka, &format!("projects/-workspace/{p1}/tool-results/t.txt"), b"out\n");
    let sent = w.jfs.0.join("sent").join(u1.to_string()).join("toolu_1");
    std::fs::create_dir_all(&sent).unwrap();
    std::fs::write(sent.join("a.pdf"), b"%PDF").unwrap();
    let mut c = verify::changed_since_sync(&w.imp, u1).await.unwrap();
    c.sort();
    assert_eq!(
        c,
        vec![
            format!("{main} changed"),
            format!("projects/-workspace/{p1}/tool-results/t.txt is new"),
            "the sent files changed".to_owned(),
        ]
    );
}

#[tokio::test]
async fn a_reingest_flushes_its_limbo_and_updates_the_record() {
    let w = World::new("reingest").await;
    let ka = w.person(ALICE).await;
    let (u1, _, main, first) = one_conversation(&w, &ka).await;
    let more = [
        user("a3", "2026-10-22T06:30:00.000Z", "and then"),
        assistant("a4", "2026-10-22T06:31:00.000Z", "done"),
    ]
    .concat();
    w.append(&ka, &main, &more);
    pass::pass(&w.imp, None).await.unwrap();
    // the old side drops the last exchange (a rewrite) and no pass has synced it: verification re-ingests it
    w.rename_over(&ka, &main, &first);
    let v = verify::verify(&w.imp, u1).await.unwrap();
    assert!(v.ok(), "{v:?}");
    assert_eq!(v.reingested, vec![main.clone()]);
    assert!(
        v.notes.iter().all(|n| n.starts_with("no workspace")),
        "the retract of the removed lines happened: {:?}",
        v.notes
    );
    let rec = files_record(&w, u1).await;
    assert_eq!(rec[&main].sha256, magpie_import::sha_hex(&first));
    assert_eq!(rec[&main].size, u64::try_from(first.len()).unwrap());
    // the record matches the source: the next pass has nothing to do
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert_eq!((r.created, r.changed), (0, 0), "{r:?}");
}

#[tokio::test]
async fn a_rename_onto_a_path_magpie_already_has_unlinks_the_old_one() {
    let w = World::new("rename_both").await;
    let ka = w.person(ALICE).await;
    let (u1, p1, _, _) = one_conversation(&w, &ka).await;
    let a = format!("projects/-workspace/{p1}/subagents/agent-a.jsonl");
    let b = format!("projects/-workspace/{p1}/subagents/agent-b.jsonl");
    let body = assistant("s1", "2026-10-22T06:24:00.000Z", "sub-agent");
    w.put(&ka, &a, &body);
    pass::pass(&w.imp, None).await.unwrap();
    // Claude Code moves a to b; an earlier pass created b in magpie and stopped before recording it
    w.put(&ka, &b, &body);
    std::fs::remove_file(w.claude(&ka).join(&a)).unwrap();
    let srcs = tree::sources(&w.jfs.0, &ka, &p1.to_string(), Some(u1)).unwrap();
    let only_b: std::collections::BTreeMap<String, tree::Source> = srcs
        .iter()
        .filter(|(p, _)| **p == b)
        .map(|(p, s)| (p.clone(), s.clone()))
        .collect();
    let mut wr = sync::Writer::new(&w.imp, u1, ALICE, std::collections::BTreeMap::new());
    wr.sync_all(&only_b, &sync::Files::new(), None, None).await.unwrap();
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let paths: Vec<String> = w.versions(u1).await.into_iter().map(|(p, _)| p).collect();
    assert!(paths.contains(&b) && !paths.contains(&a), "{paths:?}");
    assert!(verify::verify(&w.imp, u1).await.unwrap().ok());
}

#[tokio::test]
async fn a_released_conversation_ignores_shared_memory_and_sees_new_files() {
    let w = World::new("released_check").await;
    let ka = w.person(ALICE).await;
    let (u1, p1, _, _) = one_conversation(&w, &ka).await;
    let (_, _, main2, _) = one_conversation(&w, &ka).await;
    w.put(&ka, "projects/-workspace/memory/MEMORY.md", b"- one\n");
    pass::pass(&w.imp, None).await.unwrap();
    control::release_forced(&w.imp, u1).await.unwrap();
    // the person's other conversation, still legacy, changes the shared memory and its own transcript
    w.put(&ka, "projects/-workspace/memory/MEMORY.md", b"- one\n- two\n");
    w.append(&ka, &main2, &assistant("b3", "2026-10-22T07:00:00.000Z", "more"));
    for id in verify::targets(&w.imp, &[]).await.unwrap() {
        if id == u1 {
            let v = verify::verify(&w.imp, id).await.unwrap();
            assert!(v.released && v.ok(), "{v:?}");
        }
    }
    // a file the old side made in the released conversation's session folder after the release is seen
    let late = format!("projects/-workspace/{p1}/tool-results/late.txt");
    w.put(&ka, &late, b"late\n");
    // after a forced release it is the cut-off turn's write: reported apart, and verification passes
    let v = verify::verify(&w.imp, u1).await.unwrap();
    assert!(v.ok(), "{v:?}");
    assert_eq!(v.forced_writes.len(), 1, "{v:?}");
    assert_eq!(v.forced_writes[0].path, late);
    // after a hand-over's (idle) release it is a mismatch
    w.imp
        .magpie
        .pool
        .get()
        .await
        .unwrap()
        .execute(
            "update import_sources set release_reason = 'idle' where conversation_id = $1",
            &[&u1],
        )
        .await
        .unwrap();
    let v = verify::verify(&w.imp, u1).await.unwrap();
    assert!(!v.ok() && v.forced_writes.is_empty(), "{v:?}");
    assert_eq!(v.mismatches.len(), 1, "{v:?}");
    assert_eq!(v.mismatches[0].path, late);
}

#[tokio::test]
async fn a_mount_that_is_not_there_unlinks_nothing() {
    let w = World::new("mount").await;
    let ka = w.person(ALICE).await;
    let (u1, _, main, _) = one_conversation(&w, &ka).await;
    pass::pass(&w.imp, None).await.unwrap();
    let before = w.versions(u1).await;
    // an empty mount point
    let empty = magpie_import::Importer {
        jfs: w.jfs.0.join("not-mounted"),
        ..w.imp.clone()
    };
    std::fs::create_dir_all(&empty.jfs).unwrap();
    match pass::pass(&empty, None).await {
        Err(Error::Source(e)) => assert!(e.contains("does not look mounted"), "{e}"),
        other => panic!("{other:?}"),
    }
    assert!(magpie_import::export::export(&empty, None, &[], true).await.is_err());
    // the person's folder gone for a moment while the sessions row is there: the conversation's error, no unlink
    let claude = w.claude(&ka);
    let aside = claude.with_extension("aside");
    std::fs::rename(&claude, &aside).unwrap();
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(
        r.errors
            .iter()
            .any(|(id, e)| *id == u1.to_string() && e.contains(&main)),
        "{:?}",
        r.errors
    );
    assert_eq!(w.versions(u1).await, before);
    std::fs::rename(&aside, &claude).unwrap();
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(w.versions(u1).await, before);
}

#[tokio::test]
async fn a_send_folder_is_imported_once_it_is_quiet() {
    let w = World::new("sent_quiet").await;
    let ka = w.person(ALICE).await;
    let (u1, _, _, _) = one_conversation(&w, &ka).await;
    let quiet = magpie_import::Importer {
        sent_quiet: Duration::from_secs(3600),
        ..w.imp.clone()
    };
    pass::pass(&quiet, None).await.unwrap();
    let sent = w.jfs.0.join("sent").join(u1.to_string()).join("toolu_1");
    std::fs::create_dir_all(&sent).unwrap();
    std::fs::write(sent.join("a.pdf"), b"%PDF one").unwrap();
    // the hook is still writing: nothing imported, and verification waits
    let r = pass::pass(&quiet, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let count = || async {
        w.magpie()
            .get()
            .await
            .unwrap()
            .query_one("select count(*) from sent_files where conversation_id = $1", &[&u1])
            .await
            .unwrap()
            .get::<_, i64>(0)
    };
    assert_eq!(count().await, 0);
    let v = verify::verify(&quiet, u1).await.unwrap();
    assert!(!v.ok(), "{v:?}");
    // the second file lands; once quiet, both are imported in one send
    std::fs::write(sent.join("b.pdf"), b"%PDF two").unwrap();
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(count().await, 2);
    assert!(verify::verify(&w.imp, u1).await.unwrap().ok());
}

#[tokio::test]
async fn a_lost_lock_stops_the_command() {
    let w = World::new("lock_lost").await;
    let ka = w.person(ALICE).await;
    one_conversation(&w, &ka).await;
    let held = lock::OneShot::take(w.magpie()).await.unwrap();
    assert!(held.held().await);
    let imp = magpie_import::Importer {
        lock: Some(std::sync::Arc::new(held)),
        ..w.imp.clone()
    };
    imp.check_lock().await.unwrap();
    // its connection ends (a server restart, an idle kill)
    let pid: i32 = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select pid from pg_locks where locktype = 'advisory' and granted and objsubid = 1
               and ((classid::bigint << 32) | objid::bigint) = hashtext($1)::bigint",
            &[&lock::NAME],
        )
        .await
        .unwrap()
        .get(0);
    w.magpie()
        .get()
        .await
        .unwrap()
        .execute("select pg_terminate_backend($1)", &[&pid])
        .await
        .unwrap();
    assert!(matches!(imp.check_lock().await, Err(Error::LockLost)));
    assert!(matches!(pass::pass(&imp, None).await, Err(Error::LockLost)));
    let none: BTreeSet<Uuid> = BTreeSet::new();
    assert!(matches!(pass::pass(&imp, Some(&none)).await, Err(Error::LockLost)));
    let created: i64 = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from conversations", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(created, 0, "nothing written once the lock was gone");
}

#[tokio::test]
async fn the_old_databases_are_read_only_outside_the_export() {
    let w = World::new("read_only").await;
    let ka = w.person(ALICE).await;
    let ro =
        aviary_pg::connect_read_only(&w.cloudcli_db.url, "magpie-import-test", 1, Duration::from_secs(30)).unwrap();
    let c = ro.get().await.unwrap();
    let e = c
        .batch_execute(&format!(
            "insert into cc_{ka}.users (username) values ('x@example.com')"
        ))
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        Some(&aviary_pg::reexports::tokio_postgres::error::SqlState::READ_ONLY_SQL_TRANSACTION),
        "{e}"
    );
    let n: i64 = c
        .query_one(&format!("select count(*) from cc_{ka}.users"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}

#[tokio::test]
async fn a_cleared_title_becomes_provisional_and_odd_names_are_errors() {
    let w = World::new("title_names").await;
    let ka = w.person(ALICE).await;
    let (u1, p1, _, _) = one_conversation(&w, &ka).await;
    pass::pass(&w.imp, None).await.unwrap();
    w.cloudcli(&format!(
        "update cc_{ka}.sessions set custom_name = null, name_source = null where session_id = '{u1}'"
    ))
    .await;
    pass::pass(&w.imp, None).await.unwrap();
    let c = w.conv(u1).await;
    assert_eq!(c["title"], "Report");
    assert_eq!(c["nameSource"], serde_json::Value::Null);
    // a file whose name is not UTF-8 is the conversation's error, never skipped without a word
    let dir = w.claude(&ka).join(format!("projects/-workspace/{p1}/tool-results"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(std::ffi::OsStr::from_bytes(b"bad\xff.txt")), b"x").unwrap();
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(
        r.errors
            .iter()
            .any(|(id, e)| *id == u1.to_string() && e.contains("not UTF-8")),
        "{:?}",
        r.errors
    );
}
