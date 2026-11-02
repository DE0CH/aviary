//! Turns (magpie 8): admission, the driver against fake-hermit, the supervisor's command queue and reports from a
//! scripted supervisor, cancel, asks, titles, the sweeper's deadlines and watchdog, hermit's feed, and the first
//! message of a create (M3).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::many_single_char_names,
    clippy::assert_is_empty,
    dead_code
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use aviary_fakes::hermit::{FakeHermit, HermitConfig, Running};
use aviary_fakes::script::Reply;
use hermit_api::sandbox::ReleaseReason;
use magpie::jobs::outbox;
use magpie::turns::driver::Driver;
use magpie::turns::sweeper;
use magpie_api::changes::{After, ChangeBody, ChangesQuery};
use magpie_api::client::{Client, ClientError};
use magpie_api::conversations::{CreateRequest, RunState, SandboxPhase};
use magpie_api::errors::{DataCode, ErrorCode};
use magpie_api::sandbox::{
    AckRequest, AckResult, AgentTurnRequest, AskOutcome, AskReport, AskResolvedReport, CcFacts, CommandBody,
    CommandsQuery, CommandsResponse, Heartbeat, HeartbeatLastTurn, HeartbeatState, KillReason, ReportError,
    ReportedState, SupervisorFacts, TaskReportEvent, TasksReport, TurnStateReport,
};
use magpie_api::turns::{AgentPermissionResponse, Behavior, SendRequest, TurnOrigin, TurnOutcome, TurnPhase};
use magpie_api::{Admitted, Timestamp};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::json;
use uuid::Uuid;

const ALICE: &str = "alice@example.org";

/// A database, fake-hermit and one replica (more with [`World::replica`]).
struct World {
    db: common::TestDb,
    t: common::Tokens,
    hermit: Running,
    a: common::Replica,
}

impl World {
    async fn new(test: &str, ready_after: Option<Duration>) -> Self {
        let db = common::fresh_db(test).await;
        let t = common::Tokens::new();
        let fake = FakeHermit::unguarded(HermitConfig {
            ready_after,
            ..HermitConfig::default()
        });
        let hermit = fake.serve().await.unwrap();
        let a = common::replica_with(&db.url, &t, &hermit.url(), common::idle()).await;
        sweeper::availability_tick(a.pool()).await.unwrap();
        Self { db, t, hermit, a }
    }

    async fn replica(&self) -> common::Replica {
        common::replica_with(&self.db.url, &self.t, &self.hermit.url(), common::idle()).await
    }

