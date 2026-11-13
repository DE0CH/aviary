//! A person's message sent while a turn runs (src/turns/midturn.rs; Deyao, 2026-11-12): it joins the running turn
//! as a `send_message` instead of being refused, shows on the timeline under the app's
//! own message id once Claude Code's transcript says it was taken in, and is sent again as a turn of its own when
//! its turn ended first. A message with attachments joins it too, its files written by the supervisor.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::assert_is_empty,
    dead_code
)]

mod common;

use aviary_fakes::hermit::{FakeHermit, HermitConfig, Running};
use common::cc::{Sandbox, main_path, person_turn, set_main};
use magpie::turns::driver::Driver;
use magpie_api::client::ClientError;
use magpie_api::conversations::CreateRequest;
use magpie_api::errors::ErrorCode;
use magpie_api::ops::HermitSendRequest;
use magpie_api::rows::RowItem;
use magpie_api::turns::{FileRef, SendRequest};
use serde_json::{Value, json};
use uuid::Uuid;

const ALICE: &str = "alice@example.org";

struct World {
    db: common::TestDb,
    t: common::Tokens,
    hermit: Running,
    a: common::Replica,
}

impl World {
    async fn new(test: &str) -> Self {
        let db = common::fresh_db(test).await;
        let t = common::Tokens::new();
        // sandboxes never become ready: an admitted turn stays the conversation's active turn until canceled
        let hermit = FakeHermit::unguarded(HermitConfig::default()).serve().await.unwrap();
        let a = common::replica_with(&db.url, &t, &hermit.url(), common::idle()).await;
        Self { db, t, hermit, a }
    }

    fn pool(&self) -> &aviary_pg::Pool {
        self.a.pool()
    }

    async fn conversation(&self) -> Uuid {
        let key = Uuid::new_v4().to_string();
        self.a
            .pigeon()
            .person(ALICE)
            .create_conversation(&CreateRequest {
                harness: magpie_api::Harness::default(),
                model: None,
                thinking: None,
                idempotency_key: key.clone(),
                message: msg(&format!("{key}:m"), ""),
            })
            .await
            .unwrap()
            .get()
            .conversation
            .id
    }

    /// The leader's pass over the queue.
    async fn pass(&self) -> usize {
        magpie::turns::midturn::pass(&Driver::of(self.a.app())).await.unwrap()
    }

    /// `(state, turn, cmd_seq)` of the message `cmid`.
    async fn message(&self, conv: Uuid, cmid: &str) -> (String, Option<Uuid>, Option<i64>) {
        let r = self
            .pool()
            .get()
            .await
            .unwrap()
            .query_one(
                "select state, turn_id, cmd_seq from pending_messages
                 where conversation_id = $1 and client_message_id = $2",
                &[&conv, &cmid],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2))
    }

    /// Acknowledges the command `seq` as the supervisor does.
    async fn ack(&self, conv: Uuid, seq: i64, delivered: bool) {
        self.pool()
            .get()
            .await
            .unwrap()
            .execute(
                "update commands set acked_at = now(), result = $3 where conversation_id = $1 and cmd_seq = $2",
                &[&conv, &seq, &json!({"delivered": delivered})],
            )
            .await
            .unwrap();
    }

    /// The text of each `user.sent` of `conv`.
    async fn sent(&self, conv: Uuid) -> Vec<String> {
        self.pool()
            .get()
            .await
            .unwrap()
            .query(
                "select data->>'text' from events where conversation_id = $1 and kind = 'user.sent' order by seq",
                &[&conv],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect()
    }

    /// The conversation's queue as the app gets it: `(text, held)`, oldest first.
    async fn waiting(&self, conv: Uuid) -> Vec<(String, bool)> {
        self.a
            .pigeon()
            .person(ALICE)
            .get_conversation(conv)
            .await
            .unwrap()
            .pending_messages
            .into_iter()
            .map(|m| (m.text, m.held))
            .collect()
    }
}

fn msg(cmid: &str, text: &str) -> SendRequest {
    SendRequest {
        client_message_id: cmid.into(),
        text: text.into(),
        images: Vec::new(),
        files: Vec::new(),
    }
}

fn code(e: &ClientError) -> ErrorCode {
    e.code().cloned().unwrap()
}

fn line(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(v).unwrap();
    b.push(b'\n');
    b
}

