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
        self.take(turn).await;
        let mut running = report(ReportedState::Running);
        running.provider_session_id = Some("279aabf0-1111-4222-8333-444455556666".into());
        running.transcript_path = Some("claude/projects/-workspace/279aabf0-1111-4222-8333-444455556666.jsonl".into());
        self.calls().turn_state(turn, &running).await.unwrap();
    }

    /// Takes the turn's `start_turn` and acks it (no `running` report).
    async fn take(&self, turn: Uuid) {
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
    }
}

fn report(state: ReportedState) -> TurnStateReport {
    TurnStateReport {
        state,
        at: Timestamp::now(),
        harness: magpie_api::Harness::default(),
        provider_session_id: None,
        transcript_path: None,
        exit_code: None,
        error: None,
        held: None,
        tasks: None,
        last_uuid: None,
        usage: None,
        duration_ms: None,
        commands: None,
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
    // exactly one starts a turn; every other joins that turn as a mid-turn message (src/turns/midturn.rs)
    let mut ok = Vec::new();
    let mut joined = Vec::new();
    for t in tasks {
        let r = t.await.unwrap().unwrap();
        if r.get().mid_turn {
            joined.push(r);
        } else {
            ok.push(r);
        }
    }
    assert_eq!(ok.len(), 1, "exactly one send starts a turn");
    let first = ok[0].get().turn.clone();
    assert_eq!(joined.len(), 15);
    assert!(
        joined.iter().all(|r| r.get().turn.id == first.id),
        "the others joined it"
    );
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
    assert_eq!(
        w.turn(turn).await.1.as_deref(),
        Some("sandbox.failed"),
        "a turn that never ran fails as a start that failed (8.3 step 2)"
    );
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
    // the turn's driver runs on a replica that is then killed (its task aborted: no drain hands the claim back)
    w.a.start_drain();
    w.sql(
        "update turns set driver_until = now() - interval '1 second' where conversation_id = $1",
        conv,
    )
    .await;
    let dead = "magpie-dead-replica";
    let dying = Driver {
        pod: dead.to_owned(),
        ..driver(&b)
    };
    let task = tokio::spawn(async move { magpie::turns::driver::drive(&dying, turn).await });
    let driver_is = async |pod: &str| {
        w.pool()
            .get()
            .await
            .unwrap()
            .query_one("select driver, driver_until > now() from turns where id = $1", &[&turn])
            .await
            .map(|r| r.get::<_, Option<String>>(0).as_deref() == Some(pod) && r.get::<_, Option<bool>>(1) == Some(true))
            .unwrap()
    };
    assert!(
        common::eventually(Duration::from_secs(10), async || driver_is(dead).await).await,
        "the doomed replica claimed the turn"
    );
    task.abort();
    let _ = task.await;
    assert!(driver_is(dead).await, "a killed replica's claim stays until it lapses");
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
    // and more again, but not for ever: 5 min after the ack with nothing heard, unreachable
    w.sql(
        "update turns set deadline = now() - interval '1 second' where conversation_id = $1",
        conv,
    )
    .await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert_eq!(w.turn(turn).await.0, "starting");
    w.sql(
        "update commands set acked_at = now() - interval '6 minutes' where conversation_id = $1 and type = 'start_turn'",
        conv,
    )
    .await;
    w.sql0("update availability set available_since = now() - interval '1 hour'")
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
    assert!(
        sup.poll(0, 0)
            .await
            .commands
            .iter()
            .any(|c| c.turn_id == Some(turn)
                && matches!(c.body, CommandBody::Kill(k) if k.reason == KillReason::Watchdog))
    );
    // unacked: unreachable 120 s after ready, and a kill for whatever runs
    let conv2 = w.conversation().await;
    let t2 = w.send(conv2, "m1", "hi").await;
    assert!(w.state_is(t2, "starting").await);
    w.sql(
        "update turns set deadline = now() - interval '1 second', ready_at = now() - interval '3 minutes'
         where conversation_id = $1",
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
    // that sandbox goes before its supervisor took the kill: the next sandbox's supervisor never receives it
    let gone = w.fake().live(conv2).unwrap().id;
    w.fake().end(conv2, ReleaseReason::HostLost, "host lost");
    assert!(
        common::eventually(Duration::from_secs(10), async || w.fake().live(conv2).is_none()).await,
        "hermit ended it"
    );
    let t3 = w.send(conv2, "m2", "again").await;
    assert!(w.state_is(t3, "starting").await);
    let sup3 = w.supervisor(conv2).await;
    assert_ne!(sup3.sandbox, gone);
    let cmds = sup3.poll(0, 0).await;
    assert!(
        !cmds.commands.iter().any(|c| matches!(c.body, CommandBody::Kill(_))),
        "a kill of the gone sandbox's turn is not served: {cmds:?}"
    );
    assert!(
        cmds.commands
            .iter()
            .any(|c| c.turn_id == Some(t3) && matches!(c.body, CommandBody::StartTurn(_)))
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
    // a starting turn whose start_turn was acknowledged is reconciled too: its running report was lost ...
    let t4 = w.send(conv, "m4", "hi").await;
    assert!(w.state_is(t4, "starting").await);
    sup.heartbeat(None, HeartbeatState::Idle, None).await;
    assert_eq!(
        w.turn(t4).await.0,
        "starting",
        "an unacknowledged start_turn is not the supervisor's yet"
    );
    sup.take(t4).await;
    sup.heartbeat(Some(t4), HeartbeatState::Running, None).await;
    assert_eq!(w.turn(t4).await.0, "running", "turnId == T is the running report");
    sup.report(t4, ReportedState::Completed).await.unwrap();
    // ... or both its reports were lost ...
    let t5 = w.send(conv, "m5", "hi").await;
    assert!(w.state_is(t5, "starting").await);
    sup.take(t5).await;
    sup.heartbeat(
        None,
        HeartbeatState::Idle,
        Some(HeartbeatLastTurn {
            id: t5,
            outcome: TurnOutcome::Completed,
            error: None,
        }),
    )
    .await;
    assert_eq!(w.turn(t5).await.0, "completed", "lastTurn.id == T ends it");
    // ... or the supervisor restarted without it
    let t6 = w.send(conv, "m6", "hi").await;
    assert!(w.state_is(t6, "starting").await);
    sup.take(t6).await;
    sup.heartbeat(None, HeartbeatState::Idle, None).await;
    let (s, c, _) = w.turn(t6).await;
    assert_eq!((s.as_str(), c.as_deref()), ("failed", Some("run.error")));
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
    // a person's send while the agent turn runs joins it (src/turns/midturn.rs): no turn of its own
    let joined =
        w.a.pigeon()
            .person(ALICE)
            .send_turn(conv, &msg("m2", "hi"))
            .await
            .unwrap()
            .into_inner();
    assert!(joined.mid_turn);
    assert_eq!(joined.turn.id, agent);
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

/// A background launch's tool row ends when magpie stops tracking its task without a terminal report: a canceled
/// turn whose supervisor no longer holds the task, and a held task the supervisor's heartbeat no longer has.
#[tokio::test]
async fn a_background_launch_whose_task_never_reports_ends_with_its_tracking() {
    use magpie_api::events::{TaskEvent, TaskState};
    use magpie_api::rows::{RowItem, ToolStatus};
    let w = World::new("turn_task_lost", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let first = w.send(conv, "m0", "run it in the background").await;
    assert!(w.state_is(first, "starting").await);
    let sup = w.supervisor(conv).await;
    let status = async |call: &str| -> ToolStatus {
        let p =
            w.a.pigeon()
                .person(ALICE)
                .timeline(conv, &common::ev::tail(50))
                .await
                .unwrap();
        p.rows
            .iter()
            .find_map(|r| match &r.item {
                RowItem::Tool(t) if t.call_id == call => Some(t.status),
                _ => None,
            })
            .unwrap()
    };
    let started = |n: u32, call: &str| TasksReport {
        events: vec![TaskReportEvent {
            id: format!("{call}:{n}"),
            task_id: format!("task-{call}"),
            tool_use_id: Some(call.into()),
            event: TaskEvent::Started,
            status: TaskState::Running,
            description: None,
            summary: None,
            output_file: None,
            at: Timestamp::now(),
        }],
    };
    let held = |call: &str| magpie_api::sandbox::ReportedTask {
        task_id: format!("task-{call}"),
        tool_use_id: Some(call.into()),
        kind: "local_bash".into(),
        description: None,
        started_at: None,
    };
    for (round, call) in ["toolu_a", "toolu_b"].into_iter().enumerate() {
        let t = if round == 0 {
            first
        } else {
            let t = w.send(conv, &format!("m{round}"), "run it in the background").await;
            assert!(w.state_is(t, "starting").await);
            t
        };
        sup.run(t).await;
        common::ev::append(
            w.a.pool(),
            ALICE,
            conv,
            vec![common::ev::in_turn(
                common::ev::assistant(&format!("msg_{call}"), json!([common::ev::tool_use(call, "Bash")])),
                t,
            )],
        )
        .await;
        sup.calls().tasks(&started(0, call)).await.unwrap();
        assert_eq!(status(call).await, ToolStatus::Running);
        if round == 0 {
            // canceled; Claude Code never writes the notification and the supervisor no longer holds the task
            sup.calls()
                .turn_state(t, &report(ReportedState::Canceled))
                .await
                .unwrap();
            assert_eq!(
                status(call).await,
                ToolStatus::Canceled,
                "the turn's end stops tracking the task"
            );
        } else {
            // canceled while the task still runs: held, the row stays running ...
            let mut end = report(ReportedState::Canceled);
            end.held = Some(true);
            end.tasks = Some(vec![held(call)]);
            sup.calls().turn_state(t, &end).await.unwrap();
            assert_eq!(status(call).await, ToolStatus::Running);
            // ... until the supervisor goes idle with no report of the task
            sup.heartbeat(None, HeartbeatState::Idle, None).await;
            assert_eq!(
                status(call).await,
                ToolStatus::Canceled,
                "an idle heartbeat stops tracking it"
            );
        }
    }
    // a late report of the task still has the last word
    let done = TasksReport {
        events: vec![TaskReportEvent {
            id: "toolu_a:done".into(),
            task_id: "task-toolu_a".into(),
            tool_use_id: Some("toolu_a".into()),
            event: TaskEvent::Notification,
            status: TaskState::Completed,
            description: None,
            summary: Some("finished".into()),
            output_file: None,
            at: Timestamp::now(),
        }],
    };
    sup.calls().tasks(&done).await.unwrap();
    assert_eq!(status("toolu_a").await, ToolStatus::Completed);
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
        .rename_conversation(t3_conv, &magpie_api::conversations::RenameRequest::title("Mine"))
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
    let after_of = |c: &aviary_fakes::script::Call| {
        c.query
            .as_ref()
            .and_then(|q| q.get("after"))
            .and_then(|a| a.as_str())
            .map(str::to_owned)
    };
    let installed = w.fake().script().calls_to("sandbox_events").len();
    w.fake().script().handle("sandbox_events", |c| {
        let after = c.query.as_ref()?.get("after")?.as_str()?.to_owned();
        (after != "latest").then(|| Reply::error(410, "cursor_expired", "expired"))
    });
    w.fake().end(conv, ReleaseReason::HostLost, "host lost");
    // the long-poll in flight when the script was installed may still deliver the gone event (it was recorded
    // before the script), so the turn ends either from that page or from the recovery's re-read
    assert!(w.state_is(turn, "failed").await, "the gone sandbox ended the turn");
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.lost"));
    // either way the next numeric long-poll is answered 410 and the follower recovers from `latest`
    assert!(
        common::eventually(Duration::from_secs(10), async || {
            let calls = w.fake().script().calls_to("sandbox_events");
            let expired = calls
                .iter()
                .skip(installed)
                .position(|c| after_of(c).is_some_and(|a| a != "latest"));
            expired.is_some_and(|i| {
                calls[installed + i..]
                    .iter()
                    .any(|c| after_of(c).as_deref() == Some("latest"))
            })
        })
        .await,
        "an expired numeric cursor was followed by a recovery"
    );
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
    // the follower re-read hermit's head (`latest`), stored it as the cursor, and continues after it by number
    let calls = w.fake().script().calls_to("sandbox_events");
    let recovery = calls
        .iter()
        .rposition(|c| after_of(c).as_deref() == Some("latest"))
        .expect("the recovery asked for latest");
    assert!(
        calls[..recovery]
            .iter()
            .any(|c| after_of(c).is_some_and(|a| a != "latest")),
        "a numeric cursor expired before the recovery"
    );
    let hermit_head =
        w.a.app()
            .hermit
            .sandbox_events(hermit_api::sandbox::SandboxEventsQuery {
                after: hermit_api::sandbox::Cursor::Latest,
                limit: None,
                wait_ms: None,
            })
            .await
            .unwrap()
            .head_seq;
    assert!(
        common::eventually(Duration::from_secs(10), async || head().await
            == Some(i64::try_from(hermit_head).unwrap()))
        .await,
        "the cursor is the head hermit gave for latest"
    );
    let want = hermit_head.to_string();
    assert!(
        common::eventually(Duration::from_secs(10), async || {
            w.fake()
                .script()
                .calls_to("sandbox_events")
                .iter()
                .skip(recovery + 1)
                .any(|c| after_of(c).as_deref() == Some(want.as_str()))
        })
        .await,
        "the next long-poll continues after that head by number"
    );
}

#[tokio::test]
async fn deleting_a_conversation_releases_its_sandbox_once_and_ends_its_turn() {
    let w = World::new("turn_delete", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    // the leader follows hermit's feed from `latest` on its first start: a `gone` before that is never seen (on a
    // busy CI runner the follower started after the release, and the turn did not end within the wait)
    assert!(
        common::eventually(Duration::from_secs(10), async || {
            w.pool()
                .get()
                .await
                .unwrap()
                .query_opt("select after_seq from hermit_feed where id", &[])
                .await
                .unwrap()
                .and_then(|r| r.get::<_, Option<i64>>(0))
                .is_some()
        })
        .await,
        "the feed follower started"
    );
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
        harness: magpie_api::Harness::default(),
        model: None,
        thinking: None,
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
            harness: magpie_api::Harness::default(),
            model: None,
            thinking: None,
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

#[tokio::test]
async fn a_reacquire_late_in_the_wait_gets_a_fresh_deadline() {
    let w = World::new("turn_late_reacquire", None).await;
    // the feed's path: a turn admitted 16 min ago (its sandbox was ready, start_turn unacknowledged) loses the sandbox
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await);
    let first = w.fake().live(conv).unwrap().id;
    w.sql(
        "update turns set created_at = now() - interval '16 minutes' where conversation_id = $1",
        conv,
    )
    .await;
    w.fake().end(conv, ReleaseReason::Recycle, "recycled");
    assert!(
        common::eventually(Duration::from_secs(10), async || w.turn(turn).await.0 == "preparing").await,
        "the feed sent the turn back to preparing"
    );
    assert!(sweeper::pass(&driver(&w.a)).await.unwrap() >= 1);
    assert!(
        common::eventually(Duration::from_secs(10), async || {
            w.fake().live(conv).is_some_and(|s| s.id != first)
        })
        .await,
        "the sweeper's driver asked hermit for a new sandbox"
    );
    assert_eq!(w.turn(turn).await.0, "preparing", "not failed sandbox.timeout");
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await);
}

#[tokio::test]
async fn a_driver_that_reacquires_near_its_deadline_keeps_waiting() {
    let w = World::new("turn_reacquire_deadline", None).await;
    let b = w.replica().await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    let before = w.fake().live(conv).unwrap().id;
    // admitted 14 min 57 s ago, due in 3 s: replica a stops, b's driver claims the turn with that deadline
    w.a.start_drain();
    tokio::time::sleep(Duration::from_millis(200)).await;
    w.sql(
        "update turns set driver = null, driver_until = null, created_at = now() - interval '897 seconds',
                deadline = now() + interval '3 seconds'
         where conversation_id = $1",
        conv,
    )
    .await;
    magpie::turns::driver::spawn(driver(&b), turn);
    let b_pod = b.app().pod.clone();
    assert!(
        common::eventually(Duration::from_secs(5), async || {
            let r = w
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
    w.fake().end(conv, ReleaseReason::HostLost, "host lost");
    assert!(
        common::eventually(Duration::from_secs(10), async || {
            w.fake().live(conv).is_some_and(|s| s.id != before)
        })
        .await,
        "the driver asked hermit for a new sandbox"
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    let (state, code_, _) = w.turn(turn).await;
    assert_eq!(
        (state.as_str(), code_),
        ("preparing", None),
        "past the first deadline, the replacement's counts"
    );
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await);
}

#[tokio::test]
async fn an_end_before_running_needs_the_start_turn_delivered() {
    let w = World::new("turn_early_end", None).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    let sup = w.supervisor(conv).await;
    let e = sup.report(turn, ReportedState::Failed).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadTransition, "no supervisor has the turn yet");
    assert_eq!(w.turn(turn).await.0, "preparing");
    // the supervisor took start_turn and could not start Claude Code (supervisor 7.9)
    let r = sup.poll(0, 0).await;
    assert!(r.commands.iter().any(|c| c.turn_id == Some(turn)));
    let mut failed = report(ReportedState::Failed);
    failed.error = Some(ReportError {
        code: DataCode::RunError,
        message: "Claude Code would not start".into(),
    });
    sup.calls().turn_state(turn, &failed).await.unwrap();
    let (s, c, _) = w.turn(turn).await;
    assert_eq!((s.as_str(), c.as_deref()), ("failed", Some("run.error")));
}

#[tokio::test]
async fn a_cancel_and_a_command_poll_at_once_agree_on_the_delivery() {
    let w = World::new("turn_cancel_race", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let first = w.send(conv, "m0", "hi").await;
    assert!(w.state_is(first, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(first).await;
    sup.report(first, ReportedState::Completed).await.unwrap();
    for i in 0..40 {
        let turn = w.send(conv, &format!("m{}", i + 1), "again").await;
        let pigeon = w.a.pigeon();
        let person = pigeon.person(ALICE);
        let (polled, canceled) = tokio::join!(sup.poll(0, 0), person.cancel_turn(conv));
        assert_eq!(canceled.unwrap().turn_id, Some(turn));
        let delivered = polled
            .commands
            .iter()
            .any(|c| c.turn_id == Some(turn) && matches!(c.body, CommandBody::StartTurn(_)));
        let state = w.turn(turn).await.0;
        if delivered {
            // the supervisor runs it: the cancel must stop it with an interrupt, not end it at once
            assert_eq!(
                state, "stopping",
                "round {i}: a delivered start_turn's turn ended at once"
            );
            let interrupts = sup.poll(0, 0).await;
            assert!(
                interrupts
                    .commands
                    .iter()
                    .any(|c| c.turn_id == Some(turn) && matches!(c.body, CommandBody::Interrupt(_))),
                "round {i}"
            );
            sup.report(turn, ReportedState::Canceled).await.unwrap();
        } else {
            assert_eq!(state, "canceled", "round {i}");
            let later = sup.poll(0, 0).await;
            assert!(
                !later
                    .commands
                    .iter()
                    .any(|c| c.turn_id == Some(turn) && matches!(c.body, CommandBody::StartTurn(_))),
                "round {i}: a turn canceled at once never reaches the supervisor"
            );
        }
    }
}

/// `projects/-workspace`, as Claude Code creates it.
async fn project_dir(sup: &Sup) {
    for path in ["claude/projects", "claude/projects/-workspace"] {
        sup.calls()
            .cc_mkdir(&magpie_api::sandbox::CcMkdir {
                path: path.to_owned(),
                mode: 0o700,
                mtime_ns: 1,
                op_id: Uuid::new_v4(),
            })
            .await
            .unwrap();
    }
}

async fn one(w: &World, q: &str, conv: Uuid) -> aviary_pg::Row {
    w.pool().get().await.unwrap().query_one(q, &[&conv]).await.unwrap()
}

#[tokio::test]
async fn a_starting_turn_waits_out_an_outage_after_ready() {
    let w = World::new("turn_start_outage", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    // ready 3 min ago, then magpie (or its database) was down for 150 s: the supervisor's poll could not take
    // start_turn, and the first sweep after the outage starts a new available period
    w.sql(
        "update turns set ready_at = now() - interval '3 minutes', deadline = now() - interval '1 second'
         where conversation_id = $1",
        conv,
    )
    .await;
    w.sql0("update availability set last_tick = now() - interval '150 seconds', available_since = now() - interval '1 hour'")
        .await;
    sweeper::availability_tick(w.pool()).await.unwrap();
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert_eq!(w.turn(turn).await.0, "starting", "the outage does not count");
    let left: f64 = one(
        &w,
        "select extract(epoch from deadline - now())::float8 from turns where conversation_id = $1",
        conv,
    )
    .await
    .get(0);
    assert!(
        (100.0..=121.0).contains(&left),
        "120 s from the end of the outage: {left}"
    );
    let kills: i64 = one(
        &w,
        "select count(*) from commands where conversation_id = $1 and type = 'kill'",
        conv,
    )
    .await
    .get(0);
    assert_eq!(kills, 0, "no kill was queued");
    // the supervisor takes it once magpie answers
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    assert_eq!(w.turn(turn).await.0, "running");
}

/// The app's `/` completion (Deyao, 2026-11-12): a `running` report may carry the slash commands the conversation's
/// Claude Code named; magpie keeps the newest list and gives it to the conversation's person only.
#[tokio::test]
async fn a_running_report_carries_the_slash_commands_the_person_reads() {
    use magpie_api::sandbox::SlashCommand;
    let w = World::new("turn_commands", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let pigeon = w.a.pigeon();
    let alice = pigeon.person(ALICE);
    assert!(
        alice.conversation_commands(conv).await.unwrap().commands.is_empty(),
        "none before its Claude Code ran"
    );
    let turn = w.send(conv, "m1", "hi").await;
    let sup = w.supervisor(conv).await;
    sup.take(turn).await;
    let command = |name: &str, description: &str| SlashCommand {
        name: name.into(),
        description: description.into(),
        argument_hint: String::new(),
    };
    let mut running = report(ReportedState::Running);
    running.provider_session_id = Some("279aabf0-1111-4222-8333-444455556666".into());
    running.transcript_path = Some("claude/projects/-workspace/279aabf0-1111-4222-8333-444455556666.jsonl".into());
    running.commands = Some(vec![
        command("compact", "Keep a summary"),
        command("retire-session", &"x".repeat(1000)),
        command("", "no name"),
    ]);
    sup.calls().turn_state(turn, &running).await.unwrap();
    let list = alice.conversation_commands(conv).await.unwrap().commands;
    assert_eq!(
        list.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["compact", "retire-session"]
    );
    assert_eq!(list[1].description.chars().count(), 300, "a long description is cut");
    // a report that names none leaves the list; the conversation is not someone else's to read
    let mut again = report(ReportedState::Running);
    again.provider_session_id = running.provider_session_id.clone();
    sup.calls().turn_state(turn, &again).await.unwrap();
    assert_eq!(alice.conversation_commands(conv).await.unwrap().commands.len(), 2);
    let e = pigeon
        .person("bob@example.org")
        .conversation_commands(conv)
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::NotFound));
}

#[tokio::test]
async fn a_late_running_report_keeps_the_session_and_transcript() {
    let w = World::new("turn_late_running", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.take(turn).await;
    // Claude Code writes its transcript before magpie knows the path
    let sid = "279aabf0-1111-4222-8333-444455556666";
    let path = format!("projects/-workspace/{sid}.jsonl");
    let line = format!(
        "{}\n",
        json!({"type": "assistant", "uuid": "a1", "message": {"id": "msg_a1", "model": "m-1",
               "content": [{"type": "text", "text": "hello"}], "usage": {"input_tokens": 3}}})
    );
    let q = |offset: u64| magpie_api::sandbox::CcWriteQuery {
        path: format!("claude/{path}"),
        offset,
        mtime_ns: 1,
        op_id: Uuid::new_v4(),
        create: true,
        mode: 0o600,
        turn_id: Some(turn),
        append: false,
    };
    project_dir(&sup).await;
    sup.calls().cc_write(&q(0), line.clone().into_bytes()).await.unwrap();
    // the running report is late: the heartbeat reconciles the turn running without the session
    sup.heartbeat(Some(turn), HeartbeatState::Running, None).await;
    assert_eq!(w.turn(turn).await.0, "running");
    let facts = async || -> (Option<String>, Option<String>) {
        let r = w
            .pool()
            .get()
            .await
            .unwrap()
            .query_one(
                "select provider_session_id, main_transcript from conversations where id = $1",
                &[&conv],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!(facts().await, (None, None));
    // the real report lands after all: its session and path count, and the earlier line is promoted (6.2)
    let mut running = report(ReportedState::Running);
    running.provider_session_id = Some(sid.into());
    running.transcript_path = Some(format!("claude/{path}"));
    sup.calls().turn_state(turn, &running).await.unwrap();
    assert_eq!(facts().await, (Some(sid.to_owned()), Some(path.clone())));
    assert!(
        w.kinds(conv).await.contains(&"cc.assistant".to_owned()),
        "{:?}",
        w.kinds(conv).await
    );
    let promoted: i64 = one(
        &w,
        "select count(*) from events where conversation_id = $1 and kind = 'cc.assistant'",
        conv,
    )
    .await
    .get(0);
    assert_eq!(promoted, 1);
    // a replay changes nothing
    sup.calls().turn_state(turn, &running).await.unwrap();
    assert_eq!(facts().await, (Some(sid.to_owned()), Some(path.clone())));
    sup.report(turn, ReportedState::Completed).await.unwrap();
    // both reports of the next turn lost but its end: the end's session and path count too
    let t2 = w.send(conv, "m2", "again").await;
    assert!(w.state_is(t2, "starting").await);
    sup.take(t2).await;
    sup.heartbeat(Some(t2), HeartbeatState::Running, None).await;
    let sid2 = "379aabf0-1111-4222-8333-444455556666";
    let mut end = report(ReportedState::Completed);
    end.provider_session_id = Some(sid2.into());
    end.transcript_path = Some(format!("claude/projects/-workspace/{sid2}.jsonl"));
    sup.calls().turn_state(t2, &end).await.unwrap();
    assert_eq!(w.turn(t2).await.0, "completed");
    assert_eq!(
        facts().await,
        (Some(sid2.to_owned()), Some(format!("projects/-workspace/{sid2}.jsonl")))
    );
}

#[tokio::test]
async fn an_agent_turn_refused_while_a_person_turn_waits_writes_into_that_turn() {
    let w = World::new("turn_agent_refused", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let t0 = w.send(conv, "m0", "hi").await;
    assert!(w.state_is(t0, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(t0).await;
    sup.report(t0, ReportedState::Completed).await.unwrap();
    // a person turn is admitted; before its start_turn reaches the supervisor, Claude Code wakes up by itself
    w.fake().make_ready(conv);
    let person = w.send(conv, "m1", "next").await;
    assert!(w.state_is(person, "starting").await);
    let agent = Uuid::now_v7();
    let e = sup
        .calls()
        .agent_turn(&AgentTurnRequest {
            turn_id: agent,
            origin: TurnOrigin::Agent,
            at: Timestamp::now(),
        })
        .await
        .unwrap_err();
    assert_eq!(code(&e), ErrorCode::BadTransition);
    // the lines the supervisor writes under the refused turn's id belong to the active turn (magpie 6.3)
    let path = "projects/-workspace/279aabf0-1111-4222-8333-444455556666.jsonl";
    project_dir(&sup).await;
    let line = format!(
        "{}\n",
        json!({"type": "assistant", "uuid": "w1", "message": {"id": "msg_w1", "model": "m-1",
               "content": [{"type": "text", "text": "woke up"}], "usage": {"input_tokens": 3}}})
    );
    sup.calls()
        .cc_write(
            &magpie_api::sandbox::CcWriteQuery {
                path: format!("claude/{path}"),
                offset: 0,
                mtime_ns: 1,
                op_id: Uuid::new_v4(),
                create: true,
                mode: 0o600,
                turn_id: Some(agent),
                append: false,
            },
            line.into_bytes(),
        )
        .await
        .unwrap();
    let owner: Option<Uuid> = one(
        &w,
        "select turn_id from events where conversation_id = $1 and kind = 'cc.assistant' and data->>'uuid' = 'w1'",
        conv,
    )
    .await
    .get(0);
    assert_eq!(owner, Some(person));
    // the refused turn's end is not magpie's: 404 (the supervisor drops it as settled, supervisor README)
    let e = sup.report(agent, ReportedState::Completed).await.unwrap_err();
    assert_eq!(code(&e), ErrorCode::NotFound);
    assert_eq!(w.turn(person).await.0, "starting");
    let known: i64 = one(&w, "select count(*) from turns where conversation_id = $1", conv)
        .await
        .get(0);
    assert_eq!(known, 2, "no turn row for the refused agent turn");
}

#[tokio::test]
async fn a_forced_stop_never_waits_behind_hermit_and_replicas_split_the_sweep() {
    use aviary_fakes::fault::{FaultKind, Hold};
    let w = World::new("turn_sweep_split", Some(Duration::ZERO)).await;
    let b = w.replica().await;
    // one running turn whose silence needs hermit, one stopping turn due for stop.forced
    let c1 = w.conversation().await;
    let r = w.send(c1, "m1", "hi").await;
    assert!(w.state_is(r, "starting").await);
    let s1 = w.supervisor(c1).await;
    s1.run(r).await;
    let c2 = w.conversation().await;
    let s = w.send(c2, "m1", "hi").await;
    assert!(w.state_is(s, "starting").await);
    let s2 = w.supervisor(c2).await;
    s2.run(s).await;
    w.a.pigeon().person(ALICE).cancel_turn(c2).await.unwrap();
    assert_eq!(w.turn(s).await.0, "stopping");
    let hold = Hold::new();
    w.fake().faults().times("get_sandbox", FaultKind::Hold(hold.clone()), 1);
    let calls_before = w.fake().script().calls_to("get_sandbox").len();
    w.sql0("update availability set available_since = now() - interval '1 hour', last_tick = now()")
        .await;
    w.sql0("update conversations set heartbeat_at = now() - interval '2 minutes'")
        .await;
    w.sql0(
        "update turns set deadline = now() - interval '1 second', started_at = now() - interval '2 minutes',
                          stop_requested_at = now() - interval '31 seconds'
          where state in ('running', 'stopping')",
    )
    .await;
    let d = driver(&w.a);
    let sweeping = tokio::spawn(async move { sweeper::pass(&d).await.unwrap() });
    assert!(
        hold.wait_held(1, Duration::from_secs(5)).await,
        "the running check asks hermit"
    );
    // hermit hangs on the running turn: the stop was already forced
    let (state, code_, _) = w.turn(s).await;
    assert_eq!((state.as_str(), code_.as_deref()), ("canceled", Some("stop.forced")));
    // another replica's sweep finds the running turn claimed and asks hermit nothing
    assert_eq!(sweeper::pass(&driver(&b)).await.unwrap(), 0);
    assert_eq!(hold.waiting(), 1);
    hold.release();
    sweeping.await.unwrap();
    assert_eq!(w.turn(r).await.0, "running", "hermit unreachable: more time");
    assert!(
        w.fake().script().calls_to("get_sandbox").len() <= calls_before + 1,
        "one hermit call for the running turn"
    );
}

#[tokio::test]
async fn the_feed_recovery_keeps_its_cursor_until_every_reread_answered() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let w = World::new("turn_feed_reread", Some(Duration::ZERO)).await;
    let mut runs = Vec::new();
    for _ in 0..2 {
        let conv = w.conversation().await;
        let turn = w.send(conv, "m1", "hi").await;
        assert!(w.state_is(turn, "starting").await);
        let sup = w.supervisor(conv).await;
        sup.run(turn).await;
        runs.push((conv, turn));
    }
    let cursor = async || -> Option<i64> {
        w.pool()
            .get()
            .await
            .unwrap()
            .query_opt("select after_seq from hermit_feed where id", &[])
            .await
            .unwrap()
            .and_then(|r| r.get::<_, Option<i64>>(0))
    };
    assert!(common::eventually(Duration::from_secs(10), async || cursor().await.is_some_and(|h| h >= 4)).await);
    let (a, b) = (runs[0], runs[1]);
    // the feed expires, and hermit does not answer the re-read of b's sandbox
    let failing = Arc::new(AtomicBool::new(true));
    let f = failing.clone();
    let b_id = b.0.to_string();
    w.fake().script().handle("get_sandbox", move |c| {
        (f.load(Ordering::SeqCst) && c.params.get("cid").and_then(|v| v.as_str()) == Some(b_id.as_str()))
            .then(|| Reply::error(500, "internal", "down"))
    });
    let numeric = || {
        w.fake()
            .script()
            .calls_to("sandbox_events")
            .iter()
            .filter(|c| {
                c.query
                    .as_ref()
                    .and_then(|q| q.get("after"))
                    .and_then(|a| a.as_str())
                    .is_some_and(|a| a != "latest")
            })
            .count()
    };
    let polls = numeric();
    w.fake().script().handle("sandbox_events", |c| {
        let after = c.query.as_ref()?.get("after")?.as_str()?.to_owned();
        (after != "latest").then(|| Reply::error(410, "cursor_expired", "expired"))
    });
    // an event of a third conversation answers the long-poll in flight; the next one gets 410
    let c3 = w.conversation().await;
    w.send(c3, "m1", "hi").await;
    assert!(common::eventually(Duration::from_secs(10), async || numeric() > polls).await);
    let before = cursor().await;
    // both sandboxes go while the follower recovers
    w.fake().end(a.0, ReleaseReason::HostLost, "host lost");
    w.fake().end(b.0, ReleaseReason::HostLost, "host lost");
    assert!(w.state_is(a.1, "failed").await, "a's re-read ended a's turn");
    // b's gone is not lost: the cursor stays and the recovery runs again
    let recoveries = || {
        w.fake()
            .script()
            .calls_to("get_sandbox")
            .iter()
            .filter(|c| c.params.get("cid").and_then(|v| v.as_str()) == Some(b.0.to_string().as_str()))
            .count()
    };
    assert!(
        common::eventually(Duration::from_secs(15), async || recoveries() >= 2).await,
        "b was re-read again"
    );
    assert_eq!(w.turn(b.1).await.0, "running");
    assert_eq!(cursor().await, before, "the cursor did not move past b's gone");
    failing.store(false, Ordering::SeqCst);
    assert!(
        common::eventually(Duration::from_secs(15), async || w.turn(b.1).await.0 == "failed").await,
        "the next recovery ended b's turn"
    );
    assert_eq!(w.turn(b.1).await.1.as_deref(), Some("sandbox.lost"));
    w.fake().script().clear("sandbox_events");
    assert!(
        common::eventually(Duration::from_secs(10), async || cursor().await != before).await,
        "then the cursor moves to hermit's head"
    );
}

#[tokio::test]
async fn sandboxes_that_keep_dying_end_the_turn_45_minutes_after_admission() {
    let w = World::new("turn_reacquire_cap", None).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "preparing").await);
    w.fake().make_ready(conv);
    assert!(w.state_is(turn, "starting").await);
    // admitted 44 min ago: the replacement gets 1 min, not 15
    w.sql(
        "update turns set created_at = now() - interval '44 minutes' where conversation_id = $1",
        conv,
    )
    .await;
    w.fake().end(conv, ReleaseReason::HostLost, "host lost");
    assert!(
        common::eventually(Duration::from_secs(10), async || w.turn(turn).await.0 == "preparing").await,
        "the feed sent the turn back to preparing"
    );
    let left: f64 = one(
        &w,
        "select extract(epoch from deadline - now())::float8 from turns where conversation_id = $1",
        conv,
    )
    .await
    .get(0);
    assert!((0.0..=61.0).contains(&left), "capped at admission + 45 min: {left}");
    // past the cap, the next loss times the turn out
    w.sql(
        "update turns set created_at = now() - interval '50 minutes', deadline = now() - interval '1 second'
         where conversation_id = $1",
        conv,
    )
    .await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert!(w.state_is(turn, "failed").await);
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("sandbox.timeout"));
}

#[tokio::test]
async fn a_start_turn_delivered_after_a_reacquire_records_the_callers_sandbox() {
    let w = World::new("turn_delivery_sandbox", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "hi").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    // the window after a re-acquire: the turn records no sandbox yet when the new supervisor takes start_turn
    w.sql("update turns set sandbox_id = null where conversation_id = $1", conv)
        .await;
    sup.take(turn).await;
    let recorded: Option<Uuid> = one(&w, "select sandbox_id from turns where conversation_id = $1", conv)
        .await
        .get(0);
    assert_eq!(recorded, Some(sup.sandbox));
    // a cancel before its running report, then a forced stop: the kill reaches that supervisor
    w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    w.sql0("update availability set available_since = now() - interval '1 hour', last_tick = now()")
        .await;
    w.sql(
        "update turns set deadline = now() - interval '1 second', stop_requested_at = now() - interval '31 seconds'
         where conversation_id = $1",
        conv,
    )
    .await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert_eq!(w.turn(turn).await.1.as_deref(), Some("stop.forced"));
    let cmds = sup.poll(0, 0).await;
    assert!(
        cmds.commands
            .iter()
            .any(|c| c.turn_id == Some(turn) && matches!(c.body, CommandBody::Kill(k) if k.reason == KillReason::Stop)),
        "{cmds:?}"
    );
}

#[tokio::test]
async fn a_drivers_answer_never_records_another_sandbox_over_the_feeds() {
    use hermit_api::sandbox::SandboxState;
    use magpie::turns::driver::{Seen, record_sandbox};
    let w = World::new("turn_record_order", None).await;
    let conv = w.conversation().await;
    let (x, y) = (Uuid::new_v4(), Uuid::new_v4());
    let record = |sandbox: Uuid, state: SandboxState, version: u64, seen: Seen| {
        let pool = w.pool().clone();
        async move {
            aviary_pg::tx!(&pool, |t| -> magpie::error::Result<()> {
                record_sandbox(t, conv, sandbox, state, version, seen).await
            })
            .await
            .unwrap();
        }
    };
    let recorded = async || -> (Option<Uuid>, Option<String>, Option<i64>) {
        let r = w
            .pool()
            .get()
            .await
            .unwrap()
            .query_one(
                "select hermit_sandbox, hermit_state, hermit_version from conversations where id = $1",
                &[&conv],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1), r.get(2))
    };
    w.sql(
        "update conversations set hermit_sandbox = null, hermit_state = null, hermit_version = null where id = $1",
        conv,
    )
    .await;
    // nothing recorded: a driver's answer is recorded
    record(x, SandboxState::Starting, 1, Seen::Answer).await;
    assert_eq!(recorded().await, (Some(x), Some("starting".into()), Some(1)));
    // the feed records the replacement y; x's late answer (a higher version of x's own row) does not replace it
    record(y, SandboxState::Starting, 1, Seen::Feed).await;
    record(x, SandboxState::Ready, 5, Seen::Answer).await;
    assert_eq!(recorded().await, (Some(y), Some("starting".into()), Some(1)));
    // y's own answers still advance it
    record(y, SandboxState::Ready, 2, Seen::Answer).await;
    assert_eq!(recorded().await, (Some(y), Some("ready".into()), Some(2)));
    record(y, SandboxState::Starting, 1, Seen::Answer).await;
    assert_eq!(recorded().await.2, Some(2), "an older version of y is ignored");
}

// ---- DECISIONS 17: a question nobody answers never holds a sandbox ----

fn ask_report(id: &str, tool_use: &str) -> AskReport {
    AskReport {
        ask_id: id.into(),
        tool_name: "AskUserQuestion".into(),
        tool_use_id: Some(tool_use.into()),
        input: serde_json::from_value(json!({"questions": [{"question": "Which?"}]})).unwrap(),
    }
}

#[allow(clippy::needless_pass_by_value)] // json! literals
fn allow(answers: serde_json::Value) -> AgentPermissionResponse {
    AgentPermissionResponse {
        behavior: Behavior::Allow,
        selected_action_id: None,
        updated_input: Some(serde_json::from_value(json!({"answers": answers})).unwrap()),
        updated_permissions: None,
        message: None,
        interrupt: None,
    }
}

fn parked(ms: u64) -> TurnStateReport {
    let mut r = report(ReportedState::Parked);
    r.duration_ms = Some(ms);
    r
}

/// The turn is neither ended nor failed when its sandbox goes; the question stays on the person's screen; the
/// answer is recorded at once and admits the turn again, whose `start_turn` (in a new sandbox) resumes the session
/// with the answer; the turn then completes, its working time the two parts without the wait.
#[tokio::test]
async fn a_parked_turn_keeps_its_question_and_its_answer_resumes_it_in_a_new_sandbox() {
    let w = World::new("turn_parked", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "ask me").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    let first = sup.sandbox;
    sup.run(turn).await;
    sup.calls().ask_open(&ask_report("ask-1", "toolu_9")).await.unwrap();
    sup.calls().turn_state(turn, &parked(1_200)).await.unwrap();
    sup.calls().turn_state(turn, &parked(1_200)).await.unwrap();
    assert_eq!(w.turn(turn).await.0, "parked");
    // the sandbox goes (hermit's idle release): the feed leaves the parked turn alone
    w.fake().end(conv, ReleaseReason::Idle, "idle");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(w.turn(turn).await.0, "parked", "neither ended nor failed");
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.asks.len(), 1, "the question is still on screen");
    assert_eq!(c.turn.as_ref().map(|t| t.id), Some(turn));
    assert_eq!(c.run.state, RunState::Running);
    // no deadline runs while it waits: the sweeper finds nothing
    tokio::time::sleep(Duration::from_millis(500)).await;
    sweeper::pass(&driver(&w.a)).await.unwrap();
    assert_eq!(w.turn(turn).await.0, "parked");
    let answer = allow(json!({"Which?": "B"}));
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
    // a new sandbox, whose supervisor gets the resumption
    assert!(w.state_is(turn, "starting").await);
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert!(c.asks.is_empty());
    let wait = c.turn.as_ref().unwrap().ask_wait_ms;
    assert!(wait >= 2_500, "the wait is counted apart: {wait}");
    let sup2 = w.supervisor(conv).await;
    assert_ne!(sup2.sandbox, first);
    let cmds = sup2.poll(0, 5_000).await;
    let (seq, start) = cmds
        .commands
        .iter()
        .find_map(|c| match &c.body {
            CommandBody::StartTurn(s) if c.turn_id == Some(turn) && !s.answers.is_empty() => {
                Some((c.cmd_seq, s.clone()))
            }
            _ => None,
        })
        .expect("the resumption's start_turn");
    assert_eq!(
        start.resume.provider_session_id.as_deref(),
        Some("279aabf0-1111-4222-8333-444455556666")
    );
    assert_eq!(start.answers.len(), 1);
    let a = &start.answers[0];
    assert_eq!(
        (a.ask_id.as_str(), a.tool_use_id.as_deref(), a.allow),
        ("ask-1", Some("toolu_9"), true)
    );
    assert_eq!(a.updated_input, answer.updated_input);
    assert_eq!(a.input["questions"][0]["question"], "Which?");
    sup2.ack(
        seq,
        AckResult {
            delivered: Some(true),
            ..AckResult::default()
        },
    )
    .await;
    sup2.report(turn, ReportedState::Running).await.unwrap();
    let mut done = report(ReportedState::Completed);
    done.duration_ms = Some(800);
    sup2.calls().turn_state(turn, &done).await.unwrap();
    assert_eq!(w.turn(turn).await.0, "completed");
    let ms: Option<i64> = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select duration_ms from turns where id = $1", &[&turn])
        .await
        .unwrap()
        .get(0);
    assert_eq!(ms, Some(2_000), "the working time of both parts, the wait left out");
    let kinds = w.kinds(conv).await;
    assert_eq!(kinds.iter().filter(|k| *k == "turn.ended").count(), 1);
    assert_eq!(kinds.iter().filter(|k| *k == "ask.resolved").count(), 1);
}

/// A cancel of a parked turn ends it at once and closes its question.
#[tokio::test]
async fn a_parked_turn_is_canceled_at_once() {
    let w = World::new("turn_parked_cancel", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "ask me").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    sup.calls().ask_open(&ask_report("ask-1", "toolu_9")).await.unwrap();
    sup.calls().turn_state(turn, &parked(10)).await.unwrap();
    let r = w.a.pigeon().person(ALICE).cancel_turn(conv).await.unwrap();
    assert_eq!(r.turn_id, Some(turn));
    assert_eq!(w.turn(turn).await.0, "canceled");
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert!(c.asks.is_empty() && c.turn.is_none());
    assert_eq!(c.run.state, RunState::Idle);
}

/// An answer queued for the supervisor while it was already letting the question go is the person's answer all
/// the same: the `parked` report finds it and the turn resumes at once.
#[tokio::test]
async fn an_answer_that_crossed_the_park_resumes_the_turn() {
    let w = World::new("turn_parked_race", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    let turn = w.send(conv, "m1", "ask me").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.run(turn).await;
    sup.calls().ask_open(&ask_report("ask-1", "toolu_9")).await.unwrap();
    w.a.pigeon()
        .person(ALICE)
        .answer_ask(conv, "ask-1", &allow(json!({"Which?": "A"})))
        .await
        .unwrap();
    sup.calls().turn_state(turn, &parked(10)).await.unwrap();
    let (state, _, _) = w.turn(turn).await;
    assert!(
        ["admitted", "preparing", "starting"].contains(&state.as_str()),
        "{state}"
    );
    let pending: i64 = w
        .pool()
        .get()
        .await
        .unwrap()
        .query_one(
            "select count(*) from commands where conversation_id = $1 and type = 'answer_ask' and canceled_at is null",
            &[&conv],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        pending, 0,
        "the answer goes with the resumption, not to the old supervisor"
    );
    let kinds = w.kinds(conv).await;
    assert!(kinds.contains(&"ask.resolved".to_owned()));
}

/// The model a person picks names the harness its turns run in: a turn's start says which, and resumes that
/// harness's own session. The session of the harness that ran before is kept for a pick back.
#[tokio::test]
async fn a_turns_start_names_the_picked_models_harness_and_resumes_its_session() {
    use magpie_api::Harness;
    use magpie_api::conversations::RenameRequest;
    let w = World::new("turn_harness", Some(Duration::ZERO)).await;
    let start = |cmds: &CommandsResponse, turn: Uuid| -> (Harness, Option<String>) {
        let st = cmds
            .commands
            .iter()
            .find_map(|c| match &c.body {
                CommandBody::StartTurn(st) if c.turn_id == Some(turn) => Some(st.clone()),
                _ => None,
            })
            .expect("start_turn");
        (st.harness, st.resume.provider_session_id)
    };
    let claude_session = "279aabf0-1111-4222-8333-444455556666";
    let conv = w.conversation().await;
    let me = w.a.pigeon();
    // nothing picked: Claude Code, a new session
    let turn = w.send(conv, "m1", "one").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    assert_eq!(start(&sup.poll(0, 0).await, turn), (Harness::Claude, None));
    sup.run(turn).await;
    sup.report(turn, ReportedState::Completed).await.unwrap();
    assert!(w.state_is(turn, "completed").await);

    // a model of the other harness: the conversation says so, and the next turn starts Qwen Code afresh
    let pick = |model: &str, harness: Option<Harness>| RenameRequest {
        model: Some(model.into()),
        harness,
        ..Default::default()
    };
    let picked = me
        .person(ALICE)
        .rename_conversation(conv, &pick("vendor/video", Some(Harness::Qwen)))
        .await
        .unwrap();
    assert_eq!(picked.harness, Harness::Qwen);
    let turn = w.send(conv, "m2", "two").await;
    assert!(w.state_is(turn, "starting").await);
    assert_eq!(start(&sup.poll(0, 0).await, turn), (Harness::Qwen, None));
    sup.take(turn).await;
    let mut running = report(ReportedState::Running);
    running.harness = Harness::Qwen;
    running.provider_session_id = Some("q-1".into());
    // Claude Code's place for a transcript is not Qwen Code's, in either home
    for wrong in [
        "claude/projects/-workspace/q-1.jsonl",
        "qwen/projects/-workspace/q-1.jsonl",
    ] {
        running.transcript_path = Some(wrong.into());
        assert!(sup.calls().turn_state(turn, &running).await.is_err());
    }
    running.transcript_path = Some("qwen/projects/-workspace/chats/q-1.jsonl".into());
    sup.calls().turn_state(turn, &running).await.unwrap();
    sup.report(turn, ReportedState::Completed).await.unwrap();
    assert!(w.state_is(turn, "completed").await);

    // back to a Claude Code model (a pick that names no harness): its session of the first turn
    let picked = me
        .person(ALICE)
        .rename_conversation(conv, &pick("vendor/other", None))
        .await
        .unwrap();
    assert_eq!(picked.harness, Harness::Claude);
    let turn = w.send(conv, "m3", "three").await;
    assert!(w.state_is(turn, "starting").await);
    assert_eq!(
        start(&sup.poll(0, 0).await, turn),
        (Harness::Claude, Some(claude_session.into()))
    );
    sup.run(turn).await;
    sup.report(turn, ReportedState::Completed).await.unwrap();
    assert!(w.state_is(turn, "completed").await);

    // and Qwen Code again: the session it had
    me.person(ALICE)
        .rename_conversation(conv, &pick("vendor/video", Some(Harness::Qwen)))
        .await
        .unwrap();
    let turn = w.send(conv, "m4", "four").await;
    assert!(w.state_is(turn, "starting").await);
    assert_eq!(start(&sup.poll(0, 0).await, turn), (Harness::Qwen, Some("q-1".into())));
}

/// A turn in Qwen Code: its transcript, written under Qwen Code's home in Qwen Code's own record shape, makes the
/// same events and timeline rows a Claude Code transcript does: the person's message once (its echo is no second
/// prompt), the reply's text, the tool call with its result, and none of Qwen Code's own bookkeeping records.
#[tokio::test]
async fn a_qwen_code_turn_shows_on_the_timeline_like_any_other() {
    use magpie_api::Harness;
    use magpie_api::conversations::RenameRequest;
    let w = World::new("turn_qwen", Some(Duration::ZERO)).await;
    let conv = w.conversation().await;
    w.a.pigeon()
        .person(ALICE)
        .rename_conversation(
            conv,
            &RenameRequest {
                model: Some("vendor/video".into()),
                harness: Some(Harness::Qwen),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let turn = w.send(conv, "m1", "cut the video").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    sup.take(turn).await;
    let path = "qwen/projects/-workspace/chats/q-1.jsonl";
    let mut running = report(ReportedState::Running);
    running.harness = Harness::Qwen;
    running.provider_session_id = Some("q-1".into());
    running.transcript_path = Some(path.into());
    sup.calls().turn_state(turn, &running).await.unwrap();
    for dir in [
        "qwen/projects",
        "qwen/projects/-workspace",
        "qwen/projects/-workspace/chats",
    ] {
        sup.calls()
            .cc_mkdir(&magpie_api::sandbox::CcMkdir {
                path: dir.to_owned(),
                mode: 0o700,
                mtime_ns: 1,
                op_id: Uuid::new_v4(),
            })
            .await
            .unwrap();
    }
    // as Qwen Code 0.27.3 writes them
    let lines = [
        json!({"uuid": "u1", "parentUuid": null, "sessionId": "q-1", "type": "user", "provenance": "real_user",
               "message": {"role": "user", "parts": [{"text": "cut the video"}]}}),
        json!({"uuid": "s1", "parentUuid": "u1", "sessionId": "q-1", "type": "system", "provenance": "system",
               "subtype": "ui_telemetry", "systemPayload": {"uiEvent": {"v": 1}}}),
        json!({"uuid": "a1", "parentUuid": "s1", "sessionId": "q-1", "type": "assistant", "model": "vendor/video",
               "message": {"role": "model", "parts": [
                   {"text": "I should look at the clip.", "thought": true},
                   {"text": "Let me look."},
                   {"functionCall": {"id": "call-1", "name": "run_shell_command", "args": {"command": "ls videos"}}}]},
               "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 9, "cachedContentTokenCount": 0}}),
        json!({"uuid": "r1", "parentUuid": "a1", "sessionId": "q-1", "type": "tool_result",
               "message": {"role": "user", "parts": [{"functionResponse": {"id": "call-1", "name": "run_shell_command",
                   "response": {"output": "a.mp4"}}}]},
               "toolCallResult": {"callId": "call-1", "status": "success"}}),
        json!({"uuid": "a2", "parentUuid": "r1", "sessionId": "q-1", "type": "assistant", "model": "vendor/video",
               "message": {"role": "model", "parts": [{"text": "Done: one clip."}]},
               "usageMetadata": {"promptTokenCount": 130, "candidatesTokenCount": 5, "cachedContentTokenCount": 100}}),
    ];
    let mut offset = 0u64;
    for l in &lines {
        let bytes = format!("{l}\n").into_bytes();
        let n = bytes.len() as u64;
        sup.calls()
            .cc_write(
                &magpie_api::sandbox::CcWriteQuery {
                    path: path.to_owned(),
                    offset,
                    mtime_ns: 1,
                    op_id: Uuid::new_v4(),
                    create: true,
                    mode: 0o600,
                    turn_id: Some(turn),
                    append: false,
                },
                bytes,
            )
            .await
            .unwrap();
        offset += n;
    }
    let mut end = report(ReportedState::Completed);
    end.last_uuid = Some("a2".into());
    sup.calls().turn_state(turn, &end).await.unwrap();
    assert!(w.state_is(turn, "completed").await);

    // the events: the two replies and the tool result; the echo and the telemetry record make none
    let kinds = w.kinds(conv).await;
    let count = |k: &str| kinds.iter().filter(|x| x.as_str() == k).count();
    assert_eq!((count("cc.assistant"), count("cc.user")), (2, 1), "{kinds:?}");
    // the rows a person sees
    let rows: Vec<(String, serde_json::Value)> = w
        .pool()
        .get()
        .await
        .unwrap()
        .query(
            "select kind, item from rows where conversation_id = $1 order by row_id",
            &[&conv],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let of =
        |k: &str| -> Vec<&serde_json::Value> { rows.iter().filter(|(kind, _)| kind == k).map(|(_, i)| i).collect() };
    assert_eq!(of("user").len(), 1, "the person's message once: {rows:?}");
    let texts: Vec<&str> = of("assistant").iter().filter_map(|i| i["text"].as_str()).collect();
    assert_eq!(texts, ["Let me look.", "Done: one clip."], "{rows:?}");
    let tools = of("tool");
    assert_eq!(tools.len(), 1, "{rows:?}");
    assert_eq!(tools[0]["name"], "run_shell_command");
    assert_eq!(tools[0]["input"]["command"], "ls videos");
    assert_eq!(tools[0]["status"], "completed", "{}", tools[0]);
    // the conversation's model and context are the reply's
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!(c.model.as_deref(), Some("vendor/video"));
    assert_eq!(c.context_tokens, Some(130));
}

#[tokio::test]
async fn a_turns_start_carries_the_conversations_picked_thinking_level() {
    let w = World::new("turn_thinking", Some(Duration::ZERO)).await;
    let start = |cmds: &CommandsResponse| -> (Option<String>, Option<String>) {
        let st = cmds
            .commands
            .iter()
            .find_map(|c| match &c.body {
                CommandBody::StartTurn(st) => Some(st.clone()),
                _ => None,
            })
            .expect("start_turn");
        (st.model, st.thinking)
    };
    // nothing picked: the start names neither a model nor a level
    let conv = w.conversation().await;
    let c = w.a.pigeon().person(ALICE).get_conversation(conv).await.unwrap();
    assert_eq!((c.picked_model, c.picked_thinking), (None, None));
    let turn = w.send(conv, "m1", "one").await;
    assert!(w.state_is(turn, "starting").await);
    let sup = w.supervisor(conv).await;
    assert_eq!(start(&sup.poll(0, 0).await), (None, None));
    sup.run(turn).await;
    sup.report(turn, ReportedState::Completed).await.unwrap();
    assert!(w.state_is(turn, "completed").await);

    // the person picks a level (M5, without a rename or a model): on the conversation, in the next turn's start
    let me = w.a.pigeon();
    let picked = me
        .person(ALICE)
        .rename_conversation(conv, &magpie_api::conversations::RenameRequest::thinking(" max "))
        .await
        .unwrap();
    assert_eq!(
        (
            picked.picked_model.as_deref(),
            picked.picked_thinking.as_deref(),
            picked.title_source
        ),
        (None, Some("max"), None),
        "a level is no rename and no model"
    );
    let turn = w.send(conv, "m2", "two").await;
    assert!(w.state_is(turn, "starting").await);
    assert_eq!(start(&sup.poll(0, 0).await), (None, Some("max".into())));
    sup.run(turn).await;
    sup.report(turn, ReportedState::Completed).await.unwrap();
    assert!(w.state_is(turn, "completed").await);

    // a model and a level together; then the level alone back to none picked, the model untouched
    let both = me
        .person(ALICE)
        .rename_conversation(
            conv,
            &magpie_api::conversations::RenameRequest {
                model: Some("vendor/other".into()),
                thinking: Some("low".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (both.picked_model.as_deref(), both.picked_thinking.as_deref()),
        (Some("vendor/other"), Some("low"))
    );
    let cleared = me
        .person(ALICE)
        .rename_conversation(conv, &magpie_api::conversations::RenameRequest::thinking(""))
        .await
        .unwrap();
    assert_eq!(
        (cleared.picked_model.as_deref(), cleared.picked_thinking.as_deref()),
        (Some("vendor/other"), None)
    );
    // another person's conversation is not theirs to change
    assert!(
        me.person("bob@example.com")
            .rename_conversation(conv, &magpie_api::conversations::RenameRequest::thinking("low"))
            .await
            .is_err()
    );

    // picked in the draft: the first turn already runs on them
    let req = CreateRequest {
        model: Some("vendor/other".into()),
        thinking: Some("low".into()),
        idempotency_key: "draft-pick".into(),
        message: msg("draft-pick:initial-message", "hi"),
        harness: magpie_api::Harness::default(),
    };
    let made =
        w.a.pigeon()
            .person(ALICE)
            .create_conversation(&req)
            .await
            .unwrap()
            .get()
            .clone();
    assert_eq!(
        (
            made.conversation.picked_model.as_deref(),
            made.conversation.picked_thinking.as_deref()
        ),
        (Some("vendor/other"), Some("low"))
    );
    let first = made.turn.expect("the first turn");
    w.fake().make_ready(made.conversation.id);
    assert!(w.state_is(first.id, "starting").await);
    let sup2 = w.supervisor(made.conversation.id).await;
    assert_eq!(
        start(&sup2.poll(0, 0).await),
        (Some("vendor/other".into()), Some("low".into()))
    );
}