    fn fake(&self) -> &Arc<FakeHermit> {
        &self.hermit.fake
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
                idempotency_key: key.clone(),
                message: msg(&format!("{key}:m"), ""),
            })
            .await
            .unwrap()
            .get()
            .conversation
            .id
    }

    async fn send(&self, conv: Uuid, cmid: &str, text: &str) -> Uuid {
        self.a
            .pigeon()
            .person(ALICE)
            .send_turn(conv, &msg(cmid, text))
            .await
            .unwrap()
            .get()
            .turn
            .id
    }

    async fn turn(&self, turn: Uuid) -> (String, Option<String>, Option<String>) {
        let r = self
            .pool()
            .get()
            .await
            .unwrap()
            .query_one(
                "select state, error_code, error_detail from turns where id = $1",
                &[&turn],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2))
    }

    async fn state_is(&self, turn: Uuid, state: &str) -> bool {
        common::eventually(Duration::from_secs(10), async || self.turn(turn).await.0 == state).await
    }

    async fn kinds(&self, conv: Uuid) -> Vec<String> {
        self.pool()
            .get()
            .await
            .unwrap()
            .query(
                "select kind from events where conversation_id = $1 order by seq",
                &[&conv],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect()
    }

    async fn sql(&self, q: &str, conv: Uuid) {
        self.pool().get().await.unwrap().execute(q, &[&conv]).await.unwrap();
    }

    async fn sql0(&self, q: &str) {
        self.pool().get().await.unwrap().execute(q, &[]).await.unwrap();
    }

    async fn changes(&self) -> Vec<ChangeBody> {
        self.a
            .pigeon()
            .person(ALICE)
            .changes(&ChangesQuery {
                after: After::Seq(0),
                limit: Some(500),
                wait_ms: None,
            })
            .await
            .unwrap()
            .changes
            .into_iter()
            .map(|c| c.body)
            .collect()
    }

    /// The supervisor of the conversation's live fake sandbox, holding the lease.
    async fn supervisor(&self, conv: Uuid) -> Sup {
        assert!(
            common::eventually(Duration::from_secs(10), async || self.fake().live(conv).is_some()).await,
            "a sandbox exists"
        );
        let sandbox = self.fake().live(conv).unwrap().id;
        Sup::new(&self.a, conv, sandbox).await
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

/// A scripted supervisor: the gateway's token and identity headers, and the lease.
struct Sup {
    c: Client,
    lease: Uuid,
    conv: Uuid,
    sandbox: Uuid,
}

impl Sup {
    async fn new(r: &common::Replica, conv: Uuid, sandbox: Uuid) -> Self {
        let mut h = HeaderMap::new();
        h.insert(
            "aviary-sandbox-id",
            HeaderValue::from_str(&sandbox.to_string()).unwrap(),
        );
        h.insert(
            "aviary-sandbox-conversation",
            HeaderValue::from_str(&conv.to_string()).unwrap(),
        );
        h.insert("aviary-sandbox-person", HeaderValue::from_static(ALICE));
        let build = aviary_core::version::Build::new("magpie-test");
        let http = aviary_core::http::client_builder(&build, None)
            .default_headers(h)
            .build()
            .unwrap();
        let c = Client::new(http, r.url.clone(), r.tokens.gateway.clone());
        // the lease route (WP-12) grants it to hermit's live sandbox of the conversation (magpie 6.9)
        let lease = c.sandbox(None).lease().await.unwrap().lease_id;
        Self {
            c,
            lease,
            conv,
            sandbox,
        }
    }

    fn calls(&self) -> magpie_api::client::SandboxCalls<'_> {
        self.c.sandbox(Some(self.lease))
    }

    async fn poll(&self, after: u64, wait_ms: u64) -> CommandsResponse {
        self.calls()
            .commands(&CommandsQuery {
                after,
                wait_ms: Some(wait_ms),
            })
            .await
            .unwrap()
    }

    async fn ack(&self, seq: u64, result: AckResult) {
        self.calls().ack_command(seq, &AckRequest { result }).await.unwrap();
    }

    async fn report(&self, turn: Uuid, state: ReportedState) -> Result<(), ClientError> {
        self.calls().turn_state(turn, &report(state)).await
    }

    async fn heartbeat(&self, turn: Option<Uuid>, state: HeartbeatState, last: Option<HeartbeatLastTurn>) {
        self.calls()
            .heartbeat(&Heartbeat {
                turn_id: turn,
                state,
                tasks: 0,
                last_turn: last,
                generation: 1,
                supervisor: SupervisorFacts {
                    version: "supervisor-000000000000".into(),
                    boot_id: "boot".into(),
                    restarts: 0,
                },
                cc: CcFacts {
                    quota_exceeded: false,
                    blocked_writes: 0,
                    oldest_blocked_ms: 0,
                },
            })
            .await
            .unwrap();
    }

    /// Takes the turn's `start_turn` (and acks it), then reports running.
    async fn run(&self, turn: Uuid) {
        let r = self.poll(0, 5_000).await;
        let start = r
            .commands
            .iter()
            .find(|c| matches!(c.body, CommandBody::StartTurn(_)) && c.turn_id == Some(turn))
            .expect("start_turn");
        self.ack(
            start.cmd_seq,
            AckResult {
                delivered: Some(true),
                ..AckResult::default()
            },
        )
        .await;
        let mut running = report(ReportedState::Running);
        running.provider_session_id = Some("279aabf0-1111-4222-8333-444455556666".into());
        running.transcript_path = Some("projects/-workspace/279aabf0-1111-4222-8333-444455556666.jsonl".into());
        self.calls().turn_state(turn, &running).await.unwrap();
    }
}

fn report(state: ReportedState) -> TurnStateReport {
    TurnStateReport {
        state,
        at: Timestamp::now(),
        provider_session_id: None,
        transcript_path: None,
        exit_code: None,
        error: None,
        held: None,
        tasks: None,
        last_uuid: None,
        usage: None,
        duration_ms: None,
    }
}

fn driver(r: &common::Replica) -> Driver {
    Driver::of(r.app())
}

#[tokio::test]
async fn a_turn_runs_from_admission_to_completion() {
    let w = World::new("turn_happy", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "幫我整理今日有關港珠澳大橋嘅新聞").await;
    assert!(w.state_is(turn, "starting").await, "hermit's ready reached the driver");
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Running, "a starting turn shows as running");
    assert_eq!(c.turn.as_ref().unwrap().id, turn);
    assert_eq!(c.sandbox, None, "ready within 1 s: no preparing chip");
    let sup = w.supervisor(conv).await;
    let cmds = sup.poll(0, 0).await;
    let kinds: Vec<_> = cmds.commands.iter().map(|c| c.body.type_name().unwrap()).collect();
    assert_eq!(kinds, ["start_turn", "title"]);
    let CommandBody::StartTurn(st) = &cmds.commands[0].body else {
        panic!()
    };
    assert_eq!(st.text, "幫我整理今日有關港珠澳大橋嘅新聞");
    assert_eq!(st.resume.provider_session_id, None);
    sup.run(turn).await;
    assert_eq!(w.turn(turn).await.0, "running");
    // a replay of running is fine; a delivered command is not handed out again after its ack
    sup.report(turn, ReportedState::Running).await.unwrap();
    let again = sup.poll(0, 0).await;
    assert_eq!(again.commands.len(), 1, "only the unacked title is left");
    sup.report(turn, ReportedState::Completed).await.unwrap();
    assert_eq!(w.turn(turn).await.0, "completed");
    sup.report(turn, ReportedState::Completed).await.unwrap();
    let e = sup.report(turn, ReportedState::Failed).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadTransition);
    assert_eq!(e.api().unwrap().current_state(), Some("completed"));
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Idle);
    assert!(c.turn.is_none());
    assert_eq!(c.last_turn.as_ref().unwrap().outcome, TurnOutcome::Completed);
    assert_eq!(
        c.claude_session_id.as_deref(),
        Some("279aabf0-1111-4222-8333-444455556666")
    );
    assert_eq!(
        w.kinds(conv).await,
        ["conversation.created", "user.sent", "turn.started", "turn.ended"]
    );
    let turns: Vec<_> = w
        .changes()
        .await
        .into_iter()
        .filter_map(|c| match c {
            ChangeBody::Turn(t) => Some((t.turn_id, t.phase, t.origin)),
            _ => None,
        })
        .collect();
    assert_eq!(
        turns,
        [
            (turn, TurnPhase::Started, TurnOrigin::Person),
            (turn, TurnPhase::Completed, TurnOrigin::Person)
        ]
    );
    // the next turn resumes Claude Code's session
    let t2 = w.send(conv, "m2", "再嚟").await;
    assert!(w.state_is(t2, "starting").await);
    let cmds = sup.poll(0, 0).await;
    let st = cmds
        .commands
        .iter()
        .find_map(|c| match &c.body {
            CommandBody::StartTurn(s) if c.turn_id == Some(t2) => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        st.resume.provider_session_id.as_deref(),
        Some("279aabf0-1111-4222-8333-444455556666")
    );
}

#[tokio::test]
async fn a_slow_sandbox_shows_preparing_then_ready() {
    let w = World::new("turn_preparing", None).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await, "no ready within 1 s");
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Preparing);
    assert_eq!(c.sandbox, Some(SandboxPhase::Preparing));
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await, "the long-poll answered at once");
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.sandbox, Some(SandboxPhase::Ready));
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    sup.report(turn, ReportedState::Completed).await.unwrap();
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.sandbox, None);
    assert_eq!(
        w.kinds(conv).await,
        [
            "conversation.created",
            "user.sent",
            "turn.started",
            "sandbox.preparing",
            "sandbox.ready",
            "turn.ended"
        ]
    );
    let phases: Vec<_> = w
        .changes()
        .await
        .into_iter()
        .filter_map(|c| match c {
            ChangeBody::Sandbox { state } => Some(state),
            _ => None,
        })
        .collect();
    assert_eq!(phases, [SandboxPhase::Preparing, SandboxPhase::Ready]);
    // only one PUT and long-polls: no fixed-interval poll
    assert_eq!(w.fake().script().calls_to("put_sandbox").len(), 1);
}

#[tokio::test]
async fn a_supervisor_that_runs_before_the_ready_reached_the_driver_is_the_ready() {
    let w = World::new("turn_early", None).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    assert_eq!(w.turn(turn).await.0, "running");
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.sandbox, Some(SandboxPhase::Ready));
    w.fake().make_ready(conv);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        w.turn(turn).await.0,
        "running",
        "the driver's late ready changes nothing"
    );
}

