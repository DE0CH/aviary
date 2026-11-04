//! The hand-over loop (cutover 7.12 item 5): a fake Kubernetes API (ui pods, one Terminating; `pods/exec` answering
//! `holds.js` with each pod's children; `pods/proxy` answering the switch module's `/__jarvis/sockets`), a fake
//! `jarvis.sandboxes`, and real magpie and `cloudcli` databases. A conversation is released only under every condition
//! of DESIGN 9.4; a running one is never released while its idle sibling is; a schema with an unanswered child releases
//! nothing; a missing or unclean report keeps the gate unset; the running mirror follows the reads.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

mod common;

use std::sync::{Arc, Mutex};

use common::old::{Fake, Old, serve};
use common::{World, assistant, user};
use magpie_import::control;
use magpie_import::handover::{self, Handover, Wait};
use magpie_import::kube::Kube;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

async fn conversation(w: &World, ka: &str, title: &str) -> Uuid {
    let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, name_source, created_at,
                                       updated_at)
           values ('{u}', '{p}', '{title}', 'user', '2026-10-22 06:22:36', '2026-10-22T07:00:00.000Z');"
    ))
    .await;
    let main = format!("projects/-workspace/{p}.jsonl");
    w.put(
        ka,
        &main,
        &[
            user("a1", "2026-10-22T06:22:40.000Z", "write the report"),
            assistant("a2", "2026-10-22T06:23:00.000Z", "here it is"),
        ]
        .concat(),
    );
    u
}

async fn released(w: &World, id: Uuid) -> Option<String> {
    let r = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select s.release_reason, c.migrating from import_sources s join conversations c on c.id = s.conversation_id
             where s.conversation_id = $1",
            &[&id],
        )
        .await
        .unwrap();
    let reason: Option<String> = r.get(0);
    assert_eq!(
        reason.is_some(),
        !r.get::<_, bool>(1),
        "released_at and migrating agree"
    );
    reason
}

async fn run_state(w: &World, id: Uuid) -> String {
    w.magpie()
        .get()
        .await
        .unwrap()
        .query_one("select run_state from conversations where id = $1", &[&id])
        .await
        .unwrap()
        .get(0)
}

async fn waits(h: &Handover, id: Uuid) -> Option<Wait> {
    h.status
        .read()
        .await
        .open
        .iter()
        .find(|o| o.conversation == id)
        .map(|o| o.waiting_for)
}

#[tokio::test]
async fn the_loop_releases_only_under_every_condition() {
    let w = World::new("loop_release").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let schema = format!("cc_{ka}");
    let idle = conversation(&w, &ka, "Idle").await;
    let busy = conversation(&w, &ka, "Busy").await;
    let fake: Fake = Arc::new(Mutex::new(Old::default()));
    {
        let mut o = fake.lock().unwrap();
        o.pods = vec![("ui-a".into(), false), ("ui-b".into(), true)];
        o.children
            .insert("ui-a".into(), vec![(schema.clone(), true, vec![busy.to_string()])]);
        o.children.insert("ui-b".into(), vec![]);
        o.reports.insert("ui-a".into(), (0, 0));
        // ui-b (Terminating) has no report yet
    }
    let base = serve(fake.clone()).await;
    let kube = Kube::new(&base, "t0ken", None).unwrap();
    let mut h = Handover::new(kube.clone(), "sw".into());

    // before the gate: imported, verified, mirrored, never released
    for _ in 0..3 {
        h.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, idle).await, None);
    assert_eq!(waits(&h, idle).await, Some(Wait::Gate));
    assert_eq!(run_state(&w, busy).await, "running");
    assert_eq!(run_state(&w, idle).await, "idle");

    // the gate refuses while a pod (the Terminating one) has no report
    let (check, set) = handover::allow(&w.imp, &kube, "sw", None).await.unwrap();
    assert!(
        !set && check.problems().iter().any(|p| p.starts_with("ui-b")),
        "{:?}",
        check.problems()
    );
    fake.lock().unwrap().reports.insert("ui-b".into(), (0, 0));
    assert!(handover::allow(&w.imp, &kube, "sw", None).await.unwrap().1);

    // a socket that is not LU on any pod holds every release
    fake.lock().unwrap().reports.insert("ui-a".into(), (1, 0));
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None);
    assert_eq!(waits(&h, idle).await, Some(Wait::Gate));
    // HTTP that is not LU in flight, the same
    fake.lock().unwrap().reports.insert("ui-a".into(), (0, 1));
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None);
    fake.lock().unwrap().reports.insert("ui-a".into(), (0, 0));

    // an old sandbox not gone holds it
    w.jarvis(&format!(
        "insert into sandboxes values ('{}', '{idle}', 'ready')",
        Uuid::new_v4()
    ))
    .await;
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None);
    assert_eq!(waits(&h, idle).await, Some(Wait::OldSandbox));
    w.jarvis("update sandboxes set status = 'gone'").await;

    // a child of the schema that did not answer holds both
    fake.lock()
        .unwrap()
        .children
        .insert("ui-b".into(), vec![(schema.clone(), false, vec![])]);
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None);
    assert_eq!(waits(&h, idle).await, Some(Wait::HoldsUnanswered));
    fake.lock().unwrap().children.insert("ui-b".into(), vec![]);

    // every condition: the idle one goes (after two clean reads); its running sibling stays
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None, "one clean read is not two");
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await.as_deref(), Some("idle"));
    assert_eq!(released(&w, busy).await, None);
    assert_eq!(waits(&h, busy).await, Some(Wait::Running));
    assert_eq!(h.status.read().await.released_total, 1);

    // it stops running: two reads later it goes too
    fake.lock()
        .unwrap()
        .children
        .insert("ui-a".into(), vec![(schema.clone(), true, vec![])]);
    h.pass(&w.imp).await.unwrap();
    assert_eq!(run_state(&w, busy).await, "idle");
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, busy).await.as_deref(), Some("idle"));
}

