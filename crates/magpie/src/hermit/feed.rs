//! The hermit feed follower (magpie 8.7): the job leader long-polls hermit's sandbox-events feed
//! (`GET /v1/sandbox-events?after=<seq>&limit=500&waitMs=25000`, 35 s timeout) from `hermit_feed.after_seq`
//! (`latest` on the first start), and applies each page in one transaction with the cursor, so a takeover mid-page
//! applies it again (every effect is idempotent) and never skips one.
//!
//! For each event: the conversation's recorded sandbox (`hermit_sandbox`, `hermit_state`, `hermit_version`) when it
//! is newer (unknown and migrating conversations are ignored); for `gone`: the lease of that sandbox is cleared, a
//! turn of that sandbox ends (`sandbox.lost`; `sandbox.busy` / `sandbox.failed` for a start that failed) or, when
//! its `start_turn` was never acknowledged, goes back to preparing for a new sandbox, and a held conversation goes
//! idle. A 410 `cursor_expired` re-reads the sandbox of every conversation with an active turn or a lease and
//! continues from `latest` once every re-read was answered (else the next step recovers again).

use std::sync::Arc;

use aviary_pg::{Leader, Tx};
use hermit_api::Timestamp;
use hermit_api::sandbox::{Cursor, SandboxEvent, SandboxEventsQuery, SandboxQuery, SandboxState};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::changes;
use crate::error::{Error, Result};
use crate::limits;
use crate::turns::driver::{Driver, Gone, Seen, gone_verdict, lock_id, reacquire, record_sandbox};
use crate::turns::end::{self, Ending};
use crate::turns::{active, start_turn_status};

/// Starts following the feed while this replica leads.
#[must_use]
pub fn spawn(leader: Arc<Leader>, d: Driver) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut leading = leader.is_leader();
        loop {
            if d.draining.is_draining() {
                return;
            }
            if !*leading.borrow() {
                if leading.changed().await.is_err() {
                    return;
                }
                continue;
            }
            if !leader.confirm().await {
                tokio::time::sleep(limits::FEED_RETRY).await;
                continue;
            }
            if let Err(e) = step(&d).await {
                tracing::warn!(error = %e, "hermit feed: pass failed");
                tokio::time::sleep(limits::FEED_RETRY).await;
            }
        }
    })
}

async fn cursor(d: &Driver) -> Result<Option<i64>> {
    let conn = d.pool.get().await?;
    conn.execute("insert into hermit_feed default values on conflict do nothing", &[])
        .await?;
    Ok(conn
        .query_one("select after_seq from hermit_feed where id", &[])
        .await?
        .get(0))
}

async fn store(d: &Driver, seq: u64) -> Result<()> {
    let seq = i64::try_from(seq).unwrap_or(i64::MAX);
    d.pool
        .get()
        .await?
        .execute(
            "update hermit_feed set after_seq = greatest(coalesce(after_seq, 0), $1), updated_at = now() where id",
            &[&seq],
        )
        .await?;
    Ok(())
}

async fn head(d: &Driver) -> Result<u64> {
    let q = SandboxEventsQuery {
        after: Cursor::Latest,
        limit: None,
        wait_ms: None,
    };
    match tokio::time::timeout(limits::FEED_TIMEOUT, d.hermit.sandbox_events(q)).await {
        Ok(Ok(page)) => Ok(page.head_seq),
        Ok(Err(e)) => Err(Error::Internal(format!("hermit feed: {e}"))),
        Err(_) => Err(Error::Internal("hermit feed: timed out".into())),
    }
}

/// One long-poll of the feed and its page, or the first start, or a recovery after `cursor_expired`.
///
/// # Errors
/// When hermit cannot be reached or a statement fails (the caller retries).
pub async fn step(d: &Driver) -> Result<()> {
    let Some(after) = cursor(d).await? else {
        let h = head(d).await?;
        store(d, h).await?;
        tracing::info!(head = h, "hermit feed: following from latest");
        return Ok(());
    };
    let q = SandboxEventsQuery {
        after: Cursor::Seq(u64::try_from(after).unwrap_or(0)),
        limit: Some(limits::FEED_LIMIT),
        wait_ms: Some(limits::FEED_WAIT_MS),
    };
    let page = match tokio::time::timeout(limits::FEED_TIMEOUT, d.hermit.sandbox_events(q)).await {
        Ok(Ok(page)) => page,
        Ok(Err(e)) if e.status() == Some(410) => return recover(d).await,
        Ok(Err(e)) => return Err(Error::Internal(format!("hermit feed: {e}"))),
        Err(_) => return Ok(()),
    };
    let Some(last) = page.events.iter().map(|e| e.seq).max() else {
        return Ok(());
    };
    let last = i64::try_from(last).unwrap_or(i64::MAX);
    aviary_pg::tx!(&d.pool, |t| -> Result<()> {
        for ev in &page.events {
            apply(t, ev).await?;
        }
        t.execute(
            "update hermit_feed set after_seq = greatest(coalesce(after_seq, 0), $1), updated_at = now() where id",
            &[&last],
        )
        .await?;
        Ok(())
    })
    .await
}

/// Applies one feed event (or one re-read sandbox) in `tx`.
///
/// # Errors
/// When a statement fails.
pub async fn apply(tx: &Tx<'_>, ev: &SandboxEvent) -> Result<()> {
    let Some(c) = lock_id(tx, ev.conversation_id).await? else {
        return Ok(());
    };
    if c.migrating {
        return Ok(());
    }
    record_sandbox(tx, ev.conversation_id, ev.sandbox_id, ev.state, ev.version, Seen::Feed).await?;
    if ev.state != SandboxState::Gone {
        return Ok(());
    }
    gone(tx, &c, ev.conversation_id, ev.sandbox_id, ev).await
}

