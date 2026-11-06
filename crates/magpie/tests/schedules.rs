//! Schedules (src/schedules): the AI's REST routes through the gateway (its own person and conversation only, the
//! safety stops), the app's panel, the admin's list and pause-all switch, and firing: a new turn in an idle
//! conversation, a mid-turn `send_message` into a running one and its delivery again when the turn ended first, at
//! most 10 minutes late after an outage (later: skipped), exactly once across replicas.

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

use std::time::Duration;

use aviary_fakes::hermit::{FakeHermit, HermitConfig, Running};
use magpie::schedules::fire;
use magpie::turns::driver::Driver;
use magpie_api::client::{Client, ClientError};
use magpie_api::conversations::CreateRequest;
use magpie_api::errors::ErrorCode;
use magpie_api::schedules::{CreateSchedule, PauseAll, UpdateSchedule};
use magpie_api::turns::SendRequest;
use reqwest::header::{HeaderMap, HeaderValue};
use uuid::Uuid;

const ALICE: &str = "alice@example.org";
const BOB: &str = "bob@example.org";
const ADMIN: &str = "deyao@example.org";

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

    async fn replica(&self) -> common::Replica {
        common::replica_with(&self.db.url, &self.t, &self.hermit.url(), common::idle()).await
    }

    fn pool(&self) -> &aviary_pg::Pool {
        self.a.pool()
    }

    /// A conversation of `person` with no turn.
    async fn conversation(&self, person: &str) -> Uuid {
        let key = Uuid::new_v4().to_string();
        self.a
            .pigeon()
            .person(person)
            .create_conversation(&CreateRequest {
                idempotency_key: key.clone(),
                message: msg(&format!("{key}:m"), ""),
            })
            .await
            .unwrap()
            .get()
            .conversation
            .id
    }

    /// The AI in `conv`'s sandbox: the gateway's token and the identity it verified.
    fn ai(&self, conv: Uuid, person: &str) -> Client {
        let mut h = HeaderMap::new();
        h.insert(
            "aviary-sandbox-id",
            HeaderValue::from_str(&Uuid::new_v4().to_string()).unwrap(),
        );
        h.insert(
            "aviary-sandbox-conversation",
            HeaderValue::from_str(&conv.to_string()).unwrap(),
        );
        h.insert("aviary-sandbox-person", HeaderValue::from_str(person).unwrap());
        let build = aviary_core::version::Build::new("magpie-test");
        let http = aviary_core::http::client_builder(&build, None)
            .default_headers(h)
            .build()
            .unwrap();
        Client::new(http, self.a.url.clone(), self.t.gateway.clone())
    }

    async fn sql(&self, q: &str) {
        self.pool().get().await.unwrap().execute(q, &[]).await.unwrap();
    }

    /// Makes `id` due `ago` before now.
    async fn due(&self, id: &str, ago: Duration) {
        let secs = ago.as_secs_f64();
        self.pool()
            .get()
            .await
            .unwrap()
            .execute(
                "update schedules set next_at = now() - make_interval(secs => $2) where id = $1",
                &[&id, &secs],
            )
            .await
            .unwrap();
    }

    async fn pass(&self) -> usize {
        fire::pass(&Driver::of(self.a.app())).await.unwrap()
    }

    /// `(outcome, turn, cmd_seq)` of each run of `id`, oldest first.
    async fn runs(&self, id: &str) -> Vec<(String, Option<Uuid>, Option<i64>)> {
        self.pool()
            .get()
            .await
            .unwrap()
            .query(
                "select outcome, turn_id, cmd_seq from schedule_runs where schedule_id = $1 order by due_at",
                &[&id],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2)))
            .collect()
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

    async fn turn_state(&self, turn: Uuid) -> String {
        self.pool()
            .get()
            .await
            .unwrap()
            .query_one("select state from turns where id = $1", &[&turn])
            .await
            .unwrap()
            .get(0)
    }

    async fn state(&self, id: &str) -> (String, bool) {
        let r = self
            .pool()
            .get()
            .await
            .unwrap()
            .query_one("select state, next_at is not null from schedules where id = $1", &[&id])
            .await
            .unwrap();
        (r.get(0), r.get(1))
    }
}

