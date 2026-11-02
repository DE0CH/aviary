//! The person change feed (M12, magpie 7.6, pigeon 14.1): `seq` dense per person, each change written in the same
//! transaction as the event or state change it reports, kept 30 days.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Timestamp;
use crate::conversations::{Ask, Conversation};
use crate::errors::ErrorInfo;
use crate::turns::{AgentPermissionResponse, TurnOrigin, TurnPhase};

/// Change page: at most (magpie 17).
pub const CHANGES_MAX: u32 = 500;

/// M12 `after`: a seq, or `latest` (answers at once with no changes and the head).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    /// Changes with `seq > n`.
    Seq(u64),
    /// The current head.
    Latest,
}

impl fmt::Display for After {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            After::Seq(n) => write!(f, "{n}"),
            After::Latest => f.write_str("latest"),
        }
    }
}

impl FromStr for After {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "latest" {
            Ok(After::Latest)
        } else {
            s.parse()
                .map(After::Seq)
                .map_err(|_| format!("after must be a seq or latest, got {s:?}"))
        }
    }
}

impl Serialize for After {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for After {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// M12 query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesQuery {
    /// Where to resume.
    pub after: After,
    /// 1-500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Hold up to 25,000 ms when nothing is past `after`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

/// M12 response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesResponse {
    /// The person's change generation (never changes; Paseo's list generation).
    pub generation: String,
    /// The person's last change seq, read in the same snapshot as `changes` (so at least every seq in it). The next
    /// request's `after` is the last change's seq, or `head_seq` when `changes` is empty: with exactly `limit`
    /// changes, more may follow up to `head_seq`.
    pub head_seq: u64,
    /// Ascending by seq.
    pub changes: Vec<Change>,
}

/// One change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// Dense per person (consecutive `conversation_upserted` of one conversation may be collapsed).
    pub seq: u64,
    /// When.
    pub at: Timestamp,
    /// The conversation.
    pub conversation_id: Uuid,
    /// `type` and its fields.
    #[serde(flatten)]
    pub body: ChangeBody,
}

/// `type` and the type's fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChangeBody {
    /// Create, title, run state or sandbox phase change, activity, soft delete (`deletedAt` set), restore
    /// (`deletedAt: null`), ask opened or closed.
    ConversationUpserted {
        /// Its state when the change is read.
        conversation: Box<Conversation>,
    },
    /// Gone for good.
    ConversationRemoved {
        /// Why.
        reason: RemovedReason,
    },
    /// A turn started or ended.
    Turn(TurnChange),
    /// The sandbox chip.
    Sandbox {
        /// `preparing` or `ready`.
        state: crate::conversations::SandboxPhase,
    },
    /// An ask opened.
    Ask {
        /// The ask.
        ask: Ask,
    },
    /// An ask was resolved.
    AskResolved(AskResolvedChange),
    /// The conversation's rows were rebuilt (magpie 7.3 rule 9): refetch the tail.
    TimelineReplaced {
        /// The new epoch.
        epoch: String,
    },
    /// A type this build does not know.
    #[serde(other)]
    Unknown,
}

/// Every change type this build knows.
pub const CHANGE_TYPES: &[&str] = &[
    "conversation_upserted",
    "conversation_removed",
    "turn",
    "sandbox",
    "ask",
    "ask_resolved",
    "timeline_replaced",
];

impl ChangeBody {
    /// The type string, `None` for [`ChangeBody::Unknown`].
    #[must_use]
    pub fn type_name(&self) -> Option<&'static str> {
        Some(match self {
            ChangeBody::ConversationUpserted { .. } => "conversation_upserted",
            ChangeBody::ConversationRemoved { .. } => "conversation_removed",
            ChangeBody::Turn(_) => "turn",
            ChangeBody::Sandbox { .. } => "sandbox",
            ChangeBody::Ask { .. } => "ask",
            ChangeBody::AskResolved(_) => "ask_resolved",
            ChangeBody::TimelineReplaced { .. } => "timeline_replaced",
            ChangeBody::Unknown => return None,
        })
    }
}

/// `conversation_removed.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemovedReason {
    /// Purged (permanent delete, the 30-day job, a person purge).
    Purged,
    /// Removed from the active list by a delete (pigeon 14.1 names it; magpie writes `conversation_upserted` with
    /// `deletedAt` for a soft delete).
    Deleted,
}

/// `turn`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnChange {
    /// The turn.
    pub turn_id: Uuid,
    /// Started or how it ended.
    pub phase: TurnPhase,
    /// `person` or `agent`.
    pub origin: TurnOrigin,
    /// Set when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    /// `row_head` at the commit: the turn's last row has a row seq `<= timelineSeq`.
    pub timeline_seq: u64,
}

/// `ask_resolved`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskResolvedChange {
    /// The ask.
    pub ask_id: String,
    /// The answer, or the cancel resolution.
    pub resolution: AgentPermissionResponse,
}