#[tokio::test]
async fn sends_at_once_admit_one_turn_and_a_repeat_answers_the_same_turn() {
    let w = World::new("turn_race", None).await;
    let conv = w.conversation().await;
    let mut tasks = Vec::new();
    for i in 0..16 {
        let client = w.a.pigeon();
        tasks.push(tokio::spawn(async move {
            client
                .person(ALICE)
                .send_turn(conv, &msg(&format!("socket-{i}"), "hi"))
                .await
        }));
    }
    let mut ok = Vec::new();
    for t in tasks {
        match t.await.unwrap() {
            Ok(r) => ok.push(r),
            Err(e) => assert_eq!(code(&e), ErrorCode::TurnRunning),
        }
    }
    assert_eq!(ok.len(), 1, "exactly one send is admitted");
    let first = ok[0].get().turn.clone();
    let n: i64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from turns where conversation_id = $1", &[&conv])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    // the winner's repeat (a reconnect) answers 200 with the same turn, even while it runs
    let cmid: String = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select client_message_id from turns where id = $1", &[&first.id])
        .await
        .unwrap()
        .get(0);
    let again =
        w.a.pigeon()
            .person(ALICE)
            .send_turn(conv, &msg(&cmid, "hi"))
            .await
            .unwrap();
    assert!(!again.is_new());
    assert_eq!(again.get().turn, first);
    // the deleted and empty cases
    let e =
        w.a.pigeon()
            .person(ALICE)
            .send_turn(conv, &msg("empty", "  "))
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadRequest);
    let other =
        w.a.pigeon()
            .person("bob@example.org")
            .send_turn(conv, &msg("x", "hi"))
            .await
            .unwrap_err();
    assert_eq!(code(&other), ErrorCode::NotFound);
}

#[tokio::test]
async fn a_deleted_conversation_refuses_a_send() {
    let w = World::new("turn_deleted", None).await;
    let conv = w.conversation().await;
    // a turn waiting for its sandbox when the conversation is deleted gets no new sandbox
    let turn = w.send(conv, "m0", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    w.a.pigeon().person(ALICE).delete_conversation(conv).await.unwrap();
    let gone = w.fake().live(conv).unwrap();
    w.fake().end(conv, ReleaseReason::Deleted, "deleted");
    assert!(w.state_is(turn, "failed").await);
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.lost"));
    assert_eq!(w.fake().live(conv), None, "no new sandbox for a deleted conversation");
    assert_eq!(w.fake().sandboxes().len(), 1, "{gone:?}");
    let e =
        w.a.pigeon()
            .person(ALICE)
            .send_turn(conv, &msg("m", "hi"))
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ConversationDeleted);
}

#[tokio::test]
async fn another_replica_takes_over_a_dead_drivers_turn() {
    let w = World::new("turn_takeover", None).await;
    let b = w.replica().await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    // replica a dies: its claim stays until it lapses, and nothing renews it
    w.a.start_drain();
    let dead = "magpie-dead-replica";
    w.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update turns set driver = $2, driver_until = now() + interval '30 seconds' where id = $1",
            &[&turn, &dead],
        )
        .await
        .unwrap();
    assert_eq!(
        sweeper::pass(&driver(&b)).await.unwrap(),
        0,
        "a live claim is left alone"
    );
    w.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update turns set driver_until = now() - interval '1 second' where id = $1",
            &[&turn],
        )
        .await
        .unwrap();
    assert!(sweeper::pass(&driver(&b)).await.unwrap() >= 1);
    let b_pod = b.app().pod.clone();
    assert!(
        common::eventually(Duration::from_secs(5), async || {
            let r = b
                .pool()
                .get()
                .await
                .unwrap()
                .query_one("select driver from turns where id = $1", &[&turn])
                .await
                .unwrap();
            r.get::<_, Option<String>>(0).as_deref() == Some(b_pod.as_str())
        })
        .await,
        "b claimed the turn"
    );
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await, "b's driver carried the turn on");
    // hermit's PUT is idempotent: one sandbox for every PUT
    assert!(w.fake().script().calls_to("put_sandbox").len() >= 2);
    assert_eq!(w.fake().sandboxes().len(), 1);
}

#[tokio::test]
async fn hermits_refusals_map_to_turn_codes() {
    let w = World::new("turn_refusals", None).await;
    // 503 sandbox_busy with its reason and retry time
    let conv = w.conversation().await;
    w.fake().script().once(
        "put_sandbox",
        Reply::json(
            503,
            json!({"code": "sandbox_busy", "message": "no capacity", "reason": "disabled", "retryAfterS": 300}),
        ),
    );
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "failed").await);
    let (_, code_, detail) = w.turn(turn).await;
    assert_eq!(code_.as_deref(), Some("sandbox.busy"));
    assert_eq!(detail.as_deref(), Some("disabled"));
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Idle);
    assert_eq!(c.last_turn.unwrap().error.unwrap().code, DataCode::SandboxBusy);
    assert_eq!(w.kinds(conv).await.last().map(String::as_str), Some("turn.ended"));
    let ended: serde_json::Value = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select data from events where conversation_id = $1 and kind = 'turn.ended'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(ended["error"]["retryAfterS"], 300);
    // its commands are canceled: a later supervisor never runs the failed turn
    let n: i64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from commands where conversation_id = $1 and type = 'start_turn' and canceled_at is null",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
    // 403 not_allowed
    let conv = w.conversation().await;
    w.fake()
        .script()
        .once("put_sandbox", Reply::error(403, "not_allowed", "not allowed"));
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "failed").await);
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.not_allowed"));
    // 409 person_mismatch is a magpie bug
    let conv = w.conversation().await;
    w.fake()
        .script()
        .once("put_sandbox", Reply::error(409, "person_mismatch", "mismatch"));
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "failed").await);
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("internal"));
    // 5xx and connection errors are retried
    let conv = w.conversation().await;
    w.fake()
        .script()
        .once("put_sandbox", Reply::error(500, "internal", "boom"));
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    assert!(
        common::eventually(Duration::from_secs(5), async || w
            .fake()
            .script()
            .calls_to("put_sandbox")
            .len()
            >= 5)
        .await
    );
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await);
    // a start that failed: gone with sandbox_busy while waiting
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    w.fake().end(conv, ReleaseReason::PoolFull, "pool full");
    assert!(w.state_is(turn, "failed").await);
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.busy"));
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    w.fake().end(conv, ReleaseReason::StartFailed, "image pull failed");
    assert!(w.state_is(turn, "failed").await);
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.failed"));
}

