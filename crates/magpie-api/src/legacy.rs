//! The switch window's routes (DESIGN 9.2-9.5, cutover 6.4, switch-final 4.3 and 23 C5): tickets that close only on
//! an outcome, and the old asset path of a teed upload. pigeon is the only caller. Deleted in A6 with `migrating`;
//! the legacy-path record goes in A7 with the rest of the legacy-path set (DESIGN 9.7).
//!
//! Before any write to a conversation the old stack still owns, pigeon takes a ticket:
//! `POST /v1/conversations/{id}/legacy/tickets` answers `legacy` with a ticket while the conversation is legacy and
//! not fenced, `native` once magpie owns it, and, while it is fenced, waits up to 3 s for the release (then `native`)
//! or for the fence to be lifted or to lapse (then `legacy` with a ticket). pigeon reports the old stack's answer
//! with `POST /v1/legacy/tickets/{ticket}/done`; only an outcome closes a ticket (a send: the turn started and
//! processing, or ended; anything else: applied or refused), never frame delivery, so the importer cannot release a
//! conversation the old stack is about to write (switch-final C5.1).
//!
//! A send's ticket call carries the send's `clientMessageId` (only a send's may): a repeat for the same
//! `(conversation, clientMessageId)` returns the open ticket the first call opened while that ticket is younger than
//! [`TICKET_HOLD_MAX`], so a retry after a timeout opens no second one. Past that age the repeat gets a new ticket and
//! the old one stays open (a write may still be in flight under it) until its outcome arrives or the importer expires
//! it. Every ticket answer carries `issuedAt`: pigeon starts no legacy write under a ticket issued more than
//! [`TICKET_HOLD_MAX`] ago (it takes a new one); the importer relies on that bound to expire a ticket whose outcome
//! never arrives. Other kinds carry no `clientMessageId` (400): a send's upload tickets, one per attached file, must
//! each be a ticket of their own, or the first file's `applied` would close a ticket the other copies still write
//! under.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::common::Timestamp;

/// The oldest ticket pigeon may still start a legacy write under, judged by the ticket's `issuedAt` (the write itself
/// is then bounded by pigeon's own upstream timeouts). A send's ticket call repeated with its `clientMessageId` gets
/// the open ticket back only while it is younger than this. magpie's importer expires an open ticket only once it is
/// older than this and the old child shows nothing in flight for the conversation.
pub const TICKET_HOLD_MAX: Duration = Duration::from_secs(60);

/// What the ticketed write is (switch-final 4.3; DESIGN 9.1's list of LU writes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TicketKind {
    /// A send (a new old turn).
    Send,
    /// Stop.
    Stop,
    /// Rename.
    Rename,
    /// Move to Recently deleted.
    Delete,
    /// Restore from Recently deleted.
    Restore,
    /// Delete for good.
    Purge,
    /// Warm (before the gate only; from the gate a warm is a hermit hint, DESIGN 9.4).
    Warm,
    /// Create a share link.
    ShareCreate,
    /// Revoke a share link.
    ShareRevoke,
    /// Copy an attached file into the old child (the copy-at-send of DESIGN 9.5).
    Upload,
}

impl TicketKind {
    /// The wire name (also the `legacy_tickets.kind` value).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TicketKind::Send => "send",
            TicketKind::Stop => "stop",
            TicketKind::Rename => "rename",
            TicketKind::Delete => "delete",
            TicketKind::Restore => "restore",
            TicketKind::Purge => "purge",
            TicketKind::Warm => "warm",
            TicketKind::ShareCreate => "shareCreate",
            TicketKind::ShareRevoke => "shareRevoke",
            TicketKind::Upload => "upload",
        }
    }

    /// Whether an outcome closes a ticket of this kind (a send closes only on the turn's outcome).
    #[must_use]
    pub fn closes_on(self, outcome: TicketOutcome) -> bool {
        match outcome {
            TicketOutcome::Delivered => false,
            TicketOutcome::Refused => true,
            TicketOutcome::Started | TicketOutcome::Completed | TicketOutcome::Failed => self == TicketKind::Send,
            TicketOutcome::Applied => self != TicketKind::Send,
        }
    }
}

/// `POST /v1/conversations/{id}/legacy/tickets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketRequest {
    /// The write.
    pub kind: TicketKind,
    /// A send's `clientMessageId` (kind `send` only; any other kind with one is 400 `bad_request`): a repeat returns
    /// the conversation's open ticket for it while that ticket is younger than [`TICKET_HOLD_MAX`] (the old child's
    /// and magpie's own idempotency of the write itself are unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
}

/// Where the write goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TicketRoute {
    /// Through pigeon's legacy upstream, under `ticket`.
    Legacy,
    /// magpie owns the conversation: the write is a native call (no ticket).
    Native,
}

