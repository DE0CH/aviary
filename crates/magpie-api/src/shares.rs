//! Shares (M16, magpie 12): a frozen copy of a conversation up to a finished turn, and continuing it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Timestamp;
use crate::rows::Row;

/// A share id: 16 random bytes, base64url, 22 characters (pigeon builds `/s/<id>`).
pub type ShareId = String;

/// `POST /v1/conversations/{id}/shares` → 201, and an entry of the owner's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareRef {
    /// The share.
    pub id: ShareId,
    /// Created.
    pub created_at: Timestamp,
}

/// `GET /v1/conversations/{id}/shares`: live links, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharesList {
    /// The links.
    pub shares: Vec<ShareRef>,
}

/// `GET /v1/shares/{shareId}`: every frozen row, ascending by `seqStart`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareView {
    /// The share.
    pub id: ShareId,
    /// The conversation's title at the freeze.
    pub title: Option<String>,
    /// The owner's email local part.
    pub owner_name: String,
    /// The viewer is the owner.
    pub is_owner: bool,
    /// Created.
    pub created_at: Timestamp,
    /// The share's own epoch.
    pub epoch: String,
    /// The rows.
    pub rows: Vec<Row>,
}

/// `POST /v1/shares/{shareId}/continue` → 201.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueResponse {
    /// The viewer's new conversation.
    pub conversation_id: Uuid,
}