fn msg(cmid: &str, text: &str) -> SendRequest {
    SendRequest {
        client_message_id: cmid.to_owned(),
        text: text.to_owned(),
        images: Vec::new(),
        files: Vec::new(),
    }
}

fn code(e: &ClientError) -> ErrorCode {
    e.code().cloned().unwrap_or(ErrorCode::Other("none".into()))
}

fn cron(prompt: &str, rule: &str) -> CreateSchedule {
    CreateSchedule {
        prompt: prompt.into(),
        cron: Some(rule.into()),
        ..CreateSchedule::default()
    }
}

fn soon(prompt: &str) -> CreateSchedule {
    let at = time::OffsetDateTime::now_utc() + time::Duration::minutes(2);
    CreateSchedule {
        prompt: prompt.into(),
        at: Some(at.format(&time::format_description::well_known::Rfc3339).unwrap()),
        ..CreateSchedule::default()
    }
}

#[tokio::test]
async fn the_ai_arms_reads_changes_and_deletes_its_own_schedules() {
    let w = World::new("sched_api").await;
    let conv = w.conversation(ALICE).await;
    let ai = w.ai(conv, ALICE);
    let s = ai
        .sandbox(None)
        .create_schedule(&cron("整理昨天的新聞，列出來源。", "30 8 * * 1-5"))
        .await
        .unwrap();
    assert!(s.id.starts_with("sch_") && s.id.len() == 36, "{}", s.id);
    assert_eq!(s.describe, "每個工作日 08:30");
    assert_eq!(s.tz, "Asia/Hong_Kong");
    assert_eq!(s.kind, "cron");
    assert_eq!(s.state, "active");
    assert_eq!(s.conversation_id, conv);
    assert_eq!(s.created_by, ALICE);
    let next = s.next.clone().unwrap();
    assert!(next.ends_with("08:30:00+08:00"), "{next}");
    // a one-off, its reading and next time
    let once = ai.sandbox(None).create_schedule(&soon("提醒我發稿。")).await.unwrap();
    assert_eq!(once.kind, "once");
    assert!(once.describe.contains("（星期"), "{}", once.describe);
    assert_eq!(once.next, once.at);
    // the person's list, newest first
    let list = ai.sandbox(None).list_schedules().await.unwrap();
    assert_eq!(
        list.schedules.iter().map(|x| x.id.clone()).collect::<Vec<_>>(),
        [once.id.clone(), s.id.clone()]
    );
    // paused: no next time; resumed: one again
    let p = ai
        .sandbox(None)
        .update_schedule(
            &s.id,
            &UpdateSchedule {
                paused: Some(true),
                ..UpdateSchedule::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((p.state.as_str(), p.next.as_deref()), ("paused", None));
    let r = ai
        .sandbox(None)
        .update_schedule(
            &s.id,
            &UpdateSchedule {
                paused: Some(false),
                cron: Some("0 9 * * *".into()),
                prompt: Some("新的訊息".into()),
                ..UpdateSchedule::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.state, "active");
    assert_eq!(r.describe, "每天 09:00");
    assert_eq!(r.prompt, "新的訊息");
    assert!(r.next.unwrap().ends_with("09:00:00+08:00"));
    // one, with its runs (none yet)
    let g = ai.sandbox(None).get_schedule(&s.id).await.unwrap();
    assert_eq!(g.runs, Some(Vec::new()));
    // deleted: gone from the list, 404 after
    ai.sandbox(None).delete_schedule(&s.id).await.unwrap();
    assert_eq!(
        code(&ai.sandbox(None).get_schedule(&s.id).await.unwrap_err()),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(&ai.sandbox(None).delete_schedule(&s.id).await.unwrap_err()),
        ErrorCode::NotFound
    );
    assert_eq!(ai.sandbox(None).list_schedules().await.unwrap().schedules.len(), 1);
}

#[tokio::test]
async fn safety_stops_and_bad_rules_are_refused_in_words() {
    let w = World::new("sched_refused").await;
    let conv = w.conversation(ALICE).await;
    let ai = w.ai(conv, ALICE);
    for (req, words) in [
        (cron("x", "*/5 * * * *"), "at most every 15 minutes"),
        (cron("x", "61 * * * *"), "minute"),
        (cron("x", "0 9 30 2 *"), "never fires"),
        (cron("", "0 9 * * *"), "prompt is empty"),
        (
            CreateSchedule {
                prompt: "x".into(),
                at: Some("2020-01-01T09:00+08:00".into()),
                ..CreateSchedule::default()
            },
            "in the past",
        ),
        (
            CreateSchedule {
                prompt: "x".into(),
                ..CreateSchedule::default()
            },
            "give at",
        ),
        (
            CreateSchedule {
                prompt: "x".into(),
                cron: Some("0 9 * * *".into()),
                tz: Some("Mars/Base".into()),
                ..CreateSchedule::default()
            },
            "IANA",
        ),
    ] {
        let e = ai.sandbox(None).create_schedule(&req).await.unwrap_err();
        assert_eq!(code(&e), ErrorCode::BadRequest, "{req:?}");
        assert!(e.to_string().contains(words), "{req:?}: {e}");
    }
    // a person has at most 25 live schedules
    for i in 0..magpie::schedules::MAX_LIVE {
        ai.sandbox(None)
            .create_schedule(&cron(&format!("n{i}"), "0 9 * * *"))
            .await
            .unwrap();
    }
    let e = ai
        .sandbox(None)
        .create_schedule(&cron("one more", "0 9 * * *"))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("already has 25"), "{e}");
}

#[tokio::test]
async fn each_person_sees_and_arms_only_their_own() {
    let w = World::new("sched_own").await;
    let alice_conv = w.conversation(ALICE).await;
    let bob_conv = w.conversation(BOB).await;
    let s = w
        .ai(alice_conv, ALICE)
        .sandbox(None)
        .create_schedule(&cron("x", "0 9 * * *"))
        .await
        .unwrap();
    let bob = w.ai(bob_conv, BOB);
    assert_eq!(
        code(&bob.sandbox(None).get_schedule(&s.id).await.unwrap_err()),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(&bob.sandbox(None).delete_schedule(&s.id).await.unwrap_err()),
        ErrorCode::NotFound
    );
    assert!(bob.sandbox(None).list_schedules().await.unwrap().schedules.is_empty());
    // a sandbox naming someone else's conversation arms nothing
    let e = w
        .ai(alice_conv, BOB)
        .sandbox(None)
        .create_schedule(&cron("x", "0 9 * * *"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
    // the panel: the owner's conversation only
    let panel =
        w.a.pigeon()
            .person(ALICE)
            .conversation_schedules(alice_conv)
            .await
            .unwrap();
    assert_eq!(panel.schedules.len(), 1);
    assert_eq!(panel.schedules[0].runs, Some(Vec::new()));
    let e =
        w.a.pigeon()
            .person(BOB)
            .conversation_schedules(alice_conv)
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
    assert!(
        w.a.pigeon()
            .person(BOB)
            .conversation_schedules(bob_conv)
            .await
            .unwrap()
            .schedules
            .is_empty()
    );
    // pigeon's token is not the gateway's: the AI's routes refuse it
    let e = w.a.pigeon().sandbox(None).list_schedules().await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::WrongCaller);
}

#[tokio::test]
async fn a_fire_in_an_idle_conversation_is_a_new_turn_with_the_wrapped_prompt() {
    let w = World::new("sched_idle").await;
    let conv = w.conversation(ALICE).await;
    let s = w
        .ai(conv, ALICE)
        .sandbox(None)
        .create_schedule(&cron("整理新聞", "30 8 * * 1-5"))
        .await
        .unwrap();
    assert_eq!(w.pass().await, 0, "not due yet");
    w.due(&s.id, Duration::from_secs(5)).await;
    assert_eq!(w.pass().await, 1);
    let runs = w.runs(&s.id).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].0, "sent");
    let turn = runs[0].1.unwrap();
    assert_eq!(
        w.sent(conv).await,
        ["<scheduled-message rule=\"每個工作日 08:30\">整理新聞</scheduled-message>"]
    );
    let cmid: String = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select client_message_id from turns where id = $1", &[&turn])
        .await
        .unwrap()
        .get(0);
    assert!(cmid.starts_with(&format!("schedule:{}:", s.id)), "{cmid}");
    // the next fire moved on into the future; nothing fires again
    let (state, has_next) = w.state(&s.id).await;
    assert_eq!((state.as_str(), has_next), ("active", true));
    assert_eq!(w.pass().await, 0);
    // the panel shows the run
    let panel = w.a.pigeon().person(ALICE).conversation_schedules(conv).await.unwrap();
    let r = panel.schedules[0].runs.clone().unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].outcome, "sent", "delivered, not answered yet");
}

