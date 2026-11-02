//! The outbox (magpie 15): calls to hermit that must happen exactly once eventually, queued in the transaction that
//! decides them and made by the job leader afterwards, outside any transaction.
//!
//! Each pass takes the due items (`next_at <= now()`), oldest first, makes each call through a [`Deliver`], and
//! deletes the item when it is done (hermit's 404 counts as done) or pushes `next_at` out (5 s doubling to 1 h).
//! A workspace's deletion waits until the same conversation's earlier sandbox release is done (hermit's spec does
//! not promise that deleting a workspace under a live sandbox is safe). An item that does not parse is logged and
//! looked at again only hourly, so it never holds up the queue.
//! An item failing for 24 h logs `outbox failing` on every later attempt, which the backoff makes hourly.
//!
//! A `release_sandbox {reason: deleted}` is meant for a conversation in Recently deleted (or purged). Restore drops
//! the queued one ([`cancel_deleted_release`]), and the pass checks again just before the call: an item whose
//! conversation is no longer deleted is dropped uncalled, so a release held back by hermit's backoff never lands on
//! the sandbox of a restored conversation's later turn.
//!
//! Leadership is confirmed before each item (`still_leading`), not only before the pass: a pass with hermit
//! unreachable can take minutes, and a replica that lost the lock stops at the next item instead of racing the new
//! leader on the same items.

use std::future::Future;
use std::time::Duration;

use aviary_pg::{Pool, Tx};
use hermit_api::sandbox::{ConversationReleaseReason, ReleaseSandboxRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Result;
use crate::limits;

/// One call to hermit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Item {
    /// `POST /v1/conversations/{id}/sandbox/release {reason}`.
    ReleaseSandbox {
        /// The conversation.
        #[serde(rename = "conversationId")]
        conversation_id: Uuid,
        /// Why.
        reason: ConversationReleaseReason,
    },
    /// `DELETE /v1/workspaces/{wid}`.
    DeleteWorkspace {
        /// The workspace (a conversation id, or a share's frozen workspace).
        #[serde(rename = "workspaceId")]
        workspace_id: Uuid,
    },
}

impl Item {
    fn kind(&self) -> &'static str {
        match self {
            Item::ReleaseSandbox { .. } => "release_sandbox",
            Item::DeleteWorkspace { .. } => "delete_workspace",
        }
    }

    fn payload(&self) -> Result<Value> {
        let mut v = serde_json::to_value(self)?;
        Ok(v.get_mut("payload").map_or(Value::Null, Value::take))
    }

    fn parse(kind: &str, payload: &Value) -> Option<Self> {
        serde_json::from_value(serde_json::json!({"kind": kind, "payload": payload})).ok()
    }
}

/// Queues `item` in `tx` (it is made only if `tx` commits).
///
/// # Errors
/// When the statement fails.
pub async fn enqueue(tx: &Tx<'_>, item: &Item) -> Result<()> {
    tx.execute(
        "insert into outbox (kind, payload) values ($1, $2)",
        &[&item.kind(), &item.payload()?],
    )
    .await?;
    Ok(())
}

/// Drops `conversation`'s queued `release_sandbox {reason: deleted}` items, in `tx` (restore).
///
/// # Errors
/// When the statement fails.
pub async fn cancel_deleted_release(tx: &Tx<'_>, conversation: Uuid) -> Result<u64> {
    Ok(tx
        .execute(
            "delete from outbox where kind = 'release_sandbox'
               and payload->>'conversationId' = $1 and payload->>'reason' = 'deleted'",
            &[&conversation.to_string()],
        )
        .await?)
}

/// Whether a `release_sandbox {reason: deleted}` for `conversation` still applies: the conversation is in Recently
/// deleted or gone.
async fn deleted_release_applies(pool: &Pool, conversation: Uuid) -> Result<bool> {
    let row = pool
        .get()
        .await?
        .query_opt(
            "select deleted_at is not null from conversations where id = $1",
            &[&conversation],
        )
        .await?;
    Ok(row.is_none_or(|r| r.get::<_, bool>(0)))
}

/// How one call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Done (hermit's 404 included): the item is deleted.
    Done,
    /// Try again later, with this error recorded.
    Retry(String),
}

/// Makes one outbox call.
pub trait Deliver: Send + Sync + 'static {
    /// The call for `item`.
    fn deliver(&self, item: &Item) -> impl Future<Output = Outcome> + Send;
}

/// The real calls, to hermit (`MAGPIE_TOKEN_HERMIT`).
#[derive(Debug, Clone)]
pub struct Hermit(pub hermit_api::client::Client);