#[tokio::test]
async fn a_source_change_inside_the_fence_window_keeps_it_legacy() {
    let w = World::new("loop_change").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let id = conversation(&w, &ka, "Changing").await;
    let fake: Fake = Arc::new(Mutex::new(Old::default()));
    {
        let mut o = fake.lock().unwrap();
        o.pods = vec![("ui-a".into(), false)];
        o.reports.insert("ui-a".into(), (0, 0));
    }
    let kube = Kube::new(&serve(fake.clone()).await, "t0ken", None).unwrap();
    let mut h = Handover::new(kube.clone(), "sw".into());
    h.pass(&w.imp).await.unwrap();
    assert!(handover::allow(&w.imp, &kube, "sw", None).await.unwrap().1);
    // the old side writes between passes: the pass syncs it, verification is stale, nothing is released
    let p: String = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select provider_session from import_sources where conversation_id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    w.append(
        &ka,
        &format!("projects/-workspace/{p}.jsonl"),
        &user("a3", "2026-10-22T06:30:00.000Z", "one more"),
    );
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, id).await, None);
    // stable again: re-verified, then released
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, id).await.as_deref(), Some("idle"));
}

const BOB: &str = "bob@example.com";

#[tokio::test]
async fn a_person_gate_releases_that_persons_conversations_only() {
    let w = World::new("loop_person").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let kb = w.person(BOB).await;
    let a = conversation(&w, &ka, "Alice first").await;
    let b = conversation(&w, &kb, "Bob first").await;
    let (fake, kube) = common::old::cluster(&["ui-a"]).await;
    // Bob still has an old-path socket: the gate for everyone is refused, Alice's own is not
    fake.lock().unwrap().reports.insert("ui-a".into(), (1, 0));
    fake.lock()
        .unwrap()
        .persons
        .insert("ui-a".into(), [(BOB.to_owned(), (1, 0))].into());
    let mut h = Handover::new(kube.clone(), "sw".into());
    h.pass(&w.imp).await.unwrap();
    assert!(!handover::allow(&w.imp, &kube, "sw", None).await.unwrap().1);
    // Alice with a socket of her own: refused too
    fake.lock()
        .unwrap()
        .persons
        .get_mut("ui-a")
        .unwrap()
        .insert(ALICE.to_owned(), (0, 1));
    let (check, set) = handover::allow(&w.imp, &kube, "sw", Some(ALICE)).await.unwrap();
    assert!(!set && check.problems()[0].contains("HTTP"), "{:?}", check.problems());
    fake.lock().unwrap().persons.get_mut("ui-a").unwrap().remove(ALICE);
    assert!(handover::allow(&w.imp, &kube, "sw", Some(ALICE)).await.unwrap().1);
    let st = control::status(&w.imp).await.unwrap();
    assert_eq!(st.person_gates, vec![ka.clone()]);
    assert_eq!(st.phase, "handover");
    // two clean reads: Alice's goes, Bob's waits for the gate
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, a).await.as_deref(), Some("idle"));
    assert_eq!(released(&w, b).await, None);
    assert_eq!(waits(&h, b).await, Some(Wait::Gate));
    // the per-person gate is judged every pass: none of Alice's is released while a socket of hers shows up
    let a2 = conversation(&w, &ka, "Alice second").await;
    fake.lock()
        .unwrap()
        .persons
        .get_mut("ui-a")
        .unwrap()
        .insert(ALICE.to_owned(), (1, 0));
    for _ in 0..3 {
        h.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, a2).await, None);
    assert_eq!(waits(&h, a2).await, Some(Wait::Gate));
    let gates = h.status.read().await.person_gates.clone();
    assert!(gates.len() == 1 && !gates[0].clean, "{gates:?}");
}

