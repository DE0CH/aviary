//! A conversation's tasks, for the app's activity panel (Deyao, 2026-11-12): what Claude Code runs beside its reply
//! (a background command, a subagent, a workflow, a monitor), read from the `task.status` events.

use serde::{Deserialize, Serialize};

use crate::Timestamp;
use crate::events::TaskState;

/// What a task is, from the tool that launched it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskType {
    /// A command (`Bash`).
    Command,
    /// A subagent (`Agent`, `Task`).
    Subagent,
    /// A workflow (`Workflow`).
    Workflow,
    /// A monitor (`Monitor`).
    Monitor,
    /// Anything else, or a launch magpie has not seen.
    Other,
}

impl TaskType {
    /// From the launching tool's name.
    #[must_use]
    pub fn of_tool(name: Option<&str>) -> Self {
        match name {
            Some("Bash") => Self::Command,
            Some("Agent" | "Task") => Self::Subagent,
            Some("Workflow") => Self::Workflow,
            Some("Monitor") => Self::Monitor,
            _ => Self::Other,
        }
    }
}

/// One task of a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// Claude Code's task id.
    pub task_id: String,
    /// The tool call that launched it.
    pub tool_use_id: Option<String>,
    /// What it is.
    #[serde(rename = "type")]
    pub kind: TaskType,
    /// What it was started to do (its first description).
    pub description: Option<String>,
    /// `running`, `completed`, `failed`, `stopped`.
    pub status: TaskState,
    /// When it started.
    pub started_at: Timestamp,
    /// When it ended (its last event, once it no longer runs).
    pub ended_at: Option<Timestamp>,
    /// What it is doing now: its latest progress, while it runs.
    pub now: Option<String>,
    /// How many progress reports it has made (a subagent: its tool calls).
    pub steps: u32,
    /// Its result or last summary.
    pub summary: Option<String>,
}

/// `GET /v1/conversations/{id}/tasks`: the running tasks first, then the ended ones, each newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksList {
    /// The tasks.
    pub tasks: Vec<Task>,
}
