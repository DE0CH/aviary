//! A conversation's tasks for the app's activity panel (src/turns/tasks.rs; Deyao, 2026-11-12): what Claude Code runs
//! beside its reply is read from the `task.status` events as one entry per task (what it is, whether it runs, what it
//! does now, its result), the running ones first; one task is stopped with a `stop_task` command to the
//! conversation's supervisor.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    dead_code
)]

mod common;

use common::ev::{self, ALICE};
use magpie_api::errors::ErrorCode;
use magpie_api::events::TaskState;
use magpie_api::tasks::TaskType;
use serde_json::json;
use uuid::Uuid;

fn task(
    id: &str,
    call: &str,
    event: &str,
    status: &str,
    description: Option<&str>,
    summary: Option<&str>,
) -> magpie::events::NewEvent {
    let mut data = json!({"taskId": id, "toolUseId": call, "event": event, "status": status});
    if let Some(d) = description {
        data["description"] = json!(d);
    }
    if let Some(s) = summary {
        data["summary"] = json!(s);
    }
    ev::ev("task.status", data)
}

#[tokio::test]
async fn the_tasks_of_a_conversation_are_listed_and_one_is_stopped() {
    let db = common::fresh_db("tasks").await;
    let t = common::Tokens::new();
    let a = common::replica(&db.url, &t).await;
    let pigeon = a.pigeon();
    let alice = pigeon.person(ALICE);
    let id = ev::conversation(&pigeon, ALICE).await;
    let pool = a.pool();
    let turn = Uuid::new_v4();
    assert_eq!(alice.conversation_tasks(id).await.unwrap().tasks, []);

    // a command in the background that ends, and a subagent that still runs
    let seqs = ev::append(
        pool,
        ALICE,
        id,
        vec![
            ev::turn_started(turn),
            ev::in_turn(
                ev::assistant(
                    "m1",
                    json!([ev::tool_use("call-bash", "Bash"), ev::tool_use("call-agent", "Agent")]),
                ),
                turn,
            ),
            task("t-bash", "call-bash", "started", "running", Some("Run the tests"), None),
            task(
                "t-agent",
                "call-agent",
                "started",
                "running",
                Some("Review the change"),
                None,
            ),
            task(
                "t-agent",
                "call-agent",
                "progress",
                "running",
                Some("Running Read src/lib.rs"),
                None,
            ),
            task(
                "t-agent",
                "call-agent",
                "progress",
                "running",
                Some("Running Grep midturn"),
                None,
            ),
            task(
                "t-bash",
                "call-bash",
                "notification",
                "completed",
                None,
                Some("12 passed"),
            ),
        ],
    )
    .await;
    // which tool launched each call (magpie records it as it reads Claude Code's transcript)
    // (both calls are in the assistant line, the second event)
    for (call, name, seq) in [("call-bash", "Bash", seqs[1]), ("call-agent", "Agent", seqs[1])] {
        pool.get()
            .await
            .unwrap()
            .execute(
                "insert into tool_links (conversation_id, tool_use_id, use_seq, name) values ($1, $2, $3, $4)
                 on conflict do nothing",
                &[&id, &call, &seq, &name],
            )
            .await
            .unwrap();
    }
    let tasks = alice.conversation_tasks(id).await.unwrap().tasks;
    assert_eq!(
        tasks.iter().map(|t| t.task_id.as_str()).collect::<Vec<_>>(),
        ["t-agent", "t-bash"],
        "the running one first"
    );
    let agent = &tasks[0];
    assert_eq!((agent.kind, agent.status), (TaskType::Subagent, TaskState::Running));
    assert_eq!(agent.description.as_deref(), Some("Review the change"));
    assert_eq!(
        agent.now.as_deref(),
        Some("Running Grep midturn"),
        "its latest progress"
    );
    assert_eq!(agent.steps, 2);
    assert!(agent.ended_at.is_none());
    let bash = &tasks[1];
    assert_eq!((bash.kind, bash.status), (TaskType::Command, TaskState::Completed));
    assert_eq!(bash.description.as_deref(), Some("Run the tests"));
    assert_eq!(bash.command.as_deref(), Some("ls"), "the command the call ran");
    assert_eq!(bash.summary.as_deref(), Some("12 passed"));
    assert!(bash.ended_at.is_some());
    assert!(bash.now.is_none());

    // stopping one: a command for the conversation's supervisor
    alice.stop_task(id, "t-agent").await.unwrap();
    let row = pool
        .get()
        .await
        .unwrap()
        .query_one(
            "select payload from commands where conversation_id = $1 and type = 'stop_task'",
            &[&id],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, serde_json::Value>(0), json!({"taskId": "t-agent"}));

    // someone else's conversation is not found; an admin's view is read-only
    let bob = pigeon.person("bob@example.org");
    let e = bob.conversation_tasks(id).await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::NotFound));
    let e = bob.stop_task(id, "t-agent").await.unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::NotFound));
    let e = pigeon
        .viewing_as(ALICE, "admin@example.org", "s1")
        .stop_task(id, "t-agent")
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(&ErrorCode::ViewAsReadOnly));
}