async fn gone(
    tx: &Tx<'_>,
    c: &crate::turns::driver::Conv,
    conversation: Uuid,
    sandbox: Uuid,
    ev: &SandboxEvent,
) -> Result<()> {
    tx.execute(
        "update conversations set lease_id = null, lease_sandbox = null where id = $1 and lease_sandbox = $2",
        &[&conversation, &sandbox],
    )
    .await?;
    if let Some(turn) = active(tx, conversation).await?.filter(|t| !t.state.is_ended()) {
        if turn.sandbox != Some(sandbox) {
            return Ok(());
        }
        let (_, acked) = start_turn_status(tx, &turn).await?;
        match gone_verdict(&turn, acked, c.deleted, ev.release.as_ref()) {
            Gone::Fail(e) => end::end(tx, &c.person, &turn, Ending::failed(e)).await?,
            Gone::Reacquire => reacquire(tx, &c.person, c, &turn, None).await?,
        }
        return Ok(());
    }
    let held = tx
        .query_opt(
            "select background from conversations where id = $1 and run_state = 'held' and active_turn is null",
            &[&conversation],
        )
        .await?;
    if let Some(r) = &held {
        // the sandbox's tasks went with it: their launch rows end (rule 7)
        crate::turns::untracked(tx, conversation, None, &r.get(0), &[]).await?;
    }
    let n = tx
        .execute(
            "update conversations set run_state = 'idle', background = '[]'::jsonb
             where id = $1 and run_state = 'held' and active_turn is null",
            &[&conversation],
        )
        .await?;
    if n > 0 {
        changes::upserted(tx, &c.person, conversation).await?;
    }
    Ok(())
}

/// After `cursor_expired`: re-reads the sandbox of every conversation with an active turn or a lease, applies what
/// hermit says, and continues from `latest` (taken first, so nothing between is missed), but only once every re-read
/// was answered.
///
/// # Errors
/// When hermit's head cannot be read, a re-read was not answered (the cursor stays, the next step recovers again),
/// or a statement fails.
pub async fn recover(d: &Driver) -> Result<()> {
    let h = head(d).await?;
    let rows = d
        .pool
        .get()
        .await?
        .query(
            "select c.id, c.person, array_remove(array[t.sandbox_id, c.lease_sandbox, c.hermit_sandbox], null)
             from conversations c left join turns t on t.id = c.active_turn
             where (c.active_turn is not null or c.lease_id is not null) and not c.migrating",
            &[],
        )
        .await?;
    tracing::warn!(
        conversations = rows.len(),
        "hermit feed: cursor expired, re-reading live sandboxes"
    );
    let mut unread = 0usize;
    for r in rows {
        let (conversation, person, known): (Uuid, String, Vec<Uuid>) = (r.get(0), r.get(1), r.get(2));
        let read = tokio::time::timeout(
            limits::HERMIT_READ_TIMEOUT,
            d.hermit.get_sandbox(conversation, SandboxQuery::default()),
        )
        .await;
        let events: Vec<SandboxEvent> = match read {
            Ok(Ok(s)) => {
                let mut evs: Vec<SandboxEvent> = known
                    .iter()
                    .filter(|id| **id != s.id)
                    .map(|id| synthetic_gone(conversation, &person, *id))
                    .collect();
                evs.push(SandboxEvent {
                    seq: 0,
                    at: Timestamp::now(),
                    sandbox_id: s.id,
                    conversation_id: conversation,
                    person: s.person.clone(),
                    state: s.state,
                    phase: s.phase,
                    version: s.version,
                    busy: s.busy,
                    release: s.release.clone(),
                });
                evs
            }
            Ok(Err(e)) if e.status() == Some(404) => known
                .iter()
                .map(|id| synthetic_gone(conversation, &person, *id))
                .collect(),
            Ok(Err(e)) => {
                tracing::warn!(conversation = %conversation, error = %e, "hermit feed recovery: no answer");
                unread += 1;
                continue;
            }
            Err(_) => {
                tracing::warn!(conversation = %conversation, "hermit feed recovery: timed out");
                unread += 1;
                continue;
            }
        };
        aviary_pg::tx!(&d.pool, |t| -> Result<()> {
            for ev in &events {
                // a sandbox hermit no longer names is gone, whatever the record says
                if ev.seq == 0 && ev.state == SandboxState::Gone {
                    if let Some(c) = lock_id(t, ev.conversation_id).await?
                        && !c.migrating
                    {
                        gone(t, &c, ev.conversation_id, ev.sandbox_id, ev).await?;
                    }
                } else {
                    apply(t, ev).await?;
                }
            }
            Ok(())
        })
        .await?;
    }
    // a conversation hermit did not answer for may have a `gone` in the expired range: the cursor stays, so the next
    // step gets 410 again and re-reads (what was applied is idempotent)
    if unread > 0 {
        return Err(Error::Internal(format!(
            "hermit feed recovery: {unread} conversations not re-read, retrying"
        )));
    }
    d.pool
        .get()
        .await?
        .execute(
            "update hermit_feed set after_seq = $1, updated_at = now() where id",
            &[&i64::try_from(h).unwrap_or(i64::MAX)],
        )
        .await?;
    Ok(())
}

fn synthetic_gone(conversation: Uuid, person: &str, sandbox: Uuid) -> SandboxEvent {
    SandboxEvent {
        seq: 0,
        at: Timestamp::now(),
        sandbox_id: sandbox,
        conversation_id: conversation,
        person: person.to_owned(),
        state: SandboxState::Gone,
        phase: None,
        version: 0,
        busy: false,
        release: None,
    }
}