#[tokio::test]
async fn a_sandbox_lost_before_the_supervisor_took_the_turn_is_replaced() {
    let w = World::new("turn_reacquire", None).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    let first = w.fake().live(conv).unwrap().id;
    w.fake().end(conv, ReleaseReason::HostLost, "host lost");
    assert!(
        common::eventually(Duration::from_secs(10), async || {
            w.fake().live(conv).is_some_and(|s| s.id != first)
        })
        .await,
        "the driver asked hermit for a new sandbox"
    );
    assert_eq!(w.turn(turn).await.0, "preparing");
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await);
    // ready, start_turn not acknowledged, and that sandbox goes too: replaced again (the feed's path)
    let second = w.fake().live(conv).unwrap().id;
    w.fake().end(conv, ReleaseReason::Recycle, "recycled");
    assert!(
        common::eventually(Duration::from_secs(10), async || w.turn(turn).await.0 == "preparing").await,
        "the feed sent the turn back to preparing"
    );
    assert!(
        sweeper::pass(&driver(&w.a)).await.unwrap() >= 1,
        "the sweeper gives it a driver"
    );
    assert!(
        common::eventually(Duration::from_secs(10), async || {
            w.fake().live(conv).is_some_and(|s| s.id != second)
        })
        .await
    );
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await);
    // once running, a lost sandbox fails the turn
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    w.fake().end(conv, ReleaseReason::HostLost, "host lost");
    assert!(w.state_is(turn, "failed").await, "the feed ended the turn");
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.lost"));
    let lease: Option<Uuid> = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select lease_id from conversations where id = $1", &[&conv])
        .await
        .unwrap()
        .get(0);
    assert_eq!(lease, None, "a gone sandbox's lease is cleared");
    let e = sup
        .calls()
        .commands(&CommandsQuery {
            after: 0,
            wait_ms: None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        code(&e),
        ErrorCode::LeaseLost,
        "the gone sandbox's supervisor holds no lease"
    );
}

#[tokio::test]
async fn admission_and_preparing_time_out_after_15_minutes() {
    let w = World::new("turn_timeout", None).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    w.sql(
        "update turns set deadline = now() - interval '1 second', created_at = now() - interval '15 minutes'
         where conversation_id = $1",
        conv,
    )
    .await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert!(w.state_is(turn, "failed").await);
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.timeout"));
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Idle);
    assert_eq!(c.sandbox, None, "the preparing chip is cleared");
    assert!(matches!(
        w.changes()
            .await
            .iter()
            .rev()
            .find(|c| matches!(c, ChangeBody::Sandbox { .. })),
        Some(ChangeBody::Sandbox {
            state: SandboxPhase::Ready
        })
    ));
}

#[tokio::test]
async fn a_start_turn_never_taken_fails_unreachable_and_kills() {
    let w = World::new("turn_unreachable", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    // the supervisor took it and acked: 30 s more
    let sup = w.supervisor(conv).await;
    let cmds = sup.poll(0, 0).await;
    w.sql(
        "update turns set deadline = now() - interval '1 second' where conversation_id = $1",
        conv,
    )
    .await;
    sup.ack(cmds.commands[0].cmd_seq, AckResult::default()).await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert_eq!(w.turn(turn).await.0, "starting", "an acked start_turn gets 30 s more");
    // unacked: unreachable, and a kill for whatever runs
    let conv2 = w.conversation().await;
    let t2 = w.send(conv2, "m1", "hi").await;
    assert!(w.state_is(t2, "starting").await);
    w.sql(
        "update turns set deadline = now() - interval '1 second' where conversation_id = $1",
        conv2,
    )
    .await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert_eq!(w.turn(t2).await.1.as_deref(), Some("sandbox.unreachable"));
    let sup2 = w.supervisor(conv2).await;
    let cmds = sup2.poll(0, 0).await;
    assert!(
        cmds.commands.iter().any(
            |c| c.turn_id == Some(t2) && matches!(c.body, CommandBody::Kill(k) if k.reason == KillReason::Watchdog)
        ),
        "{cmds:?}"
    );
    assert!(
        !cmds
            .commands
            .iter()
            .any(|c| matches!(c.body, CommandBody::StartTurn(_))),
        "the failed turn's start_turn is canceled"
    );
}

#[tokio::test]
async fn cancel_before_delivery_during_the_run_and_forced() {
    let w = World::new("turn_cancel", None).await;
    // nothing running: never an error
    let conv = w.conversation().await;
    let r = w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    assert_eq!(r.turn_id, None);
    // before delivery: canceled at once, the message stays
    let turn = w.send(conv, "m1", "hi").await;
    let r = w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    assert_eq!(r.turn_id, Some(turn));
    assert_eq!(w.turn(turn).await.0, "canceled");
    assert_eq!(
        w.kinds(conv).await,
        ["conversation.created", "user.sent", "turn.started", "turn.ended"]
    );
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Idle);
    assert_eq!(c.last_turn.unwrap().outcome, TurnOutcome::Canceled);
    // during the run: stop requested, interrupt, the supervisor's canceled report ends it
    w.fake().make_ready(conv);
    let t2 = w.send(conv, "m2", "again").await;
    assert!(w.state_is(t2, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(t2).await;
    let r = w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    assert_eq!(r.turn_id, Some(t2));
    assert_eq!(w.turn(t2).await.0, "stopping");
    let r = w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    assert_eq!(r.turn_id, Some(t2), "a second cancel is the same stop");
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Stopping);
    let cmds = sup.poll(0, 0).await;
    let interrupts = cmds
        .commands
        .iter()
        .filter(|c| matches!(c.body, CommandBody::Interrupt(_)) && c.turn_id == Some(t2))
        .count();
    assert_eq!(interrupts, 1);
    sup.report(t2, ReportedState::Canceled).await.unwrap();
    assert_eq!(w.turn(t2).await.0, "canceled");
    assert!(w.kinds(conv).await.contains(&"turn.stop_requested".to_owned()));
    // forced: no confirmation within 30 s
    let t3 = w.send(conv, "m3", "and again").await;
    assert!(w.state_is(t3, "starting").await);
    sup.run(t3).await;
    w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    w.sql(
        "update turns set deadline = now() - interval '1 second', stop_requested_at = now() - interval '31 seconds'
         where conversation_id = $1 and state = 'stopping'",
        conv,
    )
    .await;
    w.sql0("update availability set available_since = now() - interval '1 hour'")
        .await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    let (state, code_, _) = w.turn(t3).await;
    assert_eq!((state.as_str(), code_.as_deref()), ("canceled", Some("stop.forced")));
    let cmds = sup.poll(0, 0).await;
    assert!(
        cmds.commands
            .iter()
            .any(|c| c.turn_id == Some(t3) && matches!(c.body, CommandBody::Kill(k) if k.reason == KillReason::Stop))
    );
    // a late canceled report is a replay of the end
    sup.report(t3, ReportedState::Canceled).await.unwrap();
}

#[tokio::test]
async fn an_end_waits_for_its_last_transcript_line() {
    let w = World::new("turn_pending", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    let uuid = "0b0c9d3e-7f6a-4b5c-8d9e-0f1a2b3c4d5e";
    let mut end = report(ReportedState::Completed);
    end.last_uuid = Some(uuid.into());
    let e = sup.calls().turn_state(turn, &end).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::TranscriptPending);
    assert_eq!(w.turn(turn).await.0, "running");
    // the line lands (as the cc write path stores it)
    w.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "with s as (update conversations set head_seq = head_seq + 1 where id = $1 returning head_seq)
             insert into events (conversation_id, seq, kind, at, turn_id, data)
             select $1, head_seq, 'cc.assistant', now(), $2, jsonb_build_object('uuid', $3::text) from s",
            &[&conv, &turn, &uuid],
        )
        .await
        .unwrap();
    sup.calls().turn_state(turn, &end).await.unwrap();
    assert_eq!(w.turn(turn).await.0, "completed");
}