/// The answer to a ticket request (200).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketResponse {
    /// Where the write goes.
    pub route: TicketRoute,
    /// The ticket, with `route: "legacy"` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<Uuid>,
    /// When the ticket was issued (database time), with `route: "legacy"` only: pigeon starts no legacy write under
    /// it once [`TICKET_HOLD_MAX`] has passed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<Timestamp>,
}

/// What the old stack did with a ticketed write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TicketOutcome {
    /// The write reached the old child; recorded, never closes a ticket (switch-final C5.1).
    Delivered,
    /// A send's turn started and the old child reports it processing.
    Started,
    /// A send's turn completed.
    Completed,
    /// A send's turn failed.
    Failed,
    /// A write other than a send was applied.
    Applied,
    /// The old child refused the write.
    Refused,
}

impl TicketOutcome {
    /// The wire name (also the `legacy_tickets.outcome` value).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TicketOutcome::Delivered => "delivered",
            TicketOutcome::Started => "started",
            TicketOutcome::Completed => "completed",
            TicketOutcome::Failed => "failed",
            TicketOutcome::Applied => "applied",
            TicketOutcome::Refused => "refused",
        }
    }
}

/// `POST /v1/legacy/tickets/{ticket}/done` → 204; 400 `bad_request` for an outcome that does not fit the ticket's
/// kind; 404 for an unknown ticket or one of another person. A repeat after the ticket closed is a 204 that changes
/// nothing (the first outcome stays).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketDone {
    /// What happened.
    pub outcome: TicketOutcome,
}

/// `PUT /v1/uploads/{uploadId}/legacy-path` → 204: the old asset path pigeon's T0→gate upload tee got back from the
/// old child for these bytes (DESIGN 9.5), so a send that carries that path resolves to this upload. 400
/// `bad_path` for a path that is not an old asset path of the person; 404 for an upload that is not the person's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyPathRequest {
    /// The path as the old child returned it (`…/<person key>/.claude/cloudcli-assets/<file name>`).
    pub legacy_path: String,
}

/// The old asset folder of a person's home on the old stack, as it appears in a composer's file path.
pub const LEGACY_ASSET_DIR: &str = ".claude/cloudcli-assets";

/// The file name inside the old asset folder that `path` names, when `path` is an old asset path: it ends in
/// `<LEGACY_ASSET_DIR>/<name>` (name non-empty, no `/`, not `.` or `..`), and the segment before `.claude`, when
/// there is one, is `person_key` (an old home is `<root>/<person key>`). Anything else is `None`.
#[must_use]
pub fn legacy_asset_name<'a>(path: &'a str, person_key: &str) -> Option<&'a str> {
    let marker = format!("{LEGACY_ASSET_DIR}/");
    let at = path.rfind(&marker)?;
    let name = &path[at + marker.len()..];
    if name.is_empty() || name.contains('/') || name.contains('\0') || name == "." || name == ".." {
        return None;
    }
    let before = &path[..at];
    if !before.is_empty() {
        let home = before.strip_suffix('/')?;
        let key = home.rsplit('/').next().unwrap_or(home);
        if key != person_key {
            return None;
        }
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_asset_paths_name_their_file() {
        let k = "u0123456789abcdef01234567";
        assert_eq!(
            legacy_asset_name(
                &format!("/var/lib/old/home/{k}/.claude/cloudcli-assets/1730000-a.pdf"),
                k
            ),
            Some("1730000-a.pdf")
        );
        assert_eq!(legacy_asset_name(".claude/cloudcli-assets/x.png", k), Some("x.png"));
        assert_eq!(
            legacy_asset_name("/h/uffff/.claude/cloudcli-assets/x.png", k),
            None,
            "another person's home"
        );
        assert_eq!(legacy_asset_name(&format!("/h/{k}/.claude/cloudcli-assets/"), k), None);
        assert_eq!(
            legacy_asset_name(&format!("/h/{k}/.claude/cloudcli-assets/a/b"), k),
            None
        );
        assert_eq!(
            legacy_asset_name(&format!("/h/{k}/.claude/cloudcli-assets/.."), k),
            None
        );
        assert_eq!(legacy_asset_name("uploads/0190/x.pdf", k), None);
        assert_eq!(legacy_asset_name(&format!("/h/{k}x.claude/cloudcli-assets/a"), k), None);
    }

    #[test]
    fn a_send_closes_only_on_its_turn_outcome() {
        use TicketOutcome as O;
        for o in [O::Started, O::Completed, O::Failed, O::Refused] {
            assert!(TicketKind::Send.closes_on(o), "{o:?}");
        }
        assert!(!TicketKind::Send.closes_on(O::Delivered));
        assert!(!TicketKind::Send.closes_on(O::Applied));
        assert!(TicketKind::Rename.closes_on(O::Applied));
        assert!(!TicketKind::Rename.closes_on(O::Started));
        assert!(!TicketKind::Upload.closes_on(O::Delivered));
    }
}