/// The outcome of each run of schedule `id` (newest first) as the panel shows it; the AI's read agrees.
async fn outcomes(w: &World, conv: Uuid, id: &str) -> Vec<String> {
    let panel = w.a.pigeon().person(ALICE).conversation_schedules(conv).await.unwrap();
    let mine = w.ai(conv, ALICE).sandbox(None).get_schedule(id).await.unwrap();
    let a: Vec<String> = panel.schedules[0].runs.clone().unwrap().into_iter().map(|r| r.outcome).collect();
    let b: Vec<String> = mine.runs.unwrap().into_iter().map(|r| r.outcome).collect();
    assert_eq!(a, b, "the panel and the AI read the same");
    a
}

async fn set_turn(w: &World, turn: Uuid, state: &str) {
    w.pool()
        .get()
        .await
        .unwrap()
        .execute("update turns set state = $2, ended_at = now() where id = $1", &[&turn, &state])
        .await
        .unwrap();
}

#[tokio::test]
async fn a_run_reads_answered_once_its_turn_completed_and_unanswered_once_it_failed() {
    let w = World::new("sched_answered").await;
    let conv = w.conversation(ALICE).await;
    let s = w
        .ai(conv, ALICE)
        .sandbox(None)
        .create_schedule(&cron("整理新聞", "30 8 * * 1-5"))
        .await
        .unwrap();
    w.due(&s.id, Duration::from_secs(5)).await;
    assert_eq!(w.pass().await, 1);
    let turn = w.runs(&s.id).await[0].1.unwrap();
    assert_eq!(outcomes(&w, conv, &s.id).await, ["sent"]);
    set_turn(&w, turn, "completed").await;
    assert_eq!(outcomes(&w, conv, &s.id).await, ["answered"]);
    // a message taken into a running turn that completed; a carried-over run (no turn) keeps what the old stack wrote
    w.sql(&format!(
        "insert into schedule_runs (schedule_id, due_at, outcome, conversation_id, turn_id)
           values ('{}', now() - interval '1 day', 'sent_mid_turn', '{conv}', '{turn}'),
                  ('{}', now() - interval '2 days', 'answered_mid_turn', '{conv}', null),
                  ('{}', now() - interval '3 days', 'sent', '{conv}', null)",
        s.id, s.id, s.id
    ))
    .await;
    assert_eq!(outcomes(&w, conv, &s.id).await, ["answered", "answered_mid_turn", "answered_mid_turn", "sent"]);
    // the stored outcome stays what the fire wrote: the answer is read from the turn
    assert_eq!(w.runs(&s.id).await.last().unwrap().0, "sent");
    set_turn(&w, turn, "failed").await;
    assert_eq!(outcomes(&w, conv, &s.id).await, ["unanswered", "unanswered", "answered_mid_turn", "sent"]);
    set_turn(&w, turn, "canceled").await;
    assert_eq!(outcomes(&w, conv, &s.id).await[0], "unanswered");
}

