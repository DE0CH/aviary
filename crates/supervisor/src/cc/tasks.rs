//! Background tasks and the hold (supervisor 7.7): one tracker over two sources, Claude Code's task events and the
//! static rule for the tools that emit none (`Monitor`, `ScheduleWakeup`, `CronCreate`, `TaskCreate`).

use std::collections::VecDeque;
use std::time::Duration;

use serde_json::Value;
use time::OffsetDateTime;

use crate::cc::stream::{TaskKind, TaskMsg, ToolUse};
use crate::limits::WAKEUP_SLACK;
use crate::state::TaskState;

/// The tools the static rule covers.
pub const STATIC_TOOLS: [&str; 4] = ["Monitor", "ScheduleWakeup", "CronCreate", "TaskCreate"];
/// How many main-thread `tool_use` ids are remembered to tell nested tasks apart.
const MAIN_IDS: usize = 512;

/// Where a task came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Event,
    Static,
}

/// An outstanding task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub task_id: String,
    pub tool_use_id: Option<String>,
    pub kind: String,
    pub description: String,
    pub workflow_name: Option<String>,
    pub started_at: OffsetDateTime,
    pub source: Source,
    /// A pseudo-task's end (`ScheduleWakeup`: its fire time plus 2 min).
    pub until: Option<OffsetDateTime>,
    /// Started inside a subagent or workflow (its `tool_use_id` is not one of the main thread's).
    pub nested: bool,
}

/// What a task message did to the tracker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    None,
    Added,
    Removed,
}

/// The tracker.
#[derive(Clone, Debug, Default)]
pub struct Tracker {
    tasks: Vec<Task>,
    main_ids: VecDeque<String>,
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn parse_time(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

/// When a `ScheduleWakeup` fires, from its input: a delay in seconds (`delaySeconds`, `delay_seconds`, `seconds`,
/// `delay`), in milliseconds (`delayMs`, `delay_ms`), or a time (`at`, `fireAt`, `fire_at`, `time`); `None` when
/// the input names none of them (the pseudo-task then lasts until the silence ceiling).
#[must_use]
pub fn wakeup_at(input: &Value, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let num = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    for k in ["delaySeconds", "delay_seconds", "seconds", "delay"] {
        if let Some(s) = num(k) {
            return Some(now + Duration::from_secs_f64(s.min(1e9)));
        }
    }
    for k in ["delayMs", "delay_ms"] {
        if let Some(ms) = num(k) {
            return Some(now + Duration::from_secs_f64((ms / 1000.0).min(1e9)));
        }
    }
    for k in ["at", "fireAt", "fire_at", "time"] {
        if let Some(t) = input.get(k).and_then(Value::as_str).and_then(parse_time) {
            return Some(t);
        }
    }
    None
}

impl Tracker {
    /// The tracker of a restarted supervisor.
    #[must_use]
    pub fn from_saved(saved: &[TaskState]) -> Self {
        let tasks = saved
            .iter()
            .map(|t| Task {
                task_id: t.task_id.clone(),
                tool_use_id: t.tool_use_id.clone(),
                kind: t.kind.clone(),
                description: t.description.clone(),
                workflow_name: None,
                started_at: parse_time(&t.started_at).unwrap_or_else(OffsetDateTime::now_utc),
                source: if t.source == "static" {
                    Source::Static
                } else {
                    Source::Event
                },
                until: t.until.as_deref().and_then(parse_time),
                nested: t.nested,
            })
            .collect();
        Self {
            tasks,
            main_ids: VecDeque::new(),
        }
    }

    /// The tasks as `run.json` keeps them.
    #[must_use]
    pub fn saved(&self) -> Vec<TaskState> {
        self.tasks
            .iter()
            .map(|t| TaskState {
                task_id: t.task_id.clone(),
                tool_use_id: t.tool_use_id.clone(),
                kind: t.kind.clone(),
                description: t.description.clone(),
                started_at: rfc3339(t.started_at),
                source: match t.source {
                    Source::Event => "event".into(),
                    Source::Static => "static".into(),
                },
                until: t.until.map(rfc3339),
                nested: t.nested,
            })
            .collect()
    }

    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// The event tasks' ids (what `stop_task` can stop).
    #[must_use]
    pub fn event_ids(&self) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|t| t.source == Source::Event)
            .map(|t| t.task_id.clone())
            .collect()
    }

