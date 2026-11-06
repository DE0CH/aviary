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
    // a restart or another leader: the same write is not counted again
    let mut h2 = Handover::new(kube.clone(), "sw".into());
    h2.pass(&w.imp).await.unwrap();
    assert_eq!(h2.status.read().await.pass.released_changed, 0);
    assert_eq!(
        w.imp.magpie.legacy.verify_mismatches(),
        before + 1,
        "not counted again by a new process"
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

/// DECISIONS 17 (the old stack's parked questions, claudecodeui fd1b3ed5): a conversation whose question waits
/// unanswered in `jarvis_open_questions` (its transcript ends with that `tool_use` dangling, its old sandbox gone)
/// is handed over with its question: a parked turn whose open ask is keyed by the `tool_use_id`, and the person's
/// answer on the new stack admits it again with a `start_turn` that resumes the session and carries the answer. An
/// answered question whose turn has not run keeps its conversation legacy.
#[tokio::test]
async fn a_parked_old_question_is_carried_over_and_an_answered_one_waits() {
    let w = World::new("loop_questions").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let schema = format!("cc_{ka}");
    let asking = conversation(&w, &ka, "Asking").await;
    let answered = conversation(&w, &ka, "Answered").await;
    let input = r#"{"questions":[{"question":"Which?","header":"Which","multiSelect":false,"options":[{"label":"A","description":"a"},{"label":"B","description":"b"}]}]}"#;
    w.cloudcli(&format!(
        "insert into {schema}.jarvis_open_questions
           (request_id, session_id, tool_name, tool_use_id, input, run_options, asked_at, answer) values
           ('req-1', '{asking}', 'AskUserQuestion', 'toolu_q1', '{input}', '{{}}', '2026-10-22T07:10:00.000Z', null),
           ('req-2', '{answered}', 'AskUserQuestion', 'toolu_q2', '{input}', '{{}}', '2026-10-22T07:10:00.000Z',
            '{{\"allow\":true,\"updatedInput\":{{\"answers\":{{\"Which?\":\"B\"}}}}}}')"
    ))
    .await;
    let fake: Fake = Arc::new(Mutex::new(Old::default()));
    {
        let mut o = fake.lock().unwrap();
        o.pods = vec![("ui-a".into(), false)];
        // the busy route lists neither: an unanswered question holds nothing, and the answered one is left out here
        // so that the importer's own check of the table is what keeps it
        o.children.insert("ui-a".into(), vec![(schema.clone(), true, vec![])]);
        o.reports.insert("ui-a".into(), (0, 0));
    }
    let base = serve(fake.clone()).await;
    let kube = Kube::new(&base, "t0ken", None).unwrap();
    let mut h = Handover::new(kube.clone(), "sw".into());
    h.pass(&w.imp).await.unwrap();
    assert!(handover::allow(&w.imp, &kube, "sw", None).await.unwrap().1);
    for _ in 0..3 {
        h.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, asking).await.as_deref(), Some("idle"));
    assert_eq!(
        released(&w, answered).await,
        None,
        "an answered question waits for its turn"
    );

    // the question is magpie's now: open, on the parked turn, keyed by its tool_use_id
    let db = w.magpie().get().await.unwrap();
    let c = db
        .query_one(
            "select c.run_state, t.state, t.id from conversations c join turns t on t.id = c.active_turn
             where c.id = $1",
            &[&asking],
        )
        .await
        .unwrap();
    assert_eq!(
        (c.get::<_, String>(0).as_str(), c.get::<_, String>(1).as_str()),
        ("running", "parked")
    );
    let turn: Uuid = c.get(2);
    let ask = db
        .query_one(
            "select data from events where conversation_id = $1 and kind = 'ask.pending'",
            &[&asking],
        )
        .await
        .unwrap()
        .get::<_, serde_json::Value>(0);
    assert_eq!(ask["askId"], "req-1");
    assert_eq!(ask["toolUseId"], "toolu_q1");
    assert_eq!(ask["input"]["questions"][0]["question"], "Which?");

    // the person answers on the new stack: the turn is admitted again with the answer
    let person = magpie::conversations::Person {
        email: ALICE.into(),
        viewer: None,
    };
    let answer = magpie_api::turns::AgentPermissionResponse {
        behavior: magpie_api::turns::Behavior::Allow,
        selected_action_id: None,
        updated_input: Some(serde_json::from_str(r#"{"answers":{"Which?":"A"}}"#).unwrap()),
        updated_permissions: None,
        message: None,
        interrupt: None,
    };
    let resumed = magpie::turns::cancel::answer(&w.imp.magpie.pool, &person, asking, "req-1", answer)
        .await
        .unwrap();
    assert_eq!(resumed, Some(turn));
    let start = db
        .query_one(
            "select payload from commands where conversation_id = $1 and type = 'start_turn' and turn_id = $2",
            &[&asking, &turn],
        )
        .await
        .unwrap()
        .get::<_, serde_json::Value>(0);
    assert_eq!(start["answers"][0]["toolUseId"], "toolu_q1");
    assert_eq!(start["answers"][0]["updatedInput"]["answers"]["Which?"], "A");
    assert!(start["resume"]["providerSessionId"].is_string(), "{start}");
}

/// The old router's schedule tables (claudecodeui `jarvis/schedules.mjs`).
const OLD_SCHEDULES: &str = "
create schema if not exists cc_shared;
create table cc_shared.schedules (id text primary key, person text not null, conversation_id text not null,
  kind text not null, prompt text not null, at timestamptz, cron text, tz text not null, until timestamptz,
  describe text not null, next_at timestamptz, state text not null, created_at timestamptz not null default now(),
  created_by text not null, updated_at timestamptz not null default now(), deleted_at timestamptz);
create table cc_shared.schedule_runs (schedule_id text not null, due_at timestamptz not null,
  at timestamptz not null default now(), outcome text not null, conversation_id text not null, detail text,
  primary key (schedule_id, due_at));";

/// One active repeating schedule of conversation `c` (`sch_<tag>`, next fire in an hour) with one run, `ago` before.
async fn old_schedule(w: &World, tag: &str, c: Uuid, ago: &str) {
    w.cloudcli(&format!(
        "insert into cc_shared.schedules (id, person, conversation_id, kind, prompt, cron, tz, describe, next_at, state,
                                          created_by)
           values ('sch_{tag}', '{ALICE}', '{c}', 'cron', 'report', '0 9 * * *', 'Asia/Hong_Kong', '每日 09:00',
                   date_trunc('second', now()) + interval '1 hour', 'active', '{ALICE}');
         insert into cc_shared.schedule_runs (schedule_id, due_at, at, outcome, conversation_id)
           values ('sch_{tag}', now() - interval '{ago}', now() - interval '{ago}', 'sent', '{c}');"
    ))
    .await;
}

/// The old row's state and next time; magpie's copy's next time (`None`: magpie has no row).
async fn schedule_state(
    w: &World,
    tag: &str,
) -> (
    String,
    Option<time::OffsetDateTime>,
    Option<Option<time::OffsetDateTime>>,
) {
    let old = w
        .imp
        .cloudcli
        .get()
        .await
        .unwrap()
        .query_one(
            "select state, next_at from cc_shared.schedules where id = $1",
            &[&format!("sch_{tag}")],
        )
        .await
        .unwrap();
    let new = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_opt("select next_at from schedules where id = $1", &[&format!("sch_{tag}")])
        .await
        .unwrap()
        .map(|r| r.get(0));
    (old.get(0), old.get(1), new)
}

/// A conversation's old schedules move at its release with no fire made by both stacks or by neither (2026-11-06):
/// the hand-over process, whose other connections to the old database are read-only, takes the rows on its one
/// writable connection around magpie's release (marked `moved` there, carried with the same next time); a schedule
/// the old router fired within the quiet period, or is firing at that moment (its row locked), keeps the
/// conversation legacy with the rows active and the fire's next time, and a later pass carries that next time.
#[tokio::test]
async fn old_schedules_move_at_the_release_and_a_fire_keeps_the_conversation_legacy() {
    let w = World::new("loop_schedules").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    w.cloudcli(OLD_SCHEDULES).await;
    let ka = w.person(ALICE).await;
    let schema = format!("cc_{ka}");
    let quiet = conversation(&w, &ka, "Quiet").await;
    let fired = conversation(&w, &ka, "Fired").await;
    let firing = conversation(&w, &ka, "Firing").await;
    old_schedule(&w, "quiet", quiet, "1 day").await;
    old_schedule(&w, "fired", fired, "20 seconds").await;
    old_schedule(&w, "firing", firing, "1 day").await;
    // the hand-over process's own connections: the old database read-only but for the schedules' one
    let imp = magpie_import::Importer {
        cloudcli: aviary_pg::connect_read_only(
            &w.cloudcli_db.url,
            "magpie-import-ro",
            2,
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
        ..w.imp.clone()
    };
    let fake: Fake = Arc::new(Mutex::new(Old::default()));
    {
        let mut o = fake.lock().unwrap();
        o.pods = vec![("ui-a".into(), false)];
        o.children.insert("ui-a".into(), vec![(schema.clone(), true, vec![])]);
        o.reports.insert("ui-a".into(), (0, 0));
    }
    let base = serve(fake.clone()).await;
    let kube = Kube::new(&base, "t0ken", None).unwrap();
    let mut h = Handover::new(kube.clone(), "sw".into());
    h.pass(&imp).await.unwrap();
    let (_, firing_next, _) = schedule_state(&w, "firing").await;

    // the old router is firing `firing`'s schedule (its row locked) when the release takes the rows: the claim waits
    // for it, sees its run and leaves the conversation legacy
    let (locked_tx, locked) = tokio::sync::oneshot::channel();
    let pool = w.imp.cloudcli.clone();
    let fire = tokio::spawn(async move {
        aviary_pg::tx!(&pool, |t| {
            t.query(
                "select id from cc_shared.schedules where id = 'sch_firing' for update",
                &[],
            )
            .await?;
            let _ = locked_tx.send(());
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            t.execute(
                "insert into cc_shared.schedule_runs (schedule_id, due_at, outcome, conversation_id)
                 select id, next_at, 'sent', conversation_id from cc_shared.schedules where id = 'sch_firing'",
                &[],
            )
            .await?;
            t.execute(
                "update cc_shared.schedules set next_at = next_at + interval '1 day' where id = 'sch_firing'",
                &[],
            )
            .await?;
            Ok(())
        })
        .await
        .unwrap();
    });
    locked.await.unwrap();
    assert!(handover::allow(&imp, &kube, "sw", None).await.unwrap().1);
    for _ in 0..3 {
        h.pass(&imp).await.unwrap();
    }
    fire.await.unwrap();

    // quiet: released; its schedule is magpie's with the same next time and the old row will never fire again
    assert_eq!(released(&w, quiet).await.as_deref(), Some("idle"));
    let (state, next, new) = schedule_state(&w, "quiet").await;
    assert_eq!((state.as_str(), next), ("moved", None));
    assert!(new.flatten().is_some(), "carried with its next time");
    let runs: i64 = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from schedule_runs where schedule_id = 'sch_quiet'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(runs, 1);

    // fired 20 s ago, and firing during the release: both legacy, rows active, nothing in magpie
    for (id, tag) in [(fired, "fired"), (firing, "firing")] {
        assert_eq!(released(&w, id).await, None, "{tag}");
        let (state, _, new) = schedule_state(&w, tag).await;
        assert_eq!((state.as_str(), new), ("active", None), "{tag}");
    }
    let (_, next_after_fire, _) = schedule_state(&w, "firing").await;
    assert_eq!(
        next_after_fire,
        firing_next.map(|t| t + time::Duration::days(1)),
        "the fire moved it on"
    );

    // once the fires are past the quiet period a later pass carries them, with the fire's next time and run
    w.cloudcli("update cc_shared.schedule_runs set at = at - interval '2 minutes'")
        .await;
    for _ in 0..2 {
        h.pass(&imp).await.unwrap();
    }
    for (id, tag) in [(fired, "fired"), (firing, "firing")] {
        assert_eq!(released(&w, id).await.as_deref(), Some("idle"), "{tag}");
        let (state, _, _) = schedule_state(&w, tag).await;
        assert_eq!(state, "moved", "{tag}");
    }
    let (_, _, new) = schedule_state(&w, "firing").await;
    assert_eq!(
        new,
        Some(next_after_fire),
        "magpie fires the next time after the old fire, never that one again"
    );
}