#[tokio::test]
async fn a_one_off_fires_once_and_is_done() {
    let w = World::new("sched_once").await;
    let conv = w.conversation(ALICE).await;
    let s = w
        .ai(conv, ALICE)
        .sandbox(None)
        .create_schedule(&soon("提醒我"))
        .await
        .unwrap();
    w.due(&s.id, Duration::ZERO).await;
    assert_eq!(w.pass().await, 1);
    assert_eq!(w.state(&s.id).await, ("done".to_owned(), false));
    assert_eq!(w.pass().await, 0);
    assert_eq!(w.runs(&s.id).await.len(), 1);
}

#[tokio::test]
async fn late_fires_go_out_within_10_minutes_and_are_skipped_after() {
    let w = World::new("sched_late").await;
    let conv = w.conversation(ALICE).await;
    let ai = w.ai(conv, ALICE);
    let within = ai.sandbox(None).create_schedule(&cron("a", "0 9 * * *")).await.unwrap();
    let after = ai
        .sandbox(None)
        .create_schedule(&cron("b", "0 10 * * *"))
        .await
        .unwrap();
    w.due(&within.id, Duration::from_mins(9)).await;
    w.due(&after.id, Duration::from_mins(11)).await;
    assert_eq!(w.pass().await, 2);
    assert_eq!(w.runs(&within.id).await[0].0, "sent");
    assert_eq!(w.runs(&after.id).await[0].0, "skipped_late");
    assert_eq!(w.sent(conv).await.len(), 1, "only the one within 10 minutes was posted");
    // the skipped one waits for its next time, in the future
    let (state, has_next) = w.state(&after.id).await;
    assert_eq!((state.as_str(), has_next), ("active", true));
    let future: bool = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select next_at > now() from schedules where id = $1", &[&after.id])
        .await
        .unwrap()
        .get(0);
    assert!(future);
    let shown = ai.sandbox(None).get_schedule(&after.id).await.unwrap().runs.unwrap();
    assert_eq!(shown[0].outcome, "skipped_late");
}