#[tokio::test]
async fn a_message_sent_while_a_turn_runs_joins_it() {
    let w = World::new("midturn_join").await;
    let conv = w.conversation().await;
    let alice = w.a.pigeon();
    let alice = alice.person(ALICE);
    let busy = alice.send_turn(conv, &msg("m1", "長任務")).await.unwrap().get().turn.id;

    // the second message starts no turn: it joins the one that runs
    let r = alice.send_turn(conv, &msg("m2", "順便查埋天氣")).await.unwrap();
    assert!(r.is_new());
    let r = r.into_inner();
    assert!(r.mid_turn);
    assert_eq!(r.turn.id, busy);
    assert_eq!(w.sent(conv).await, ["長任務"], "no user.sent and no turn of its own");
    let (state, turn, seq) = w.message(conv, "m2").await;
    assert_eq!((state.as_str(), turn), ("pending", Some(busy)));
    let seq = seq.unwrap();
    let c = w.pool().get().await.unwrap();
    let cmd = c
        .query_one(
            "select type, turn_id, payload from commands where conversation_id = $1 and cmd_seq = $2",
            &[&conv, &seq],
        )
        .await
        .unwrap();
    assert_eq!(cmd.get::<_, String>(0), "send_message");
    assert_eq!(cmd.get::<_, Option<Uuid>>(1), Some(busy));
    assert_eq!(cmd.get::<_, Value>(2), json!({"text": "順便查埋天氣"}));
    assert_eq!(w.waiting(conv).await, [("順便查埋天氣".to_owned(), false)]);

    // a repeat is the same message: nothing new
    let again = alice.send_turn(conv, &msg("m2", "順便查埋天氣")).await.unwrap();
    assert!(!again.is_new());
    let again = again.into_inner();
    assert!(again.mid_turn);
    assert_eq!(again.turn.id, busy);
    let n: i64 = c
        .query_one(
            "select count(*) from commands where conversation_id = $1 and type = 'send_message'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);

    // written by the supervisor: it still waits (Claude Code has not taken it in)
    w.ack(conv, seq, true).await;
    w.pass().await;
    assert_eq!(w.message(conv, "m2").await.0, "written");
    assert_eq!(w.waiting(conv).await, [("順便查埋天氣".to_owned(), false)]);

    // a third joins too, in order
    alice.send_turn(conv, &msg("m3", "仲有交通")).await.unwrap();
    assert_eq!(
        w.waiting(conv).await,
        [("順便查埋天氣".to_owned(), false), ("仲有交通".to_owned(), false)]
    );
}

/// A message with attachments joins the running turn too (Deyao, 2026-11-13: in a turn of hours it looked hung): a
/// `send_attached` carries its files to the supervisor. One that cannot join (a supervisor that does not know the
/// command) is held in the queue, can be taken back, and starts a turn of its own once the running turn has ended;
/// what was sent after it waits behind it.
#[tokio::test]
async fn a_message_with_attachments_joins_the_running_turn_or_is_held_until_it_ends() {
    use magpie_api::uploads::UploadKind;
    let w = World::new("midturn_attached").await;
    let conv = w.conversation().await;
    let pigeon = w.a.pigeon();
    let alice = pigeon.person(ALICE);
    let busy = alice.send_turn(conv, &msg("m1", "長任務")).await.unwrap().get().turn.id;
    let with_file = async |cmid: &str, text: &str| -> SendRequest {
        let up = alice
            .upload("a.txt", UploadKind::File, "text/plain", b"hello".to_vec())
            .await
            .unwrap();
        let mut m = msg(cmid, text);
        m.files.push(FileRef {
            path: up.path,
            name: up.name,
            mime_type: up.mime_type,
            size: up.size,
        });
        m
    };
    // each `send_attached` of the conversation: `(cmd_seq, turn, text, its file's workspace path)`
    let attached = async || -> Vec<(i64, Option<Uuid>, String, String)> {
        w.pool()
            .get()
            .await
            .unwrap()
            .query(
                "select cmd_seq, turn_id, payload->>'text', payload->'files'->0->>'workspacePath' from commands
                 where conversation_id = $1 and type = 'send_attached' order by cmd_seq",
                &[&conv],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
            .collect()
    };
    let uploads = async || -> i64 {
        w.pool()
            .get()
            .await
            .unwrap()
            .query_one(
                "select count(*) from uploads where conversation_id = $1 and attached_seq is not null",
                &[&conv],
            )
            .await
            .unwrap()
            .get(0)
    };
    let m2 = with_file("m2", "睇下呢個").await;
    let r = alice.send_turn(conv, &m2).await.unwrap().into_inner();
    assert!(r.mid_turn);
    assert_eq!(r.turn.id, busy);
    // written into the running turn at once, its file with it
    let cmds = attached().await;
    assert_eq!(cmds.len(), 1);
    let (m2_seq, ref turn, ref text, ref path) = cmds[0];
    assert_eq!(
        (*turn, text.as_str(), path.as_str()),
        (Some(busy), "睇下呢個", "uploads/a.txt")
    );
    let c = alice.get_conversation(conv).await.unwrap();
    let q = &c.pending_messages;
    assert_eq!(
        (q[0].client_message_id.as_str(), q[0].attachments, q[0].held),
        ("m2", 1, false)
    );
    // its upload becomes the conversation's when Claude Code has taken the message in, not before
    assert_eq!(uploads().await, 0);
    // a second file of the same name gets a name of its own, though the first is not the conversation's yet
    let m3 = with_file("m3", "同埋呢個").await;
    alice.send_turn(conv, &m3).await.unwrap();
    let cmds = attached().await;
    assert_eq!(cmds[1].3, "uploads/a (2).txt");
    let m3_seq = cmds[1].0;

    // a supervisor that does not know the command: the message is held, and waits for the turn's end
    let c = w.pool().get().await.unwrap();
    c.execute(
        "update commands set acked_at = now(), result = $3 where conversation_id = $1 and cmd_seq = $2",
        &[&conv, &m2_seq, &json!({"error": "unsupported"})],
    )
    .await
    .unwrap();
    w.ack(conv, m3_seq, true).await;
    assert_eq!(w.pass().await, 0, "it is not written again while the turn runs");
    assert_eq!(attached().await.len(), 2);
    assert_eq!(
        w.waiting(conv).await,
        [("睇下呢個".to_owned(), true), ("同埋呢個".to_owned(), false)]
    );
    assert_eq!(w.sent(conv).await, ["長任務"]);
    // the order they were sent in is the order they arrive in (Deyao, 2026-11-13: a later text overtook an earlier
    // message with a picture): a text sent after a held message waits behind it, not written into the running turn
    alice.send_turn(conv, &msg("m4", "文字")).await.unwrap();
    let written: i64 = c
        .query_one(
            "select count(*) from commands where conversation_id = $1 and type = 'send_message'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(written, 0, "m4 waits behind the held m2");
    assert_eq!(w.pass().await, 0, "and goes on waiting while m2 does");
    // held messages have reached no Claude Code: the person can take them back; one written into the turn, not
    let e = alice.withdraw_pending(conv, "m3").await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadTransition);
    let e = alice.withdraw_pending(conv, "nothing").await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
    let m5 = with_file("m5", "唔要").await;
    alice.send_turn(conv, &m5).await.unwrap();
    assert_eq!(attached().await.len(), 2, "behind a held message it waits too");
    let after = alice.withdraw_pending(conv, "m5").await.unwrap();
    assert_eq!(after.pending_messages.len(), 3);

    // the turn ends: the held message starts a turn of its own, with its file
    alice.cancel_turn(conv).await.unwrap();
    assert_eq!(w.pass().await, 1, "one turn at a time: m2 first");
    let (state, turn, _) = w.message(conv, "m2").await;
    assert_eq!(state, "turn");
    assert_ne!(turn, Some(busy));
    assert_eq!(w.sent(conv).await, ["長任務", "睇下呢個"]);
    assert_eq!(uploads().await, 1, "its upload is attached to the new turn");
    // m3 was written into the turn that was stopped and no line of it came: sent again, it joins the new turn with
    // its file (the name it had), and the text after it
    c.execute(
        "update pending_messages set state = 'redeliver' where conversation_id = $1 and client_message_id = 'm3'",
        &[&conv],
    )
    .await
    .unwrap();
    assert_eq!(w.pass().await, 1);
    let cmds = attached().await;
    assert_eq!(cmds.len(), 3);
    assert_eq!((cmds[2].1, cmds[2].3.as_str()), (turn, "uploads/a (2).txt"));
    assert_eq!(w.pass().await, 1);
    assert_eq!(w.message(conv, "m4").await.1, turn);
    // on its way into the turn now: it cannot be taken back any more
    let e = alice.withdraw_pending(conv, "m4").await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadTransition);
}

#[tokio::test]
async fn the_deployments_and_a_view_as_message_are_refused_while_a_turn_runs_unless_the_deployments_joins() {
    let w = World::new("midturn_refused").await;
    let conv = w.conversation().await;
    let pigeon = w.a.pigeon();
    let alice = pigeon.person(ALICE);
    alice.send_turn(conv, &msg("m1", "長任務")).await.unwrap();
    // the deployment's own message (Jarvis's inbox delivers it again once the conversation is idle)
    let e =
        w.a.hermit()
            .service(None)
            .hermit_send_turn(
                conv,
                &HermitSendRequest {
                    person: ALICE.into(),
                    message: msg("wakeup-1", "Scheduled wakeup"),
                    join_running: false,
                },
            )
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::TurnRunning);
    assert!(w.waiting(conv).await.is_empty());
    // unless it says it joins the running turn (a warning to the agent that must not wait for a long turn to end):
    // then it waits in the conversation's queue as a person's message does
    let joined =
        w.a.hermit()
            .service(None)
            .hermit_send_turn(
                conv,
                &HermitSendRequest {
                    person: ALICE.into(),
                    message: msg("watchdog-1", "Jarvis watchdog: memory pressure"),
                    join_running: true,
                },
            )
            .await
            .unwrap();
    assert!(joined.get().mid_turn, "it joined the running turn");
    assert_eq!(w.waiting(conv).await.len(), 1);
    // an admin viewing as the person
    let e = pigeon
        .viewing_as(ALICE, "admin@example.org", "s1")
        .send_turn(conv, &msg("m4", "as admin"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::TurnRunning);
    assert_eq!(
        w.waiting(conv).await.len(),
        1,
        "only the deployment's joining message waits"
    );
}

#[tokio::test]
async fn a_message_its_turn_never_took_becomes_a_turn_of_its_own() {
    let w = World::new("midturn_redeliver").await;
    let conv = w.conversation().await;
    let pigeon = w.a.pigeon();
    let alice = pigeon.person(ALICE);
    let busy = alice.send_turn(conv, &msg("m1", "長任務")).await.unwrap().get().turn.id;

    // the turn ends with the command never acknowledged: canceled, and sent again as a new turn
    alice.send_turn(conv, &msg("m2", "第二個")).await.unwrap();
    let stale = w.message(conv, "m2").await.2.unwrap();
    assert_eq!(alice.cancel_turn(conv).await.unwrap().turn_id, Some(busy));
    assert_eq!(w.pass().await, 1);
    let (state, second, _) = w.message(conv, "m2").await;
    assert_eq!(state, "turn");
    let second = second.unwrap();
    assert_ne!(second, busy);
    assert_eq!(w.sent(conv).await, ["長任務", "第二個"], "now a typed message's turn");
    assert!(w.waiting(conv).await.is_empty());
    let canceled: bool = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select canceled_at is not null from commands where conversation_id = $1 and cmd_seq = $2",
            &[&conv, &stale],
        )
        .await
        .unwrap()
        .get(0);
    assert!(canceled, "the stale command is never delivered later");
    // the app's repeat of that send finds the turn it became
    let again = alice.send_turn(conv, &msg("m2", "第二個")).await.unwrap();
    assert!(!again.is_new());
    let again = again.into_inner();
    assert_eq!((again.turn.id, again.mid_turn), (second, false));

    // a negative ack (the turn was being stopped when the supervisor read the command): while a turn still runs the
    // message joins it again with a new command; once the conversation is idle it is a turn of its own
    alice.send_turn(conv, &msg("m3", "第三個")).await.unwrap();
    let seq = w.message(conv, "m3").await.2.unwrap();
    w.ack(conv, seq, false).await;
    w.pass().await;
    let (state, turn, again_seq) = w.message(conv, "m3").await;
    assert_eq!((state.as_str(), turn), ("pending", Some(second)));
    assert_ne!(again_seq, Some(seq));
    alice.cancel_turn(conv).await.unwrap();
    w.pass().await;
    assert_eq!(w.message(conv, "m3").await.0, "turn");
    assert_eq!(
        w.sent(conv).await,
        ["長任務", "第二個", "第三個"],
        "each message exactly once"
    );

    // written, then lost with its Claude Code: the conversation has been idle and no line showed it
    let third = w.message(conv, "m3").await.1.unwrap();
    alice.send_turn(conv, &msg("m4", "第四個")).await.unwrap();
    let seq = w.message(conv, "m4").await.2.unwrap();
    w.ack(conv, seq, true).await;
    alice.cancel_turn(conv).await.unwrap();
    w.pass().await;
    assert_eq!(
        w.message(conv, "m4").await.0,
        "written",
        "Claude Code may still start it right after its reply"
    );
    w.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update turns set ended_at = now() - interval '31 seconds' where id = $1",
            &[&third],
        )
        .await
        .unwrap();
    w.pass().await; // settles it as lost
    w.pass().await;
    assert_eq!(w.message(conv, "m4").await.0, "turn");
    assert_eq!(w.sent(conv).await.len(), 4);
}