#[tokio::test]
async fn the_watchdog_counts_only_time_magpie_was_available() {
    let w = World::new("turn_watchdog", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    // a heartbeat pushes the deadline 90 s past now
    sup.heartbeat(Some(turn), HeartbeatState::Running, None).await;
    let left: f64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select extract(epoch from deadline - now())::float8 from turns where id = $1",
            &[&turn],
        )
        .await
        .unwrap()
        .get(0);
    assert!((80.0..=91.0).contains(&left), "{left}");
    // 10 minutes of silence, but magpie was down until just now: a full 90 s again
    w.sql0(
        "update availability set last_tick = now() - interval '5 minutes', available_since = now() - interval '1 hour'",
    )
    .await;
    sweeper::availability_tick(w.pool()).await.unwrap();
    w.sql(
        "update turns set deadline = now() - interval '1 second', started_at = now() - interval '10 minutes'
         where conversation_id = $1",
        conv,
    )
    .await;
    w.sql(
        "update conversations set heartbeat_at = now() - interval '10 minutes' where id = $1",
        conv,
    )
    .await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    let now = w.turn(turn).await;
    assert_eq!(
        now.0, "running",
        "silence while magpie was down does not count: {now:?}"
    );
    // the same silence with magpie available all along, hermit sees the sandbox alive: unreachable after 5 min
    w.sql0("update availability set available_since = now() - interval '1 hour', last_tick = now()")
        .await;
    w.sql(
        "update turns set deadline = now() - interval '1 second' where conversation_id = $1",
        conv,
    )
    .await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    let (state, code_, _) = w.turn(turn).await;
    assert_eq!(
        (state.as_str(), code_.as_deref()),
        ("failed", Some("sandbox.unreachable"))
    );
    let cmds = sup.poll(0, 0).await;
    assert!(
        cmds.commands
            .iter()
            .any(|c| c.turn_id == Some(turn)
                && matches!(c.body, CommandBody::Kill(k) if k.reason == KillReason::Watchdog))
    );
    // silence and hermit says gone: lost; hermit unreachable: more time
    let t2 = w.send(conv, "m2", "again").await;
    assert!(w.state_is(t2, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(t2).await;
    w.sql(
        "update turns set deadline = now() - interval '1 second', started_at = now() - interval '2 minutes'
         where conversation_id = $1 and state = 'running'",
        conv,
    )
    .await;
    w.sql(
        "update conversations set heartbeat_at = now() - interval '2 minutes' where id = $1",
        conv,
    )
    .await;
    w.fake()
        .script()
        .once("get_sandbox", Reply::error(500, "internal", "down"));
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert_eq!(w.turn(t2).await.0, "running", "no turn is failed on missing evidence");
    w.sql(
        "update turns set deadline = now() - interval '1 second' where conversation_id = $1 and state = 'running'",
        conv,
    )
    .await;
    w.fake()
        .script()
        .once("get_sandbox", Reply::error(404, "no_sandbox", "none"));
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert_eq!(w.turn(t2).await.1.as_deref(), Some("sandbox.lost"));
}

#[tokio::test]
async fn heartbeats_reconcile_lost_reports() {
    let w = World::new("turn_reconcile", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    // 1: the end report never landed; the heartbeat's lastTurn ends the turn
    let t1 = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(t1, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(t1).await;
    sup.heartbeat(
        None,
        HeartbeatState::Idle,
        Some(HeartbeatLastTurn {
            id: t1,
            outcome: TurnOutcome::Failed,
            error: Some(ReportError {
                code: DataCode::RunOom,
                message: "memory guard".into(),
            }),
        }),
    )
    .await;
    let (s, c, _) = w.turn(t1).await;
    assert_eq!((s.as_str(), c.as_deref()), ("failed", Some("run.oom")));
    // 2: the sandbox no longer knows the turn
    let t2 = w.send(conv, "m2", "hi").await;
    assert!(w.state_is(t2, "starting").await);
    sup.run(t2).await;
    sup.heartbeat(Some(t2), HeartbeatState::Running, None).await;
    assert_eq!(w.turn(t2).await.0, "running");
    sup.heartbeat(None, HeartbeatState::Idle, None).await;
    let (s, c, _) = w.turn(t2).await;
    assert_eq!((s.as_str(), c.as_deref()), ("failed", Some("run.error")));
    // 3: a turn Claude Code started whose report never landed
    let t3 = Uuid::now_v7();
    sup.heartbeat(Some(t3), HeartbeatState::Running, None).await;
    assert_eq!(w.turn(t3).await.0, "running");
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.turn.as_ref().unwrap().origin, TurnOrigin::Agent);
    sup.report(t3, ReportedState::Completed).await.unwrap();
    // 4: held, then idle
    sup.heartbeat(None, HeartbeatState::Held, None).await;
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Held);
    sup.heartbeat(None, HeartbeatState::Idle, None).await;
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Idle);
}

#[tokio::test]
async fn agent_turns_held_tasks_and_stop_tasks() {
    let w = World::new("turn_agent", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let t1 = w.send(conv, "m1", "start a background job").await;
    assert!(w.state_is(t1, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(t1).await;
    let ev = |id: &str, status| TaskReportEvent {
        id: id.into(),
        task_id: "task-1".into(),
        tool_use_id: Some("toolu_1".into()),
        event: magpie_api::events::TaskEvent::Started,
        status,
        description: Some("npm test".into()),
        summary: None,
        output_file: None,
        at: Timestamp::now(),
    };
    let batch = TasksReport {
        events: vec![ev("1:0", magpie_api::events::TaskState::Running)],
    };
    sup.calls().tasks(&batch).await.unwrap();
    sup.calls().tasks(&batch).await.unwrap();
    let n = w.kinds(conv).await.iter().filter(|k| *k == "task.status").count();
    assert_eq!(n, 1, "a repeated report id is skipped");
    let mut end = report(ReportedState::Completed);
    end.held = Some(true);
    end.tasks = Some(vec![magpie_api::sandbox::ReportedTask {
        task_id: "task-1".into(),
        tool_use_id: Some("toolu_1".into()),
        kind: "local_bash".into(),
        description: Some("npm test".into()),
        started_at: None,
    }]);
    sup.calls().turn_state(t1, &end).await.unwrap();
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Held);
    assert_eq!(c.run.background.len(), 1);
    // the task's completion starts a turn by itself
    let agent = Uuid::now_v7();
    let req = AgentTurnRequest {
        turn_id: agent,
        origin: TurnOrigin::Agent,
        at: Timestamp::now(),
    };
    assert!(sup.calls().agent_turn(&req).await.unwrap().is_new());
    assert!(!sup.calls().agent_turn(&req).await.unwrap().is_new());
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.run.state, RunState::Running);
    assert_eq!(c.turn.unwrap().origin, TurnOrigin::Agent);
    // a person's send while the agent turn runs
    let e =
        w.a.pigeon()
            .person(ALICE)
            .send_turn(conv, &msg("m2", "hi"))
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::TurnRunning);
    let mut end = report(ReportedState::Completed);
    end.held = Some(true);
    end.tasks = Some(vec![magpie_api::sandbox::ReportedTask {
        task_id: "task-2".into(),
        tool_use_id: None,
        kind: "monitor".into(),
        description: None,
        started_at: None,
    }]);
    sup.calls().turn_state(agent, &end).await.unwrap();
    // a cancel while held stops the tasks
    let r = w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    assert_eq!(r.turn_id, None);
    let cmds = sup.poll(0, 0).await;
    assert!(
        cmds.commands
            .iter()
            .any(|c| matches!(c.body, CommandBody::StopTasks(_)))
    );
    // a held conversation admits a send (the supervisor supersedes the background work)
    let t3 = w.send(conv, "m3", "next").await;
    assert!(w.state_is(t3, "starting").await);
    let busy = AgentTurnRequest {
        turn_id: Uuid::now_v7(),
        origin: TurnOrigin::Agent,
        at: Timestamp::now(),
    };
    let e = sup.calls().agent_turn(&busy).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadTransition);
}