    /// Whether any event task is outstanding.
    #[must_use]
    pub fn has_event_tasks(&self) -> bool {
        self.tasks.iter().any(|t| t.source == Source::Event)
    }

    /// A pseudo-task's `until` lies ahead (the silence ceiling waits for it).
    #[must_use]
    pub fn until_ahead(&self, now: OffsetDateTime) -> bool {
        self.tasks.iter().any(|t| t.until.is_some_and(|u| u > now))
    }

    /// A main-thread `tool_use`: remembered, and a pseudo-task for the static tools. `true` when one was added.
    pub fn on_tool_use(&mut self, tu: &ToolUse, now: OffsetDateTime) -> bool {
        self.main_ids.push_back(tu.id.clone());
        while self.main_ids.len() > MAIN_IDS {
            self.main_ids.pop_front();
        }
        if !STATIC_TOOLS.contains(&tu.name.as_str()) {
            return false;
        }
        let task_id = format!("static:{}", tu.id);
        if self.tasks.iter().any(|t| t.task_id == task_id) {
            return false;
        }
        let until = (tu.name == "ScheduleWakeup")
            .then(|| wakeup_at(&tu.input, now).map(|t| t + WAKEUP_SLACK))
            .flatten();
        self.tasks.push(Task {
            task_id,
            tool_use_id: Some(tu.id.clone()),
            kind: tu.name.clone(),
            description: tu.name.clone(),
            workflow_name: None,
            started_at: now,
            source: Source::Static,
            until,
            nested: false,
        });
        true
    }

    /// A task event.
    pub fn on_task(&mut self, m: &TaskMsg, now: OffsetDateTime) -> Change {
        match m.kind {
            TaskKind::Started => {
                let Some(tool_use_id) = m.tool_use_id.clone() else {
                    // housekeeping tasks without a tool_use_id are not background work
                    return Change::None;
                };
                if self.tasks.iter().any(|t| t.task_id == m.task_id) {
                    return Change::None;
                }
                let nested = !self.main_ids.contains(&tool_use_id);
                self.tasks.push(Task {
                    task_id: m.task_id.clone(),
                    tool_use_id: Some(tool_use_id),
                    kind: m.task_type.clone().unwrap_or_default(),
                    description: m.description.clone().unwrap_or_default(),
                    workflow_name: m.workflow_name.clone(),
                    started_at: now,
                    source: Source::Event,
                    until: None,
                    nested,
                });
                Change::Added
            }
            TaskKind::Notification => self.remove(&m.task_id),
            TaskKind::Updated if matches!(m.status.as_deref(), Some("completed" | "failed" | "killed" | "stopped")) => {
                self.remove(&m.task_id)
            }
            TaskKind::Updated | TaskKind::Progress => Change::None,
        }
    }

