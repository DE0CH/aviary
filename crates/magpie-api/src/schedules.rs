//! Schedules (Deyao, 2026-11-06): a prompt the conversation's AI arms to be sent into that same conversation
//! once (`at`) or on a repeating rule (`cron` in `tz`, optionally `until`).
//!
//! The AI calls the sandbox routes with `curl` (`http://magpie/v1/sandbox/schedules`, through the gateway, which adds
//! the verified sandbox identity: the person and the conversation); pigeon reads one conversation's schedules for the
//! app's panel; the admin routes list everyone's and hold the pause-all switch. Times on these routes are local
//! times with their offset in the schedule's zone (`2026-11-07T08:30:00+08:00`), so the AI repeats what the person
//! means; the stored instants are UTC. `describe` is the server's own reading of the rule in plain Hong Kong Chinese
//! (`每個工作日 08:30`), which the AI repeats to the person instead of its own reading.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A schedule as the routes answer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    /// `sch_` and 32 lowercase hex characters.
    pub id: String,
    /// The conversation it posts into (always the one that set it).
    pub conversation_id: Uuid,
    /// `once` or `cron`.
    pub kind: String,
    /// The exact message posted when it fires.
    pub prompt: String,
    /// `once`: the time, local with its offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// `cron`: five fields, minute hour day-of-month month day-of-week.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// The IANA zone its times are read in.
    pub tz: String,
    /// `cron`: no fire after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// The rule in plain Hong Kong Chinese.
    pub describe: String,
    /// The next fire, local with its offset; `null` when none is due (paused, done, deleted).
    pub next: Option<String>,
    /// `active`, `paused`, `done` or `deleted`.
    pub state: String,
    /// The person (the conversation's owner) who set it.
    pub created_by: String,
    /// When it was set, local with its offset.
    pub created_at: String,
    /// The conversation's title (admin and panel lists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_title: Option<String>,
    /// The last runs, newest first (one schedule's read, the panel and the admin list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<ScheduleRun>>,
}

/// One time a schedule came due.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRun {
    /// When it was due, local with its offset.
    pub due: String,
    /// When it was handled, local with its offset.
    pub at: String,
    /// `sent` (a new turn), `sent_mid_turn` (taken into the running turn), `pending` (being delivered),
    /// `skipped_late` (the service was down for more than 10 minutes), `skipped_paused` (all schedules were paused),
    /// `skipped_deleted` (the conversation is in Recently deleted) or `failed`.
    pub outcome: String,
    /// Why, for `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `POST /v1/sandbox/schedules`: exactly one of `at` and `cron`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSchedule {
    /// The exact message to post.
    #[serde(default)]
    pub prompt: String,
    /// A one-off: RFC 3339 with an offset (`2026-11-07T09:00+08:00`), or a local time read in `tz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// A repeating rule (five fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// The zone (default `Asia/Hong_Kong`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
    /// A repeating rule's end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

/// `PATCH /v1/sandbox/schedules/{scheduleId}`: the fields to change (an empty `until` removes the end).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSchedule {
    /// Pause or resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    /// A new message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// A new time (a one-off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// A new rule (a repeating one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// A new zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
    /// A new end (`""`: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

/// A list of schedules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulesList {
    /// Newest first.
    pub schedules: Vec<Schedule>,
}

/// `GET /v1/admin/schedules`: everyone's schedules and the pause-all switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSchedules {
    /// No schedule fires while set (their due times are recorded as `skipped_paused`).
    pub paused_all: bool,
    /// Who set the switch last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_by: Option<String>,
    /// When, RFC 3339 UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<String>,
    /// Every schedule not deleted, with its last runs.
    pub schedules: Vec<Schedule>,
}

/// `PUT /v1/admin/schedules/pause`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseAll {
    /// On or off.
    pub paused: bool,
}