#[tokio::test]
async fn a_fire_during_a_turn_joins_it_and_is_delivered_again_if_the_turn_ended_first() {
    let w = World::new("sched_midturn").await;
    let conv = w.conversation(ALICE).await;
    let ai = w.ai(conv, ALICE);
    let s = ai
        .sandbox(None)
        .create_schedule(&cron("看看進度", "0 9 * * *"))
        .await
        .unwrap();
    // a turn is running (admitted, waiting for its sandbox)
    let busy =
        w.a.pigeon()
            .person(ALICE)
            .send_turn(conv, &msg("m1", "長任務"))
            .await
            .unwrap()
            .get()
            .turn
            .id;
    w.due(&s.id, Duration::from_secs(1)).await;
    assert_eq!(w.pass().await, 1);
    let runs = w.runs(&s.id).await;
    assert_eq!(runs[0].0, "pending");
    assert_eq!(runs[0].1, Some(busy), "it joined the running turn");
    let seq = runs[0].2.unwrap();
    let (kind, turn, text): (String, Option<Uuid>, Option<String>) = {
        let r = w
            .pool()
            .get()
            .await
            .unwrap()
            .query_one(
                "select type, turn_id, payload->>'text' from commands where conversation_id = $1 and cmd_seq = $2",
                &[&conv, &seq],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2))
    };
    assert_eq!((kind.as_str(), turn), ("send_message", Some(busy)));
    assert_eq!(
        text.as_deref(),
        Some("<scheduled-message rule=\"每天 09:00\">看看進度</scheduled-message>")
    );
    assert_eq!(w.sent(conv).await, ["長任務"], "no new turn while one runs");
    // the supervisor wrote it into the turn: delivered
    w.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update commands set acked_at = now(), result = '{\"delivered\": true}' where conversation_id = $1
               and cmd_seq = $2",
            &[&conv, &seq],
        )
        .await
        .unwrap();
    w.pass().await;
    assert_eq!(w.runs(&s.id).await[0].0, "sent_mid_turn");
    // the next fire: the turn ends before the supervisor took the message, so it is sent again as a new turn
    w.due(&s.id, Duration::from_secs(1)).await;
    w.pass().await;
    let runs = w.runs(&s.id).await;
    assert_eq!(runs[1].0, "pending");
    let stale = runs[1].2.unwrap();
    let r = w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    assert_eq!(r.turn_id, Some(busy));
    assert_eq!(w.turn_state(busy).await, "canceled");
    w.pass().await;
    let runs = w.runs(&s.id).await;
    assert_eq!(runs[1].0, "sent", "delivered again as a new turn");
    let second = runs[1].1.unwrap();
    assert_ne!(second, busy);
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
    assert_eq!(w.sent(conv).await.len(), 2);
    // a negative ack (the turn ended between the fire and the write) is delivered again the same way
    w.due(&s.id, Duration::from_secs(1)).await;
    w.pass().await;
    let runs = w.runs(&s.id).await;
    assert_eq!((runs[2].0.as_str(), runs[2].1), ("pending", Some(second)));
    let seq = runs[2].2.unwrap();
    w.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update commands set acked_at = now(), result = '{\"delivered\": false}' where conversation_id = $1
               and cmd_seq = $2",
            &[&conv, &seq],
        )
        .await
        .unwrap();
    // still running: it joins the turn again with a new command
    w.pass().await;
    let runs = w.runs(&s.id).await;
    assert_eq!(runs[2].0, "pending");
    assert_ne!(runs[2].2, Some(seq));
    w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    assert_eq!(w.turn_state(second).await, "canceled");
    w.pass().await;
    assert_eq!(w.runs(&s.id).await[2].0, "sent");
    assert_eq!(w.sent(conv).await.len(), 3, "each fire posted exactly once");
}