    fn remove(&mut self, task_id: &str) -> Change {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.task_id != task_id);
        if self.tasks.len() < before {
            Change::Removed
        } else {
            Change::None
        }
    }

    /// Pseudo-tasks whose `until` has passed are dropped; how many.
    pub fn expire(&mut self, now: OffsetDateTime) -> usize {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.until.is_none_or(|u| u > now));
        before - self.tasks.len()
    }

    /// An agent turn opened by a `<task-notification>`: the pseudo-tasks whose `tool_use` id it names end.
    pub fn on_notification(&mut self, text: &str) -> usize {
        let before = self.tasks.len();
        self.tasks
            .retain(|t| t.source != Source::Static || !t.tool_use_id.as_deref().is_some_and(|id| text.contains(id)));
        before - self.tasks.len()
    }

    /// Drops the pseudo-tasks.
    pub fn clear_static(&mut self) {
        self.tasks.retain(|t| t.source != Source::Static);
    }

    /// Drops everything (the generation ended: its tasks died with its process group).
    pub fn clear(&mut self) {
        self.tasks.clear();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tu(id: &str, name: &str, input: Value) -> ToolUse {
        ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    fn task(kind: TaskKind, id: &str, tool: Option<&str>, status: Option<&str>) -> TaskMsg {
        TaskMsg {
            kind,
            task_id: id.into(),
            tool_use_id: tool.map(str::to_owned),
            task_type: Some("local_bash".into()),
            description: Some("build".into()),
            workflow_name: None,
            status: status.map(str::to_owned),
            summary: None,
            output_file: None,
        }
    }

    #[test]
    fn events_and_the_static_rule_combine() {
        let now = OffsetDateTime::now_utc();
        let mut t = Tracker::default();
        assert!(!t.on_tool_use(&tu("toolu_1", "Bash", json!({"run_in_background": true})), now));
        assert_eq!(
            t.on_task(&task(TaskKind::Started, "b1", Some("toolu_1"), None), now),
            Change::Added
        );
        assert!(t.on_tool_use(&tu("toolu_2", "Monitor", json!({})), now));
        assert_eq!(
            t.len(),
            2,
            "a task event does not make the tracker alone decide (A9 surprise)"
        );
        assert_eq!(
            t.on_task(&task(TaskKind::Progress, "b1", None, None), now),
            Change::None
        );
        assert_eq!(
            t.on_task(&task(TaskKind::Updated, "b1", None, Some("running")), now),
            Change::None
        );
        assert_eq!(
            t.on_task(&task(TaskKind::Updated, "b1", None, Some("killed")), now),
            Change::Removed
        );
        assert_eq!(t.event_ids(), Vec::<String>::new());
        assert_eq!(t.len(), 1);
        assert_eq!(
            t.on_notification("<task-notification>toolu_2 fired</task-notification>"),
            1
        );
        assert!(t.is_empty());
        // housekeeping without a tool_use_id is not tracked; a task from inside a subagent is nested
        assert_eq!(t.on_task(&task(TaskKind::Started, "h", None, None), now), Change::None);
        t.on_task(&task(TaskKind::Started, "n1", Some("toolu_sub"), None), now);
        assert!(t.tasks()[0].nested);
        assert_eq!(
            t.on_task(&task(TaskKind::Notification, "n1", None, Some("completed")), now),
            Change::Removed
        );
    }

    #[test]
    fn a_wakeup_lasts_until_its_fire_time_plus_two_minutes() {
        let now = OffsetDateTime::now_utc();
        let mut t = Tracker::default();
        t.on_tool_use(&tu("toolu_w", "ScheduleWakeup", json!({"delaySeconds": 3600})), now);
        let until = t.tasks()[0].until.unwrap();
        assert_eq!(until, now + Duration::from_secs(3600) + WAKEUP_SLACK);
        assert!(t.until_ahead(now));
        assert_eq!(t.expire(now + Duration::from_mins(50)), 0);
        assert_eq!(t.expire(until + Duration::from_secs(1)), 1);
        let at = json!({"at": "2030-01-01T00:00:00Z"});
        assert_eq!(wakeup_at(&at, now).unwrap().year(), 2030);
        assert_eq!(wakeup_at(&json!({"prompt": "x"}), now), None);
        // round trip through run.json
        t.on_tool_use(&tu("toolu_c", "CronCreate", json!({})), now);
        let back = Tracker::from_saved(&t.saved());
        assert_eq!(back.tasks()[0].task_id, "static:toolu_c");
        assert_eq!(back.tasks()[0].source, Source::Static);
    }
}
