//! Shares (cutover 7.4.4, 7.12 item 1's share fixtures): a frozen transcript that is a byte prefix of the live one,
//! one that is not, a share revoked before the import, one revoked after it, one of a conversation that is gone,
//! and `reset` taking them with their conversations.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use common::{World, assistant, user, user_image};
use magpie_import::{control, pass, plan};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

fn share_id(c: char) -> String {
    std::iter::repeat_n(c, 22).collect()
}

async fn share(w: &World, id: &str) -> serde_json::Value {
    w.magpie()
        .get()
        .await
        .unwrap()
        .query_opt(
            "select jsonb_build_object('conversation', conversation_id, 'owner', owner, 'ownerName', owner_name,
                    'title', title, 'upto', upto_seq, 'revoked', revoked_at is not null, 'reason', revoke_reason,
                    'workspace', workspace_id, 'psid', provider_session_id,
                    'rows', (select count(*) from share_rows r where r.share_id = s.id),
                    'files', (select count(*) from cc_files f where f.scope = s.snapshot),
                    'refs', (select count(*) from blob_refs b where b.owner = s.snapshot))
             from shares s where id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .map_or(serde_json::Value::Null, |r| r.get(0))
}

/// The snapshot's main transcript: its size and each line's (offset, sha256 hex).
async fn snapshot_main(w: &World, id: &str) -> (i64, Vec<(i64, String)>) {
    let c = w.magpie().get().await.unwrap();
    let size: i64 = c
        .query_one(
            "select f.size from shares s join cc_files f on f.scope = s.snapshot
             where s.id = $1 and f.path = 'projects/-workspace/' || s.provider_session_id || '.jsonl'",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    let lines = c
        .query(
            "select l.start_off, encode(l.sha256, 'hex') from shares s join cc_files f on f.scope = s.snapshot
             join cc_lines l on l.file_id = f.id
             where s.id = $1 and f.path = 'projects/-workspace/' || s.provider_session_id || '.jsonl'
             order by l.ord",
            &[&id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    (size, lines)
}

fn lines_of(b: &[u8]) -> Vec<(i64, String)> {
    let mut at = 0i64;
    b.split_inclusive(|x| *x == b'\n')
        .map(|l| {
            let r = (at, magpie_import::sha_hex(l));
            at += i64::try_from(l.len()).unwrap();
            r
        })
        .collect()
}

#[tokio::test]
async fn prefix_non_prefix_revoked_and_orphaned_shares() {
    let w = World::new("shares").await;
    let ka = w.person(ALICE).await;
    let (u1, p1) = (Uuid::new_v4(), Uuid::new_v4());
    let gone = Uuid::new_v4();
    w.cloudcli(&format!(
        "insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, name_source, created_at,
                                       updated_at)
           values ('{u1}', '{p1}', 'Report', 'user', '2026-10-28 02:55:00', '2026-10-28 03:10:00');"
    ))
    .await;
    let png: Vec<u8> = (0..5000u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
    let first = [
        user("a1", "2026-10-28T02:55:28.000Z", "find the news"),
        user_image("a2", "2026-10-28T02:56:00.000Z", &png),
        assistant("a3", "2026-10-28T02:57:00.000Z", "here are the clippings"),
    ]
    .concat();
    let second = [
        user("a4", "2026-10-28T03:05:00.000Z", "now in English"),
        assistant("a5", "2026-10-28T03:06:00.000Z", "done"),
    ]
    .concat();
    let live = [first.clone(), second.clone()].concat();
    w.put(&ka, &format!("projects/-workspace/{p1}.jsonl"), &live);

    let (pre, other, early, late, orphan) = (
        share_id('P'),
        share_id('N'),
        share_id('E'),
        share_id('L'),
        share_id('O'),
    );
    // a prefix of the live transcript (the first exchange)
    w.share(&pre, ALICE, u1, &first, false).await;
    // not a prefix: the second prompt differs (a rewrite after the freeze)
    let changed = [first.clone(), user("a4", "2026-10-28T03:05:00.000Z", "now in French")].concat();
    w.share(&other, ALICE, u1, &changed, false).await;
    // revoked before the import
    w.share(&early, ALICE, u1, &first, true).await;
    // revoked after the import
    w.share(&late, ALICE, u1, &live, false).await;
    // of a conversation that has no sessions row
    w.share(&orphan, ALICE, gone, &first, false).await;

    let p = plan::plan(&w.imp).await.unwrap();
    assert!(p.blockers.is_empty(), "{:?}", p.blockers);
    assert_eq!(p.shares.live, 4);
    assert_eq!(p.shares.revoked, 1);
    assert_eq!(p.shares.orphaned, 1);

    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.shares.frozen, 3, "{:?}", r.shares);
    assert_eq!(r.shares.not_prefix, vec![other.clone()]);
    assert_eq!(r.shares.revoked, 2);

    let kinds = w.kinds(u1).await;
    // the events of the first exchange end where the second prompt's begin
    let second_prompt = 1 + kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| *k == "cc.user")
        .nth(2)
        .unwrap()
        .0;
    let head = i64::try_from(kinds.len()).unwrap();
    let second_seq = i64::try_from(second_prompt).unwrap();

    let s = share(&w, &pre).await;
    assert_eq!(s["conversation"], u1.to_string());
    assert_eq!(s["owner"], ALICE);
    assert_eq!(s["ownerName"], "alice");
    assert_eq!(s["title"], "Shared");
    assert_eq!(s["revoked"], false);
    assert_eq!(s["psid"], p1.to_string());
    assert_eq!(s["upto"], second_seq - 1);
    assert_eq!(s["workspace"], magpie::shares::workspace_of(&pre).to_string());
    assert!(s["rows"].as_i64().unwrap() >= 2, "{s}");
    // the share holds its own reference to every blob the conversation references
    let conv_refs: i64 = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from blob_refs where owner = $1", &[&u1])
        .await
        .unwrap()
        .get(0);
    assert_eq!(s["refs"], conv_refs, "{s}");
    // the snapshot's transcript is the frozen file, line for line
    let (size, lines) = snapshot_main(&w, &pre).await;
    assert_eq!(size, i64::try_from(first.len()).unwrap());
    assert_eq!(lines, lines_of(&first));

    // the non-prefix share keeps the common lines
    let s = share(&w, &other).await;
    assert_eq!(s["revoked"], false);
    assert_eq!(s["upto"], second_seq - 1);
    let (size, lines) = snapshot_main(&w, &other).await;
    assert_eq!(size, i64::try_from(first.len()).unwrap());
    assert_eq!(lines, lines_of(&first));

    // the whole transcript: every event
    let s = share(&w, &late).await;
    assert_eq!(s["upto"], head);
    assert_eq!(snapshot_main(&w, &late).await.1, lines_of(&live));

    // revoked before: no data
    let s = share(&w, &early).await;
    assert_eq!(s["revoked"], true);
    assert_eq!(s["reason"], "owner");
    assert_eq!(s["rows"], 0);
    assert_eq!(s["files"], 0);
    // of a conversation that is gone: revoked, as the old stack answers 404 for it
    let s = share(&w, &orphan).await;
    assert_eq!(s["revoked"], true);
    assert_eq!(s["reason"], "deleted");
    assert_eq!(s["conversation"], serde_json::Value::Null);

    // shares are served by magpie's own view
    let person = magpie::conversations::Person {
        email: "bob@example.com".into(),
        viewer: None,
    };
    let view = magpie::shares::view(&w.magpie().get().await.unwrap(), &person, &pre)
        .await
        .unwrap();
    assert!(!view.is_owner);
    assert_eq!(view.owner_name, "alice");
    assert!(
        magpie::shares::view(&w.magpie().get().await.unwrap(), &person, &early)
            .await
            .is_err()
    );

    // a pass with nothing changed writes nothing; a revoke on the old side reaches magpie on the next pass
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert_eq!(
        r.shares.frozen + r.shares.revoked + r.shares.updated,
        0,
        "{:?}",
        r.shares
    );
    w.revoke_share(&late).await;
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.shares.revoked, 1);
    let s = share(&w, &late).await;
    assert_eq!(s["revoked"], true);
    assert_eq!(s["rows"], 0);
    assert_eq!(s["files"], 0);
    assert_eq!(s["refs"], 0);

    // reset takes the imported shares with their conversations; a run imports them again
    control::reset(&w.imp).await.unwrap();
    assert_eq!(share(&w, &pre).await, serde_json::Value::Null);
    let r = pass::pass(&w.imp, None).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.shares.frozen, 2);
    assert_eq!(share(&w, &pre).await["upto"], second_seq - 1);
}
