//! Delete, restore and purge (M6), magpie 9.6.
//!
//! A soft delete puts the conversation in Recently deleted and releases its sandbox through the outbox; restore takes
//! it out and drops that release if it is still queued (the release names only the conversation, so delivered after a
//! restore it would release the sandbox of the restored conversation's next turn; the outbox also re-checks, see
//! [`outbox`]). A purge (the person's "delete for good" from Recently deleted, the 30-day job, a person purge) removes the
//! conversation and everything it owns in one transaction and queues the hermit calls: the sandbox release and the
//! workspace deletion, and each share's frozen workspace.

use aviary_pg::{Pool, Tx};
use hermit_api::sandbox::ConversationReleaseReason;
use magpie_api::changes::{ChangeBody, RemovedReason};
use magpie_api::conversations::Conversation;
use magpie_api::errors::ErrorCode;
use uuid::Uuid;

use super::{Person, load_owned, lock};
use crate::changes;
use crate::error::{Error, Result};
use crate::fanout;
use crate::jobs::outbox::{self, Item};

/// M6 `POST …/delete`: into Recently deleted (idempotent).
///
/// # Errors
/// 403 `view_as_read_only`, 404, 409 `conversation_migrating`; a statement error.
pub async fn delete(pool: &Pool, person: &Person, id: Uuid) -> Result<Conversation> {
    person.refuse_view_as()?;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = lock(t, &person.email, id).await?;
        c.refuse_migrating()?;
        if c.deleted_at.is_some() {
            return Ok(());
        }
        t.execute("update conversations set deleted_at = now() where id = $1", &[&id])
            .await?;
        outbox::enqueue(
            t,
            &Item::ReleaseSandbox {
                conversation_id: id,
                reason: ConversationReleaseReason::Deleted,
            },
        )
        .await?;
        changes::upserted(t, &person.email, id).await?;
        Ok(())
    })
    .await?;
    let conn = pool.get().await?;
    load_owned(&conn, &person.email, id).await
}

/// M6 `POST …/restore`: out of Recently deleted (idempotent).
///
/// # Errors
/// 403 `view_as_read_only`, 404, 409 `conversation_migrating`; a statement error.
pub async fn restore(pool: &Pool, person: &Person, id: Uuid) -> Result<Conversation> {
    person.refuse_view_as()?;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = lock(t, &person.email, id).await?;
        c.refuse_migrating()?;
        if c.deleted_at.is_none() {
            return Ok(());
        }
        t.execute("update conversations set deleted_at = null where id = $1", &[&id])
            .await?;
        outbox::cancel_deleted_release(t, id).await?;
        changes::upserted(t, &person.email, id).await?;
        Ok(())
    })
    .await?;
    let conn = pool.get().await?;
    load_owned(&conn, &person.email, id).await
}

/// M6 `DELETE …`: purge a conversation that is in Recently deleted.
///
/// # Errors
/// 403 `view_as_read_only`, 404, 409 `not_deleted` / `conversation_migrating`; a statement error.
pub async fn purge(pool: &Pool, person: &Person, id: Uuid) -> Result<()> {
    person.refuse_view_as()?;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let c = lock(t, &person.email, id).await?;
        c.refuse_migrating()?;
        if c.deleted_at.is_none() {
            return Err(Error::code(
                ErrorCode::NotDeleted,
                "only a conversation in Recently deleted can be deleted for good",
            ));
        }
        purge_locked(t, &c.person, id, ShareRevoke::Deleted).await
    })
    .await
}

/// Why a purged conversation's shares are revoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareRevoke {
    /// The conversation was deleted.
    Deleted,
    /// The person was offboarded.
    Offboarded,
}

impl ShareRevoke {
    fn as_str(self) -> &'static str {
        match self {
            ShareRevoke::Deleted => "deleted",
            ShareRevoke::Offboarded => "offboarded",
        }
    }
}