async fn fenced(w: &World, id: Uuid) -> bool {
    w.magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select legacy_fence_at is not null from conversations where id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0)
}

/// Runs leader A's passes until it holds a fence (its release paused inside it), then kills it (the task is
/// dropped where it stands: nothing after the fence runs, as with `kill -9`).
async fn kill_inside_the_fence(w: &World, kube: &Kube, id: Uuid) {
    // one read here, the second in the slow pass, which fences
    let mut a = Handover::new(kube.clone(), "sw".into());
    a.pass(&w.imp).await.unwrap();
    let slow = magpie_import::Importer {
        pause_in_fence: std::time::Duration::from_secs(30),
        ..w.imp.clone()
    };
    let task = tokio::spawn(async move { a.pass(&slow).await });
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !fenced(w, id).await {
        assert!(std::time::Instant::now() < end, "leader A never fenced");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(fenced(w, id).await, "the fence is left behind");
}

#[tokio::test]
async fn a_leader_killed_between_fence_and_release_is_finished_by_the_next() {
    let w = World::new("loop_kill").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let (_fake, kube) = common::old::cluster(&["ui-a"]).await;
    let mut warm = Handover::new(kube.clone(), "sw".into());
    let id = conversation(&w, &ka, "Killed").await;
    warm.pass(&w.imp).await.unwrap();
    assert!(handover::allow(&w.imp, &kube, "sw", None).await.unwrap().1);

    // 1. the other replica takes over within the lease: it re-stats and releases
    kill_inside_the_fence(&w, &kube, id).await;
    let mut b = Handover::new(kube.clone(), "sw".into());
    let rec = handover::recover(&w.imp, &mut magpie_import::uploads::Seen::new())
        .await
        .unwrap();
    assert_eq!(rec.released, 1, "{rec:?}");
    assert_eq!(released(&w, id).await.as_deref(), Some("idle"));
    assert!(!fenced(&w, id).await);
    b.pass(&w.imp).await.unwrap();

    // 2. the source changed while the fence was left: the next leader lifts it, and releases only once re-verified
    let id2 = conversation(&w, &ka, "Changed").await;
    b.pass(&w.imp).await.unwrap();
    kill_inside_the_fence(&w, &kube, id2).await;
    let p: String = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select provider_session from import_sources where conversation_id = $1",
            &[&id2],
        )
        .await
        .unwrap()
        .get(0);
    w.append(
        &ka,
        &format!("projects/-workspace/{p}.jsonl"),
        &user("c3", "2026-10-22T06:40:00.000Z", "and one more"),
    );
    let mut c = Handover::new(kube.clone(), "sw".into());
    c.pass(&w.imp).await.unwrap();
    assert!(!fenced(&w, id2).await, "unfenced");
    assert_eq!(released(&w, id2).await, None);
    for _ in 0..3 {
        c.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, id2).await.as_deref(), Some("idle"));

    // 3. nobody took over within the lease: the fence lapsed, the next leader clears it and goes the normal way
    let id3 = conversation(&w, &ka, "Lapsed").await;
    c.pass(&w.imp).await.unwrap();
    kill_inside_the_fence(&w, &kube, id3).await;
    tokio::time::sleep(magpie::limits::FENCE_LEASE + std::time::Duration::from_millis(200)).await;
    let rec = handover::recover(&w.imp, &mut magpie_import::uploads::Seen::new())
        .await
        .unwrap();
    assert_eq!(rec.lapsed, 1, "{rec:?}");
    assert!(!fenced(&w, id3).await);
    let mut d = Handover::new(kube.clone(), "sw".into());
    for _ in 0..3 {
        d.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, id3).await.as_deref(), Some("idle"));
}

