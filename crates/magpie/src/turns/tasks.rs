//! A conversation's tasks for the app's activity panel (Deyao, 2026-11-12): the `task.status` events of the
//! conversation read as one entry per task (what it is, whether it runs, what it is doing, its result), and the
//! stop of one task (`stop_task`, to the conversation's supervisor).

use std::collections::HashMap;

use aviary_pg::Pool;
use magpie_api::Timestamp;
use magpie_api::events::{TaskEvent, TaskState, TaskStatus};
use magpie_api::tasks::{TASK_COMMAND_CHARS, Task, TaskType, TasksList};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::command;
use crate::conversations::{Person, load_owned, lock};
use crate::error::Result;

/// The tasks of the person's conversation `id`: the running ones first, then the ended ones, each newest first.
///
/// # Errors
/// 404 `not_found`; a statement error.
pub async fn list(pool: &Pool, person: &Person, id: Uuid) -> Result<TasksList> {
    let conn = pool.get().await?;
    load_owned(&conn, &person.email, id).await?;
    let rows = conn
        .query(
            "select data, at from events where conversation_id = $1 and kind = 'task.status' order by seq",
            &[&id],
        )
        .await?;
    let mut tasks: Vec<Task> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for r in &rows {
        let Ok(s) = serde_json::from_value::<TaskStatus>(r.get::<_, Value>(0)) else {
            continue;
        };
        let when = Timestamp(r.get::<_, OffsetDateTime>(1));
        let i = *at.entry(s.task_id.clone()).or_insert_with(|| {
            tasks.push(Task {
                task_id: s.task_id.clone(),
                tool_use_id: None,
                kind: TaskType::Other,
                description: None,
                command: None,
                status: TaskState::Running,
                started_at: when,
                ended_at: None,
                now: None,
                steps: 0,
                summary: None,
            });
            tasks.len() - 1
        });
        let t = &mut tasks[i];
        if t.tool_use_id.is_none() {
            t.tool_use_id.clone_from(&s.tool_use_id);
        }
        t.status = s.status;
        if s.event == TaskEvent::Progress {
            // a progress report says what the task does now (a subagent: one per tool call)
            t.steps += 1;
            t.now.clone_from(&s.description);
        } else if t.description.is_none() {
            t.description.clone_from(&s.description);
        }
        if s.summary.is_some() {
            t.summary = s.summary;
        }
        t.ended_at = (s.status != TaskState::Running).then_some(when);
    }
    // what each one is: the tool that launched it
    let calls: Vec<String> = tasks.iter().filter_map(|t| t.tool_use_id.clone()).collect();
    // (the assistant line that made the call holds its input: the command a background command runs)
    let launches: HashMap<String, (String, Option<String>)> = conn
        .query(
            "select l.tool_use_id, l.name, e.data from tool_links l
             left join events e on e.conversation_id = l.conversation_id and e.seq = l.use_seq
             where l.conversation_id = $1 and l.tool_use_id = any($2)",
            &[&id, &calls],
        )
        .await?
        .iter()
        .map(|r| {
            let call: String = r.get(0);
            let command = r.get::<_, Option<Value>>(2).and_then(|line| command_of(&line, &call));
            (call, (r.get(1), command))
        })
        .collect();
    for t in &mut tasks {
        let launch = t.tool_use_id.as_ref().and_then(|c| launches.get(c));
        t.kind = TaskType::of_tool(launch.map(|(name, _)| name.as_str()));
        t.command = launch.and_then(|(_, command)| command.clone());
        if t.status != TaskState::Running {
            t.now = None;
        }
    }
    tasks.sort_by(|a, b| {
        (a.status != TaskState::Running)
            .cmp(&(b.status != TaskState::Running))
            .then(b.started_at.0.cmp(&a.started_at.0))
    });
    Ok(TasksList { tasks })
}

/// The `command` of the tool call `call` in the assistant line `line` (Claude Code's transcript line, as the
/// `cc.assistant` event keeps it), cut to [`TASK_COMMAND_CHARS`] characters; `None` when the call has none.
fn command_of(line: &Value, call: &str) -> Option<String> {
    let block = line.pointer("/message/content")?.as_array()?.iter().find(|b| {
        b.get("type").and_then(Value::as_str) == Some("tool_use") && b.get("id").and_then(Value::as_str) == Some(call)
    })?;
    let command = block.pointer("/input/command")?.as_str()?.trim();
    (!command.is_empty()).then(|| command.chars().take(TASK_COMMAND_CHARS).collect())
}

/// Tells the conversation's supervisor to stop the task `task_id`. Nothing is checked about the task: one that no
/// longer runs is the supervisor's to answer (it stops nothing).
///
/// # Errors
/// 403 `view_as_read_only`, 404 `not_found`; a statement error.
pub async fn stop(pool: &Pool, person: &Person, id: Uuid, task_id: &str) -> Result<()> {
    person.refuse_view_as()?;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        lock(t, &person.email, id).await?;
        command(t, id, "stop_task", None, &json!({"taskId": task_id})).await?;
        Ok(())
    })
    .await
}