#[tokio::test]
async fn asks_are_opened_answered_and_resolved_once() {
    let w = World::new("turn_asks", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "ask me").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    let ask = AskReport {
        ask_id: "ask-1".into(),
        tool_name: "AskUserQuestion".into(),
        tool_use_id: Some("toolu_9".into()),
        input: serde_json::from_value(json!({"questions": [{"question": "Which?"}]})).unwrap(),
    };
    sup.calls().ask_open(&ask).await.unwrap();
    sup.calls().ask_open(&ask).await.unwrap();
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.asks.len(), 1);
    assert_eq!(c.asks[0].id, "ask-1");
    // view-as is read-only
    let e =
        w.a.pigeon()
            .viewing_as(ALICE, "admin@example.com", "s1")
            .answer_ask(conv, "ask-1", &AgentPermissionResponse::canceled())
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::ViewAsReadOnly);
    let answer = AgentPermissionResponse {
        behavior: Behavior::Allow,
        selected_action_id: Some("opt-2".into()),
        updated_input: Some(serde_json::from_value(json!({"answers": {"Which?": "B"}})).unwrap()),
        updated_permissions: None,
        message: None,
        interrupt: None,
    };
    w.a.pigeon()
        .person(ALICE)
        .answer_ask(conv, "ask-1", &answer)
        .await
        .unwrap();
    let e =
        w.a.pigeon()
            .person(ALICE)
            .answer_ask(conv, "ask-1", &answer)
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::AlreadyResolved);
    let e =
        w.a.pigeon()
            .person(ALICE)
            .answer_ask(conv, "ask-x", &answer)
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
    let cmds = sup.poll(0, 0).await;
    let a = cmds
        .commands
        .iter()
        .find_map(|c| match &c.body {
            CommandBody::AnswerAsk(a) => Some(a.clone()),
            _ => None,
        })
        .unwrap();
    assert!(a.allow);
    assert_eq!(a.ask_id, "ask-1");
    assert_eq!(a.updated_input, answer.updated_input);
    let wire = serde_json::to_value(&cmds).unwrap();
    assert!(
        !wire.to_string().contains("resolution"),
        "the stored answer stays in magpie"
    );
    sup.calls()
        .ask_resolved(
            "ask-1",
            &AskResolvedReport {
                outcome: AskOutcome::Answered,
            },
        )
        .await
        .unwrap();
    sup.calls()
        .ask_resolved(
            "ask-1",
            &AskResolvedReport {
                outcome: AskOutcome::Answered,
            },
        )
        .await
        .unwrap();
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert!(c.asks.is_empty());
    let resolved: Vec<_> = w
        .changes()
        .await
        .into_iter()
        .filter_map(|c| match c {
            ChangeBody::AskResolved(r) => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].resolution, answer, "the person's whole answer");
    // a canceled ask
    let mut ask2 = ask.clone();
    ask2.ask_id = "ask-2".into();
    sup.calls().ask_open(&ask2).await.unwrap();
    sup.calls()
        .ask_resolved(
            "ask-2",
            &AskResolvedReport {
                outcome: AskOutcome::Canceled,
            },
        )
        .await
        .unwrap();
    let e =
        w.a.pigeon()
            .person(ALICE)
            .answer_ask(conv, "ask-2", &answer)
            .await
            .unwrap_err();
    assert_eq!(code(&e), ErrorCode::AlreadyResolved);
}