/// Removes `conversation` (whose row the caller has locked) and everything it owns, in `tx` (magpie 9.6 steps 1-4).
///
/// # Errors
/// When a statement fails.
pub async fn purge_locked(tx: &Tx<'_>, person: &str, conversation: Uuid, why: ShareRevoke) -> Result<()> {
    // 1. its shares: revoke the live ones, remove every frozen snapshot, queue each frozen workspace's deletion
    let shares = tx
        .query(
            "update shares set revoked_at = coalesce(revoked_at, now()),
                               revoke_reason = coalesce(revoke_reason, $2),
                               conversation_id = null
             where conversation_id = $1
             returning id, snapshot, workspace_id",
            &[&conversation, &why.as_str()],
        )
        .await?;
    for s in &shares {
        let share_id: String = s.get(0);
        let snapshot: Uuid = s.get(1);
        let workspace: Uuid = s.get(2);
        drop_scope(tx, snapshot).await?;
        tx.execute("delete from share_rows where share_id = $1", &[&share_id])
            .await?;
        outbox::enqueue(
            tx,
            &Item::DeleteWorkspace {
                workspace_id: workspace,
            },
        )
        .await?;
    }
    // 2. the conversation (cascades: events, rows, row_seqs, turns, commands, tool links, uploads, sent files,
    //    sent staging, report ids), its uploads' blob refs, its cc scope, its live frames
    tx.execute(
        "delete from blob_refs where owner_kind = 'upload'
           and owner in (select id from uploads where conversation_id = $1)",
        &[&conversation],
    )
    .await?;
    drop_scope(tx, conversation).await?;
    tx.execute("delete from live_frames where conversation_id = $1", &[&conversation])
        .await?;
    tx.execute("delete from conversations where id = $1", &[&conversation])
        .await?;
    // 3. the person's counter and change; held polls of the conversation wake (and answer 404)
    tx.execute(
        "update persons set purged_conversations = purged_conversations + 1 where person = $1",
        &[&person],
    )
    .await?;
    changes::emit(
        tx,
        person,
        conversation,
        &ChangeBody::ConversationRemoved {
            reason: RemovedReason::Purged,
        },
    )
    .await?;
    let key = conversation.to_string();
    fanout::notify(tx, fanout::ROWS, &key, 0).await?;
    fanout::notify(tx, fanout::COMMANDS, &key, 0).await?;
    fanout::notify(tx, fanout::LIVE, &key, 0).await?;
    // 4. hermit: release the sandbox, delete the workspace (a conversation's workspace id is its id, hermit 3.8)
    outbox::enqueue(
        tx,
        &Item::ReleaseSandbox {
            conversation_id: conversation,
            reason: ConversationReleaseReason::Deleted,
        },
    )
    .await?;
    outbox::enqueue(
        tx,
        &Item::DeleteWorkspace {
            workspace_id: conversation,
        },
    )
    .await?;
    tracing::info!(conversation = %conversation, "conversation purged");
    Ok(())
}

/// Removes a cc scope (a conversation's or a share snapshot's): its files (lines and pages cascade), limbo,
/// operation ids and blob refs.
async fn drop_scope(tx: &Tx<'_>, scope: Uuid) -> Result<()> {
    tx.execute("delete from cc_files where scope = $1", &[&scope]).await?;
    tx.execute("delete from cc_limbo where scope = $1", &[&scope]).await?;
    tx.execute("delete from cc_ops where scope = $1", &[&scope]).await?;
    tx.execute("delete from blob_refs where owner = $1", &[&scope]).await?;
    Ok(())
}

/// The 30-day purge (magpie 15): conversations in Recently deleted for 30 days, at most `batch` per pass, each in
/// its own transaction; migrating conversations are skipped (cutover 6.4). How many were purged.
///
/// # Errors
/// When a statement fails.
pub async fn purge_due(pool: &Pool, batch: i64) -> Result<usize> {
    let rows = pool
        .get()
        .await?
        .query(
            "select id from conversations
             where deleted_at <= now() - interval '30 days' and not migrating
             order by deleted_at limit $1",
            &[&batch],
        )
        .await?;
    let mut n = 0;
    for r in rows {
        let id: Uuid = r.get(0);
        let purged = aviary_pg::tx!(pool, |t| -> Result<bool> {
            // re-checked under the lock: a restore or the importer may have come first
            let Some(row) = t
                .query_opt(
                    "select person from conversations
                     where id = $1 and deleted_at <= now() - interval '30 days' and not migrating
                     for update",
                    &[&id],
                )
                .await?
            else {
                return Ok(false);
            };
            let person: String = row.get(0);
            purge_locked(t, &person, id, ShareRevoke::Deleted).await?;
            Ok(true)
        })
        .await?;
        if purged {
            n += 1;
        }
    }
    Ok(n)
}