/// The loop checks released conversations live (DESIGN 9.9's `import_verify_mismatch_total` on the hand-over's own
/// registry, not only the one-shot `verify`'s): a write the old side makes to a released conversation is counted once
/// per change, a stat that changed with the same bytes is not a mismatch, and the shared memory is not checked.
#[tokio::test]
async fn the_loop_counts_an_old_side_write_to_a_released_conversation_once() {
    let w = World::new("loop_released").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let id = conversation(&w, &ka, "Released").await;
    let (_fake, kube) = common::old::cluster(&["ui-a"]).await;
    let mut h = Handover::new(kube.clone(), "sw".into());
    h.pass(&w.imp).await.unwrap();
    assert!(handover::allow(&w.imp, &kube, "sw", None).await.unwrap().1);
    for _ in 0..3 {
        h.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, id).await.as_deref(), Some("idle"));
    let p: String = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select provider_session from import_sources where conversation_id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    let main = format!("projects/-workspace/{p}.jsonl");
    let before = w.imp.magpie.legacy.verify_mismatches();
    // nothing written: nothing checked
    h.pass(&w.imp).await.unwrap();
    assert_eq!(h.status.read().await.pass.released_changed, 0);
    // the person's shared memory changes (their legacy conversations rightly write it): not checked
    w.put(&ka, "projects/-workspace/memory/MEMORY.md", b"- later\n");
    h.pass(&w.imp).await.unwrap();
    assert_eq!(h.status.read().await.pass.released_changed, 0);
    // the old side appends to the released transcript: counted at the next pass, once
    w.append(&ka, &main, &assistant("z9", "2026-10-22T08:00:00.000Z", "too late"));
    h.pass(&w.imp).await.unwrap();
    assert_eq!(h.status.read().await.pass.released_changed, 1);
    assert_eq!(w.imp.magpie.legacy.verify_mismatches(), before + 1);
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(
        w.imp.magpie.legacy.verify_mismatches(),
        before + 1,
        "a change counted once"
    );
    // a new file in its session folder: the stats change again, checked again (the transcript and the new file)
    w.put(
        &ka,
        &format!("projects/-workspace/{p}/tool-results/late.txt"),
        b"late\n",
    );
    h.pass(&w.imp).await.unwrap();
    assert_eq!(w.imp.magpie.legacy.verify_mismatches(), before + 3);
}

/// A released conversation whose file only changed its stat (the same bytes rewritten) is checked and not counted.
#[tokio::test]
async fn a_released_conversation_rewritten_with_the_same_bytes_is_no_mismatch() {
    let w = World::new("loop_released_same").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let id = conversation(&w, &ka, "Same").await;
    let (_fake, kube) = common::old::cluster(&["ui-a"]).await;
    let mut h = Handover::new(kube.clone(), "sw".into());
    h.pass(&w.imp).await.unwrap();
    assert!(handover::allow(&w.imp, &kube, "sw", None).await.unwrap().1);
    for _ in 0..3 {
        h.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, id).await.as_deref(), Some("idle"));
    let p: String = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select provider_session from import_sources where conversation_id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    let main = format!("projects/-workspace/{p}.jsonl");
    let before = w.imp.magpie.legacy.verify_mismatches();
    let same = [
        user("a1", "2026-10-22T06:22:40.000Z", "write the report"),
        assistant("a2", "2026-10-22T06:23:00.000Z", "here it is"),
    ]
    .concat();
    std::thread::sleep(std::time::Duration::from_millis(20));
    w.put(&ka, &main, &same);
    h.pass(&w.imp).await.unwrap();
    assert_eq!(h.status.read().await.pass.released_changed, 1);
    assert_eq!(w.imp.magpie.legacy.verify_mismatches(), before);
}

/// The released check stats each recorded file where its record says it is: a released conversation whose recorded
/// session id does not name its transcript (the old row's provider session missing: the box's smoke conversations)
/// still has a write to that transcript counted.
#[tokio::test]
async fn a_write_to_a_recorded_file_counts_whatever_its_session_id() {
    let w = World::new("loop_released_src").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let id = conversation(&w, &ka, "Elsewhere").await;
    let (_fake, kube) = common::old::cluster(&["ui-a"]).await;
    let mut h = Handover::new(kube.clone(), "sw".into());
    h.pass(&w.imp).await.unwrap();
    assert!(handover::allow(&w.imp, &kube, "sw", None).await.unwrap().1);
    for _ in 0..3 {
        h.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, id).await.as_deref(), Some("idle"));
    let db = w.magpie().get().await.unwrap();
    let p: String = db
        .query_one(
            "select provider_session from import_sources where conversation_id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    db.execute(
        "update import_sources set provider_session = $2 where conversation_id = $1",
        &[&id, &Uuid::new_v4().to_string()],
    )
    .await
    .unwrap();
    let before = w.imp.magpie.legacy.verify_mismatches();
    // the transcript is where the record says, not where the session id points: a stat difference, the same bytes
    h.pass(&w.imp).await.unwrap();
    assert_eq!(w.imp.magpie.legacy.verify_mismatches(), before);
    w.append(
        &ka,
        &format!("projects/-workspace/{p}.jsonl"),
        &assistant("z9", "2026-10-22T08:00:00.000Z", "too late"),
    );
    h.pass(&w.imp).await.unwrap();
    assert_eq!(w.imp.magpie.legacy.verify_mismatches(), before + 1);
}