/// Claude Code takes every message that waits when it next looks, as ONE prompt: their texts joined by a newline
/// (2026-11-13: two messages queued in a row came in as one line; neither was known as taken, their rows stayed in
/// the queue, and both were sent again, one by one, when the conversation went idle two hours later).
#[tokio::test]
async fn messages_claude_code_took_in_together_are_all_taken() {
    let db = common::fresh_db("midturn_taken_joined").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let mut s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    set_main(r.pool(), s.conversation, &main_path()).await;
    let conv = s.conversation;
    person_turn(r.pool(), &mut s).await;
    let c = r.pool().get().await.unwrap();
    c.execute("update conversations set run_state = 'running' where id = $1", &[&conv])
        .await
        .unwrap();
    let pigeon = r.pigeon();
    let alice = pigeon.person(ALICE);
    for (id, text) in [("m2", "Also say KIWI"), ("m3", "Then say MANGO"), ("m4", "And PLUM")] {
        alice.send_turn(conv, &msg(id, text)).await.unwrap();
    }
    // the first two in one prompt line; the third still waits
    let lines = [
        json!({"type": "user", "uuid": "u1", "message": {"role": "user", "content": "Run sleep"}}),
        json!({"type": "user", "uuid": "u2", "message": {"role": "user",
               "content": "Also say KIWI\nThen say MANGO"}}),
    ];
    let bytes: Vec<u8> = lines.iter().flat_map(line).collect();
    s.write(&main_path(), 0, &bytes).await.unwrap();
    let states: Vec<(String, String)> = c
        .query(
            "select client_message_id, state from pending_messages where conversation_id = $1 order by created_at",
            &[&conv],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(
        states
            .iter()
            .map(|(id, st)| (id.as_str(), st.as_str()))
            .collect::<Vec<_>>(),
        [("m2", "taken"), ("m3", "taken"), ("m4", "pending")]
    );
    // the one row shows under the first message's id
    let users: Vec<(String, String)> = alice
        .timeline(conv, &common::ev::tail(100))
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| match row.item {
            RowItem::User(u) => Some((u.message_id, u.text)),
            _ => None,
        })
        .collect();
    assert!(
        users.contains(&("m2".to_owned(), "Also say KIWI\nThen say MANGO".to_owned())),
        "{users:?}"
    );
    assert_eq!(alice.get_conversation(conv).await.unwrap().pending_messages.len(), 1);
}