impl Deliver for Hermit {
    async fn deliver(&self, item: &Item) -> Outcome {
        let res = match item {
            Item::ReleaseSandbox {
                conversation_id,
                reason,
            } => self
                .0
                .release_sandbox(*conversation_id, &ReleaseSandboxRequest { reason: *reason })
                .await
                .map(|_| ()),
            Item::DeleteWorkspace { workspace_id } => self.0.delete_workspace(*workspace_id).await,
        };
        match res {
            Ok(()) => Outcome::Done,
            Err(e) if e.status() == Some(404) => Outcome::Done,
            Err(e) => Outcome::Retry(e.to_string()),
        }
    }
}

/// The backoff after `attempts` failed attempts: 5 s doubling to 1 h.
#[must_use]
pub fn backoff(attempts: i32) -> Duration {
    let n = u32::try_from(attempts.saturating_sub(1).clamp(0, 20)).unwrap_or(0);
    limits::OUTBOX_BACKOFF_FIRST
        .saturating_mul(1u32 << n)
        .min(limits::OUTBOX_BACKOFF_MAX)
}

/// One pass over the due items; how many were done.
///
/// # Errors
/// When a statement fails.
pub async fn pass(pool: &Pool, deliver: &impl Deliver) -> Result<usize> {
    pass_while(pool, deliver, || async { true }).await
}

/// [`pass`], stopping before the next item once `still_leading` answers false (the job leader's lock check).
///
/// # Errors
/// When a statement fails.
pub async fn pass_while<F, Fut>(pool: &Pool, deliver: &impl Deliver, mut still_leading: F) -> Result<usize>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let rows = pool
        .get()
        .await?
        .query(
            "select id, kind, payload, attempts, created_at from outbox o
             where next_at <= now()
               and not (kind = 'delete_workspace' and exists (
                 select 1 from outbox r
                 where r.kind = 'release_sandbox' and r.id < o.id
                   and r.payload->>'conversationId' = o.payload->>'workspaceId'))
             order by id limit $1",
            &[&limits::OUTBOX_BATCH],
        )
        .await?;
    let mut done = 0;
    for r in rows {
        let id: i64 = r.get(0);
        let kind: String = r.get(1);
        let attempts: i32 = r.get(3);
        let created: OffsetDateTime = r.get(4);
        let payload: Value = r.get(2);
        let Some(item) = Item::parse(&kind, &payload) else {
            // out of the way of the items behind it: looked at again only hourly
            tracing::error!(id, kind = %kind, "outbox item that does not parse (left for an operator)");
            let next = OffsetDateTime::now_utc()
                + time::Duration::try_from(limits::OUTBOX_BACKOFF_MAX).unwrap_or(time::Duration::HOUR);
            pool.get()
                .await?
                .execute(
                    "update outbox set attempts = attempts + 1, next_at = $2, last_error = 'does not parse' where id = $1",
                    &[&id, &next],
                )
                .await?;
            continue;
        };
        if !still_leading().await {
            tracing::info!("outbox pass stopped: no longer the job leader");
            break;
        }
        if let Item::ReleaseSandbox {
            conversation_id,
            reason: ConversationReleaseReason::Deleted,
        } = &item
            && !deleted_release_applies(pool, *conversation_id).await?
        {
            pool.get()
                .await?
                .execute("delete from outbox where id = $1", &[&id])
                .await?;
            tracing::info!(id, conversation = %conversation_id, "outbox release dropped: the conversation was restored");
            continue;
        }
        let outcome = tokio::time::timeout(limits::OUTBOX_CALL_TIMEOUT, deliver.deliver(&item))
            .await
            .unwrap_or_else(|_| Outcome::Retry("timed out".into()));
        let conn = pool.get().await?;
        match outcome {
            Outcome::Done => {
                conn.execute("delete from outbox where id = $1", &[&id]).await?;
                done += 1;
            }
            Outcome::Retry(err) => {
                let attempts = attempts.saturating_add(1);
                let wait = time::Duration::try_from(backoff(attempts)).unwrap_or(time::Duration::HOUR);
                let next = OffsetDateTime::now_utc() + wait;
                conn.execute(
                    "update outbox set attempts = $2, next_at = $3, last_error = $4 where id = $1",
                    &[&id, &attempts, &next, &err],
                )
                .await?;
                let failing = OffsetDateTime::now_utc() - created;
                if failing >= time::Duration::try_from(limits::OUTBOX_FAILING_AFTER).unwrap_or(time::Duration::DAY) {
                    tracing::error!(id, kind = %kind, attempts, error = %err, "outbox failing");
                } else {
                    tracing::info!(id, kind = %kind, attempts, error = %err, "outbox call failed, retrying");
                }
            }
        }
    }
    Ok(done)
}