#[tokio::test]
async fn each_fire_happens_exactly_once_across_replicas() {
    let w = World::new("sched_once_across").await;
    let b = w.replica().await;
    let c = w.replica().await;
    let mut ids = Vec::new();
    let mut convs = Vec::new();
    for i in 0..6 {
        let conv = w.conversation(ALICE).await;
        let s = w
            .ai(conv, ALICE)
            .sandbox(None)
            .create_schedule(&cron(&format!("p{i}"), "0 9 * * *"))
            .await
            .unwrap();
        w.due(&s.id, Duration::from_secs(1)).await;
        ids.push(s.id);
        convs.push(conv);
    }
    let (da, db, dc) = (Driver::of(w.a.app()), Driver::of(b.app()), Driver::of(c.app()));
    let (x, y, z) = tokio::join!(fire::pass(&da), fire::pass(&db), fire::pass(&dc));
    assert_eq!(
        x.unwrap() + y.unwrap() + z.unwrap(),
        6,
        "six fires between the three replicas"
    );
    for (id, conv) in ids.iter().zip(&convs) {
        assert_eq!(w.runs(id).await.len(), 1, "{id}: one run");
        assert_eq!(w.sent(*conv).await.len(), 1, "{id}: one message");
    }
    let (x, y) = tokio::join!(fire::pass(&da), fire::pass(&db));
    assert_eq!(x.unwrap() + y.unwrap(), 0);
}

#[tokio::test]
async fn pause_all_and_deleted_conversations_skip_and_record() {
    let w = World::new("sched_pause").await;
    let conv = w.conversation(ALICE).await;
    let ai = w.ai(conv, ALICE);
    let s = ai.sandbox(None).create_schedule(&cron("a", "0 9 * * *")).await.unwrap();
    let admin = w.a.pigeon();
    let v = admin
        .service(Some(ADMIN))
        .admin_schedules_pause(&PauseAll { paused: true })
        .await
        .unwrap();
    assert!(v.paused_all);
    assert_eq!(v.paused_by.as_deref(), Some(ADMIN));
    w.due(&s.id, Duration::from_secs(1)).await;
    w.pass().await;
    assert_eq!(w.runs(&s.id).await[0].0, "skipped_paused");
    assert!(w.sent(conv).await.is_empty());
    admin
        .service(Some(ADMIN))
        .admin_schedules_pause(&PauseAll { paused: false })
        .await
        .unwrap();
    // a conversation in Recently deleted gets nothing; its schedule goes on
    w.a.pigeon().person(ALICE).delete_conversation(conv).await.unwrap();
    w.due(&s.id, Duration::from_secs(1)).await;
    w.pass().await;
    assert_eq!(w.runs(&s.id).await[1].0, "skipped_deleted");
    assert!(w.sent(conv).await.is_empty());
    // the admin's list: everyone's, with runs; without Aviary-Admin refused
    let all = admin.service(Some(ADMIN)).admin_schedules().await.unwrap();
    assert!(!all.paused_all);
    assert_eq!(all.schedules.len(), 1);
    assert_eq!(all.schedules[0].runs.as_ref().unwrap().len(), 2);
    let e = admin.service(None).admin_schedules().await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadRequest);
}