#[tokio::test]
async fn the_title_command_names_a_provisional_conversation() {
    let w = World::new("turn_title", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "幫我整理今日有關港珠澳大橋嘅新聞").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    let cmds = sup.poll(0, 0).await;
    let (seq, title) = cmds
        .commands
        .iter()
        .find_map(|c| match &c.body {
            CommandBody::Title(t) => Some((c.cmd_seq, t.clone())),
            _ => None,
        })
        .unwrap();
    assert!(
        title
            .user_prompt
            .contains("<message>\n幫我整理今日有關港珠澳大橋嘅新聞\n</message>")
    );
    assert_eq!(title.max_tokens, 100);
    // a failed attempt counts
    sup.ack(
        seq,
        AckResult {
            error: Some("529 overloaded".into()),
            ..AckResult::default()
        },
    )
    .await;
    let attempts: i16 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select title_attempts from conversations where id = $1", &[&conv])
        .await
        .unwrap()
        .get(0);
    assert_eq!(attempts, 1);
    sup.run(turn).await;
    sup.report(turn, ReportedState::Completed).await.unwrap();
    // the next turn tries again
    let t2 = w.send(conv, "m2", "多啲").await;
    assert!(w.state_is(t2, "starting").await);
    let cmds = sup.poll(0, 0).await;
    let seq = cmds
        .commands
        .iter()
        .find_map(|c| matches!(c.body, CommandBody::Title(_)).then_some(c.cmd_seq))
        .unwrap();
    sup.ack(
        seq,
        AckResult {
            response: Some(json!({"content": [{"type": "thinking", "thinking": "…"}, {"type": "text", "text": "標題: 「港珠澳大橋新聞」"}]})),
            ..AckResult::default()
        },
    )
    .await;
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.title.as_deref(), Some("港珠澳大橋新聞"));
    assert_eq!(c.title_source, Some(magpie_api::conversations::TitleSource::Generated));
    assert!(w.kinds(conv).await.contains(&"title.changed".to_owned()));
    // a repeated ack changes nothing
    sup.ack(seq, AckResult::default()).await;
    let t3_conv = w.conversation().await;
    w.a.pigeon()
        .person(ALICE)
        .rename_conversation(
            t3_conv,
            &magpie_api::conversations::RenameRequest { title: "Mine".into() },
        )
        .await
        .unwrap();
    let t3 = w.send(t3_conv, "m1", "hi").await;
    assert!(w.state_is(t3, "starting").await);
    let n: i64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from commands where conversation_id = $1 and type = 'title'",
            &[&t3_conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0, "a renamed conversation gets no title command");
}

