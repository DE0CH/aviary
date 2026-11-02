//! The gateway's in-cluster ops summary (platform 3.11.7, 15.3): `GET /v1/ops/summary?tz=&recentMinutes=` on the
//! implementation's in-cluster port, for hermit's pair bearer (its ops page) and admin reads. Every `owner` is the
//! person's trimmed, lowercased email.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use hermit_api::Timestamp;

pub const OPS_SUMMARY: &str = "/v1/ops/summary";

/// The query: an IANA zone for the day boundaries and the window of `recent`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsSummaryQuery {
    pub tz: Option<String>,
    pub recent_minutes: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsSummary {
    pub budget: Budget,
    pub egress: Egress,
    pub recent: Recent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    pub status: BudgetStatus,
    pub total: Spend,
    pub by_user: Vec<UserSpend>,
    pub by_model: Vec<ModelSpend>,
    pub by_day: Vec<DaySpend>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetStatus {
    pub spent_usd: f64,
    /// `null` when the deployment sets no limit.
    pub limit_usd: Option<f64>,
    /// Start of the budget period.
    pub since: Option<Timestamp>,
    pub exhausted: bool,
}

/// Spend in USD and calls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    pub today: f64,
    pub month: f64,
    pub since_budget: f64,
    pub calls_today: u64,
    pub calls_month: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSpend {
    pub owner: String,
    #[serde(flatten)]
    pub spend: Spend,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpend {
    pub model: String,
    #[serde(flatten)]
    pub spend: Spend,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySpend {
    /// `YYYY-MM-DD` in the query's zone.
    pub day: String,
    pub usd: f64,
    pub calls: u64,
    pub people: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Egress {
    pub by_status: Vec<StatusCount>,
    pub recent: Vec<RecentRequest>,
    pub denied_hosts: Vec<DeniedHost>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCount {
    /// `model`, `tools`, `internet`, … (the implementation's kinds).
    pub kind: String,
    pub status: Option<u16>,
    /// `allow`, `deny:<why>`, … (the implementation's decisions).
    pub decision: String,
    pub n: u64,
    pub last_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentRequest {
    pub at: Timestamp,
    pub kind: String,
    pub host: String,
    pub path: Option<String>,
    pub status: Option<u16>,
    pub decision: String,
    pub owner: Option<String>,
    pub conversation_id: Option<Uuid>,
    /// Expected noise or a provider glitch the agent retries (a 429, a 5xx, no answer, a `count_tokens` 404): kept
    /// in the log, left out of `recent`'s counts, and skipped by the news-stream monitor's event feed. `false` when
    /// an implementation does not send it.
    #[serde(default)]
    pub quiet: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeniedHost {
    pub host: String,
    pub why: String,
    pub n: u64,
    pub last_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    pub model_errors: u64,
    pub blocked: u64,
    pub list: Vec<RecentEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentEvent {
    pub at: Timestamp,
    pub what: String,
    pub detail: Option<String>,
    pub owner: Option<String>,
}