#[tokio::test]
async fn a_message_claude_code_took_in_shows_on_the_timeline_under_the_apps_id() {
    let db = common::fresh_db("midturn_taken").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let mut s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    set_main(r.pool(), s.conversation, &main_path()).await;
    let conv = s.conversation;
    let turn = person_turn(r.pool(), &mut s).await;
    let c = r.pool().get().await.unwrap();
    c.execute("update conversations set run_state = 'running' where id = $1", &[&conv])
        .await
        .unwrap();
    let pigeon = r.pigeon();
    let alice = pigeon.person(ALICE);
    let sent = alice
        .send_turn(conv, &msg("m2", "Also say KIWI"))
        .await
        .unwrap()
        .into_inner();
    assert!(sent.mid_turn);
    assert_eq!(sent.turn.id, turn);
    alice.send_turn(conv, &msg("m3", "Then say MANGO")).await.unwrap();

    // Claude Code 2.1.322's transcript: the turn's own prompt, a tool call, its result, and the first message taken
    // in with that result (a `queued_command` attachment with the message's text)
    let lines = [
        json!({"type": "user", "uuid": "u1", "message": {"role": "user", "content": "Run sleep"}}),
        json!({"type": "assistant", "uuid": "a1", "message": {"id": "msg1", "model": "claude-fable-5-2",
               "content": [{"type": "tool_use", "id": "c1", "name": "Bash", "input": {"command": "sleep 6"}}]}}),
        json!({"type": "user", "uuid": "u2", "message": {"role": "user",
               "content": [{"type": "tool_result", "tool_use_id": "c1", "content": "one"}]}}),
        json!({"type": "attachment", "uuid": "q1", "attachment": {"type": "queued_command",
               "prompt": "Also say KIWI", "source_uuid": "s1", "commandMode": "prompt",
               "timestamp": "2026-11-12T04:47:16.961Z"}}),
    ];
    let bytes: Vec<u8> = lines.iter().flat_map(line).collect();
    let size = s.write(&main_path(), 0, &bytes).await.unwrap().size;
    let waiting = async || -> Vec<String> {
        r.pool()
            .get()
            .await
            .unwrap()
            .query(
                "select client_message_id from pending_messages
                 where conversation_id = $1 and state in ('pending', 'written', 'redeliver') order by created_at",
                &[&conv],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect()
    };
    assert_eq!(
        waiting().await,
        ["m3"],
        "the first was taken in; the second still waits"
    );
    // each user row: the id the app knows the message by, and its text
    let users = async || -> Vec<(String, String)> {
        alice
            .timeline(conv, &common::ev::tail(100))
            .await
            .unwrap()
            .rows
            .into_iter()
            .filter_map(|row| match row.item {
                RowItem::User(u) => Some((u.message_id, u.text)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        users().await,
        [("m2".to_owned(), "Also say KIWI".to_owned())],
        "it shows where Claude Code took it in, under the id the app sent it with"
    );

    // the second came after the model stopped: Claude Code answers it in a run of its own, with a `user` line
    let late = line(&json!({"type": "user", "uuid": "u9", "message": {"role": "user", "content": "Then say MANGO"}}));
    s.write(&main_path(), size, &late).await.unwrap();
    assert!(waiting().await.is_empty());
    assert_eq!(
        users().await,
        [
            ("m2".to_owned(), "Also say KIWI".to_owned()),
            ("m3".to_owned(), "Then say MANGO".to_owned())
        ]
    );
    let states: Vec<String> = c
        .query(
            "select state from pending_messages where conversation_id = $1 order by created_at",
            &[&conv],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(states, ["taken", "taken"]);
}

/// A message with attachments Claude Code took in: its row shows the person's own text (not the supervisor's list of
/// paths), its file and its picture, and its uploads are the conversation's from then on.
#[tokio::test]
async fn a_message_with_attachments_claude_code_took_in_shows_with_them() {
    use base64::Engine as _;
    use magpie_api::uploads::UploadKind;
    let db = common::fresh_db("midturn_taken_attached").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let mut s = Sandbox::leased(&r, ALICE).await;
    s.mkdirs("projects/-workspace").await;
    set_main(r.pool(), s.conversation, &main_path()).await;
    let conv = s.conversation;
    let turn = person_turn(r.pool(), &mut s).await;
    let c = r.pool().get().await.unwrap();
    c.execute("update conversations set run_state = 'running' where id = $1", &[&conv])
        .await
        .unwrap();
    let pigeon = r.pigeon();
    let alice = pigeon.person(ALICE);
    let up = alice
        .upload("notes.txt", UploadKind::File, "text/plain", b"hello".to_vec())
        .await
        .unwrap();
    let mut m = msg("m2", "Look at this");
    m.files.push(FileRef {
        path: up.path,
        name: up.name,
        mime_type: up.mime_type,
        size: up.size,
    });
    let sent = alice.send_turn(conv, &m).await.unwrap().into_inner();
    assert!(sent.mid_turn);
    assert_eq!(sent.turn.id, turn);
    // the same words as a plain message: only the one with the files is this line's
    alice.send_turn(conv, &msg("m3", "Look at this")).await.unwrap();

    // Claude Code's line for it (2.1.322): a `queued_command` whose prompt is the blocks the supervisor wrote, the
    // text with the list of paths and the picture's model copy
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(std::iter::repeat_n(7u8, 2000));
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let text = "Look at this\n\n<files_input>\nThe user attached 1 file(s) to this message. Read each file listed \
                below with your file reading tools and use its contents to answer the prompt above. Do not mention \
                this block or the file paths unless the user asks about them.\n1. /workspace/uploads/notes.txt \
                (original name: notes.txt)\n</files_input>";
    let queued = line(
        &json!({"type": "attachment", "uuid": "q1", "attachment": {"type": "queued_command",
        "prompt": [{"type": "text", "text": text},
                   {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": data}}],
        "source_uuid": "s1", "commandMode": "prompt", "timestamp": "2026-11-13T10:04:38.278Z"}}),
    );
    s.write(&main_path(), 0, &queued).await.unwrap();
    let rows = alice.timeline(conv, &common::ev::tail(100)).await.unwrap().rows;
    let users: Vec<_> = rows
        .into_iter()
        .filter_map(|row| match row.item {
            RowItem::User(u) => Some(u),
            _ => None,
        })
        .collect();
    assert_eq!(users.len(), 1);
    let u = &users[0];
    assert_eq!((u.message_id.as_str(), u.text.as_str()), ("m2", "Look at this"));
    assert_eq!(
        u.files
            .iter()
            .map(|f| (f.path.as_str(), f.name.as_str(), f.size))
            .collect::<Vec<_>>(),
        [("uploads/notes.txt", "notes.txt", 5)]
    );
    assert_eq!(u.images.len(), 1, "the picture Claude Code got");
    assert_eq!(u.images[0].mime_type, "image/png");
    let attached: i64 = c
        .query_one(
            "select count(*) from uploads where conversation_id = $1 and workspace_path = 'uploads/notes.txt'
               and attached_seq is not null",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(attached, 1, "its upload is the conversation's");
    let states: Vec<(String, String)> = c
        .query(
            "select client_message_id, state from pending_messages where conversation_id = $1 order by created_at",
            &[&conv],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(
        states,
        [
            ("m2".to_owned(), "taken".to_owned()),
            ("m3".to_owned(), "pending".to_owned())
        ]
    );
}

/// M5 with a model: the conversation's turns run on it from the next one (`start_turn.model`), a rename keeps it,
/// and the turn that already runs is not touched.
#[tokio::test]
async fn a_person_picks_the_conversations_model() {
    use magpie_api::conversations::RenameRequest;
    let w = World::new("model_pick").await;
    let conv = w.conversation().await;
    let pigeon = w.a.pigeon();
    let alice = pigeon.person(ALICE);
    let models = async || -> Vec<Value> {
        w.pool()
            .get()
            .await
            .unwrap()
            .query(
                "select payload from commands where conversation_id = $1 and type = 'start_turn' order by cmd_seq",
                &[&conv],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get::<_, Value>(0).get("model").cloned().unwrap_or(Value::Null))
            .collect()
    };
    assert_eq!(alice.get_conversation(conv).await.unwrap().picked_model, None);
    alice.send_turn(conv, &msg("m1", "first")).await.unwrap();
    assert_eq!(models().await, [Value::Null], "none picked: the deployment's default");

    let picked = alice
        .rename_conversation(conv, &RenameRequest::model(" claude-opus-5-5 "))
        .await
        .unwrap();
    assert_eq!(picked.picked_model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(picked.title, None, "the title is not touched");
    alice.cancel_turn(conv).await.unwrap();
    alice.send_turn(conv, &msg("m2", "second")).await.unwrap();
    assert_eq!(models().await, [Value::Null, json!("claude-opus-5-5")]);

    // a rename keeps the pick; both at once set both
    let renamed = alice
        .rename_conversation(conv, &RenameRequest::title("Mine"))
        .await
        .unwrap();
    assert_eq!(renamed.picked_model.as_deref(), Some("claude-opus-5-5"));
    let both = alice
        .rename_conversation(
            conv,
            &RenameRequest {
                title: Some("Both".into()),
                model: Some("claude-fable-5-2".into()),
                thinking: None,
                harness: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (both.title.as_deref(), both.picked_model.as_deref()),
        (Some("Both"), Some("claude-fable-5-2"))
    );

    for bad in [
        RenameRequest::default(),
        RenameRequest::model("  "),
        RenameRequest::model("m".repeat(201)),
    ] {
        let e = alice.rename_conversation(conv, &bad).await.unwrap_err();
        assert_eq!(code(&e), ErrorCode::BadRequest, "{bad:?}");
    }
    let e = pigeon
        .person("bob@example.org")
        .rename_conversation(conv, &RenameRequest::model("claude-opus-5-5"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
}