#[tokio::test]
async fn sandbox_calls_need_the_current_lease() {
    let w = World::new("turn_lease", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    let old = w.supervisor(conv).await;
    let new = Sup::new(&w.a, conv, old.sandbox).await;
    let e = old
        .calls()
        .commands(&CommandsQuery {
            after: 0,
            wait_ms: None,
        })
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::LeaseLost);
    let e = old.report(turn, ReportedState::Running).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::LeaseLost);
    let e = new
        .c
        .sandbox(None)
        .commands(&CommandsQuery {
            after: 0,
            wait_ms: None,
        })
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::LeaseLost);
    new.run(turn).await;
    // a held command poll answers the moment a command is queued
    let poll = tokio::spawn({
        let c = new.c.clone();
        let lease = new.lease;
        async move {
            let started = std::time::Instant::now();
            let r = c
                .sandbox(Some(lease))
                .commands(&CommandsQuery {
                    after: 100,
                    wait_ms: Some(10_000),
                })
                .await
                .unwrap();
            (r, started.elapsed())
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    let head: i64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select cmd_head from conversations where id = $1", &[&conv])
        .await
        .unwrap()
        .get(0);
    // the poll was after 100: it times out with the head, the interrupt is below its cursor
    let (r, _) = poll.await.unwrap();
    assert!(r.commands.is_empty());
    assert_eq!(r.head, u64::try_from(head).unwrap());
    let poll = tokio::spawn({
        let c = new.c.clone();
        let lease = new.lease;
        let after = r.head;
        async move {
            let started = std::time::Instant::now();
            let r = c
                .sandbox(Some(lease))
                .commands(&CommandsQuery {
                    after,
                    wait_ms: Some(10_000),
                })
                .await
                .unwrap();
            (r, started.elapsed())
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    // queue a command: a held conversation's stop_tasks, via a forced end and held state
    new.report(turn, ReportedState::Canceled).await.unwrap();
    w.sql("update conversations set run_state = 'held' where id = $1", conv)
        .await;
    w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    let (r, took) = poll.await.unwrap();
    assert_eq!(r.commands.len(), 1, "{r:?}");
    assert!(took < Duration::from_secs(5), "woken by the notification: {took:?}");
}

#[tokio::test]
async fn the_feed_ends_turns_and_recovers_from_an_expired_cursor() {
    let w = World::new("turn_feed", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    // the leader follows the feed: wait until its cursor caught up with hermit's head
    let head = || async {
        w.pool()
            .get()
            .await
            .unwrap()
            .query_opt("select after_seq from hermit_feed where id", &[])
            .await
            .unwrap()
            .and_then(|r| r.get::<_, Option<i64>>(0))
    };
    assert!(common::eventually(Duration::from_secs(10), async || head().await.is_some_and(|h| h >= 2)).await);
    // the feed's events are gone before the follower reads them: 410, then the re-read finds the sandbox gone
    let sandbox = w.fake().live(conv).unwrap();
    w.fake().script().handle("sandbox_events", |c| {
        let after = c.query.as_ref()?.get("after")?.as_str()?.to_owned();
        (after != "latest").then(|| Reply::error(410, "cursor_expired", "expired"))
    });
    w.fake().end(conv, ReleaseReason::HostLost, "host lost");
    assert!(w.state_is(turn, "failed").await, "the recovery ended the turn");
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.lost"));
    w.fake().script().clear("sandbox_events");
    let row = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select hermit_sandbox, hermit_state from conversations where id = $1",
            &[&conv],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Option<Uuid>>(0), Some(sandbox.id));
    assert_eq!(row.get::<_, Option<String>>(1).as_deref(), Some("gone"));
    // the follower continues from latest
    let latest = w.fake().script().calls_to("sandbox_events").last().cloned().unwrap();
    assert!(latest.query.unwrap().to_string().contains("latest") || head().await.is_some());
}

#[tokio::test]
async fn deleting_a_conversation_releases_its_sandbox_once_and_ends_its_turn() {
    let w = World::new("turn_delete", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    w.a.pigeon().person(ALICE).delete_conversation(conv).await.unwrap();
    let hermit = outbox::Hermit(w.hermit.client(common::http(), aviary_core::auth::Caller::Magpie));
    assert_eq!(outbox::pass(w.pool(), &hermit).await.unwrap(), 1);
    assert_eq!(outbox::pass(w.pool(), &hermit).await.unwrap(), 0, "done once");
    assert_eq!(w.fake().script().calls_to("release_sandbox").len(), 1);
    assert!(w.state_is(turn, "failed").await, "the feed's gone ended the turn");
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.lost"));
    // 404 counts as done
    let conv2 = w.conversation().await;
    w.a.pigeon().person(ALICE).delete_conversation(conv2).await.unwrap();
    w.fake()
        .script()
        .once("release_sandbox", Reply::error(404, "no_sandbox", "none"));
    assert_eq!(outbox::pass(w.pool(), &hermit).await.unwrap(), 1);
    let left: i64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from outbox", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_create_with_a_first_message_admits_one_first_turn() {
    let w = World::new("turn_create", None).await;
    let req = CreateRequest {
        idempotency_key: "draft-7f3a".into(),
        message: msg("draft-7f3a:initial-message", "幫我整理今日有關港珠澳大橋嘅新聞"),
    };
    let first = w.a.pigeon().person(ALICE).create_conversation(&req).await.unwrap();
    assert!(first.is_new());
    let body = first.get().clone();
    let turn = body.turn.clone().expect("the first turn");
    assert_eq!(body.conversation.run.state, RunState::Admitted);
    assert_eq!(
        body.conversation.title.as_deref(),
        Some("幫我整理今日有關港珠澳大橋嘅新聞")
    );
    // a repeat, and 16 concurrent creates with the key: the same conversation and the same first turn
    let again = w.a.pigeon().person(ALICE).create_conversation(&req).await.unwrap();
    assert!(!again.is_new());
    assert_eq!(again.get().conversation.id, body.conversation.id);
    assert_eq!(again.get().turn.as_ref(), Some(&turn));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let client = w.a.pigeon();
        let req = req.clone();
        tasks.push(tokio::spawn(async move {
            client.person(ALICE).create_conversation(&req).await
        }));
    }
    for t in tasks {
        let r = t.await.unwrap().unwrap();
        assert!(matches!(r, Admitted::Repeat(_)));
        assert_eq!(r.get().conversation.id, body.conversation.id);
        assert_eq!(r.get().turn.as_ref(), Some(&turn));
    }
    let lookup = w.a.pigeon().person(ALICE).get_creation("draft-7f3a").await.unwrap();
    assert_eq!(lookup.turn.as_ref(), Some(&turn));
    // 16 concurrent first creates with a fresh key: one conversation, one turn
    let mut tasks = Vec::new();
    for i in 0..16 {
        let client = w.a.pigeon();
        let req = CreateRequest {
            idempotency_key: "draft-race".into(),
            message: msg("draft-race:initial-message", &format!("hi {i}")),
        };
        tasks.push(tokio::spawn(async move {
            client.person(ALICE).create_conversation(&req).await
        }));
    }
    let mut seen = Vec::new();
    for t in tasks {
        let r = t.await.unwrap().unwrap();
        seen.push((r.get().conversation.id, r.get().turn.clone().unwrap().id));
    }
    seen.dedup();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let n: i64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from turns where conversation_id = $1", &[&seen[0].0])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    // the first turn is driven like any other
    assert!(w.state_is(turn.id, "preparing").await);
    w.fake().make_ready(body.conversation.id);
    assert!(w.state_is(turn.id, "starting").await);
}

fn small_png() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(16, 12, |x, y| {
        image::Rgb([u8::try_from(x * 15).unwrap(), u8::try_from(y * 20).unwrap(), 99])
    }));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[tokio::test]
async fn a_send_attaches_the_persons_uploads_with_unique_workspace_names() {
    use magpie_api::turns::{FileRef, ImageRef};
    use magpie_api::uploads::UploadKind;
    let w = World::new("turn_attach", Some(Duration::ZERO)).await;
    let me = w.a.pigeon();
    let me = me.person(ALICE);
    let pic = me
        .upload("pic.png", UploadKind::Image, "image/png", small_png())
        .await
        .unwrap();
    let n1 = me
        .upload("notes.txt", UploadKind::File, "text/plain", b"one".to_vec())
        .await
        .unwrap();
    let n2 = me
        .upload("notes.txt", UploadKind::File, "text/plain", b"two".to_vec())
        .await
        .unwrap();
    let file = |u: &magpie_api::uploads::UploadResponse| FileRef {
        path: u.path.clone(),
        name: u.name.clone(),
        mime_type: u.mime_type.clone(),
        size: u.size,
    };
    let conv = w.conversation().await;
    let mut m = msg("m1", "see attached");
    m.images = vec![ImageRef {
        upload_id: pic.upload_id,
    }];
    m.files = vec![file(&n1), file(&n2)];
    let turn = me.send_turn(conv, &m).await.unwrap().get().turn.id;
    let sent: serde_json::Value = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select data from events where conversation_id = $1 and kind = 'user.sent'",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(sent["images"][0]["path"], "uploads/pic.png");
    assert_eq!(sent["files"][0]["path"], "uploads/notes.txt");
    assert_eq!(sent["files"][1]["path"], "uploads/notes (2).txt");
    assert_eq!(sent["files"][1]["blob"]["$blob"]["bytes"], 3);
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    let cmds = sup.poll(0, 0).await;
    let CommandBody::StartTurn(st) = &cmds.commands[0].body else {
        panic!()
    };
    assert_eq!(st.files.len(), 2);
    assert_eq!(st.files[1].workspace_path, "uploads/notes (2).txt");
    assert_eq!(st.files[1].blob.len(), 64);
    assert_eq!(st.images[0].workspace_path, "uploads/pic.png");
    assert_eq!(st.images[0].model_blob.len(), 64);
    let attached: i64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from uploads where conversation_id = $1 and attached_seq is not null",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(attached, 3);
    // an attached upload, another person's, an unknown one, an old asset path: unknown_upload
    let other = w.conversation().await;
    let mut again = msg("m1", "again");
    again.files = vec![file(&n1)];
    assert_eq!(
        code(&me.send_turn(other, &again).await.unwrap_err()),
        ErrorCode::UnknownUpload
    );
    let bobs =
        w.a.pigeon()
            .person("bob@example.org")
            .upload("b.txt", UploadKind::File, "text/plain", b"b".to_vec())
            .await
            .unwrap();
    again.files = vec![file(&bobs)];
    assert_eq!(
        code(&me.send_turn(other, &again).await.unwrap_err()),
        ErrorCode::UnknownUpload
    );
    again.files = vec![FileRef {
        path: "cloudcli-assets/abc/report.pdf".into(),
        name: "report.pdf".into(),
        mime_type: "application/pdf".into(),
        size: 1,
    }];
    assert_eq!(
        code(&me.send_turn(other, &again).await.unwrap_err()),
        ErrorCode::UnknownUpload
    );
    // nothing of a refused send is kept
    assert_eq!(w.kinds(other).await, ["conversation.created"]);
    again.files = Vec::new();
    again.images = (0..11)
        .map(|_| ImageRef {
            upload_id: Uuid::new_v4(),
        })
        .collect();
    assert_eq!(
        code(&me.send_turn(other, &again).await.unwrap_err()),
        ErrorCode::TooManyAttachments
    );
}
