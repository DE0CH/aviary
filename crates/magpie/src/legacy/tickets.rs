//! Tickets (switch-final 4.3 and 23 C5.1, cutover 6.4): pigeon takes one before every write it sends to a legacy
//! conversation through its legacy upstream, and reports the old stack's outcome. The importer fences only a
//! conversation without an open ticket, so a write the old stack is about to make is never cut off by a release.
//!
//! A ticket whose outcome never arrives (pigeon died or rolled out between the ticket and the report, or a ticket call
//! that timed out after it committed was not retried with its `clientMessageId`) would keep its conversation legacy
//! for ever. A ticket call repeated with the same `(kind, clientMessageId)` returns the open ticket instead of a
//! second one; anything still stranded is closed by the importer with [`expire`] once the old child shows that no
//! write under it can still be in flight (switch-final 8.1's "done or expired"; the design gap is recorded in
//! `crates/magpie/README.md`).

use aviary_pg::Pool;
use magpie_api::legacy::{TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketResponse, TicketRoute};
use time::OffsetDateTime;
use uuid::Uuid;

use super::fence::{self, Owner};
use crate::app::Magpie;
use crate::conversations::Person;
use crate::error::{Error, Result};
use crate::limits;

/// The NOTIFY channel a closed ticket wakes the importer on (payload `<conversation uuid>:0`): it syncs that one
/// conversation at once instead of at its next pass (switch-final A §3.3, wake on dirty). magpie does not listen.
pub const CHANNEL: &str = "magpie_legacy";

/// How many fences one ticket call waits through before giving up with 503 (each lasts at most 3 s; a new one needs
/// the importer's 60 s quiet period, so a second is not expected).
const FENCE_WAITS: usize = 3;

enum Take {
    Native,
    Legacy(Uuid),
    Wait,
}

/// `POST /v1/conversations/{id}/legacy/tickets`: `native` once magpie owns the conversation; a ticket while it is
/// legacy (a lapsed fence is cleared first); while it is fenced, waits for the release or the fence's end. With a
/// `clientMessageId`, a repeat of an open ticket's `(kind, clientMessageId)` returns that ticket (pigeon's retry of a
/// call that timed out after it committed opens no second one).
///
/// # Errors
/// 400 `bad_request` (a `clientMessageId` over 200 bytes), 404, 503 when fences never end; a statement error.
pub async fn take(app: &Magpie, person: &Person, id: Uuid, req: &TicketRequest) -> Result<TicketResponse> {
    if req
        .client_message_id
        .as_ref()
        .is_some_and(|c| c.len() > limits::IDEMPOTENCY_KEY_MAX)
    {
        return Err(Error::bad_request("clientMessageId must be at most 200 bytes"));
    }
    let kind = req.kind.as_str();
    let cmid = req.client_message_id.as_deref();
    for _ in 0..=FENCE_WAITS {
        let taken = aviary_pg::tx!(&app.pool, |t| -> Result<Take> {
            let r = t
                .query_opt(
                    "select person, migrating, legacy_fence_at > clock_timestamp() - make_interval(secs => $2)
                     from conversations where id = $1 for update",
                    &[&id, &limits::FENCE_LEASE.as_secs_f64()],
                )
                .await?
                .ok_or_else(Error::not_found)?;
            if r.get::<_, String>(0) != person.email {
                return Err(Error::not_found());
            }
            if !r.get::<_, bool>(1) {
                return Ok(Take::Native);
            }
            if r.get::<_, Option<bool>>(2) == Some(true) {
                return Ok(Take::Wait);
            }
            fence::lapse_in(t, id).await?;
            if let Some(cmid) = cmid
                && let Some(r) = t
                    .query_opt(
                        "select ticket from legacy_tickets
                         where conversation_id = $1 and kind = $2 and client_message_id = $3 and done_at is null",
                        &[&id, &kind, &cmid],
                    )
                    .await?
            {
                return Ok(Take::Legacy(r.get(0)));
            }
            let ticket = Uuid::now_v7();
            t.execute(
                "insert into legacy_tickets (ticket, conversation_id, kind, client_message_id) values ($1, $2, $3, $4)",
                &[&ticket, &id, &kind, &cmid],
            )
            .await?;
            Ok(Take::Legacy(ticket))
        })
        .await?;
        match taken {
            Take::Native => {
                return Ok(TicketResponse {
                    route: TicketRoute::Native,
                    ticket: None,
                });
            }
            Take::Legacy(ticket) => {
                return Ok(TicketResponse {
                    route: TicketRoute::Legacy,
                    ticket: Some(ticket),
                });
            }
            Take::Wait => match fence::await_unfenced(app, id).await? {
                None => return Err(Error::not_found()),
                Some(Owner::Native) => {
                    return Ok(TicketResponse {
                        route: TicketRoute::Native,
                        ticket: None,
                    });
                }
                Some(_) => {}
            },
        }
    }
    Err(fence::still_fenced())
}

fn kind_of(s: &str) -> Option<TicketKind> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

/// `POST /v1/legacy/tickets/{ticket}/done`: `delivered` is recorded and closes nothing; an outcome that fits the
/// ticket's kind closes it (and wakes the importer). A ticket already closed keeps its first outcome.
///
/// # Errors
/// 400 `bad_request` for an outcome that does not close this kind; 404 for an unknown ticket or another person's; a
/// statement error.
pub async fn done(app: &Magpie, person: &Person, ticket: Uuid, req: &TicketDone) -> Result<()> {
    let outcome = req.outcome;
    aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        let r = t
            .query_opt(
                "select t.kind, t.done_at is not null, c.person, t.conversation_id
                 from legacy_tickets t join conversations c on c.id = t.conversation_id
                 where t.ticket = $1 for update of t",
                &[&ticket],
            )
            .await?
            .ok_or_else(Error::not_found)?;
        if r.get::<_, String>(2) != person.email {
            return Err(Error::not_found());
        }
        if r.get::<_, bool>(1) {
            return Ok(());
        }
        let kind = kind_of(&r.get::<_, String>(0)).ok_or_else(|| Error::Internal("a ticket of unknown kind".into()))?;
        let conversation: Uuid = r.get(3);
        if outcome == TicketOutcome::Delivered {
            t.execute(
                "update legacy_tickets set delivered_at = coalesce(delivered_at, now()) where ticket = $1",
                &[&ticket],
            )
            .await?;
            return Ok(());
        }
        if !kind.closes_on(outcome) {
            return Err(Error::bad_request(format!(
                "outcome {} does not close a {} ticket",
                outcome.as_str(),
                kind.as_str()
            )));
        }
        t.execute(
            "update legacy_tickets set done_at = now(), outcome = $2 where ticket = $1",
            &[&ticket, &outcome.as_str()],
        )
        .await?;
        t.notify(CHANNEL, &format!("{conversation}:0")).await?;
        Ok(())
    })
    .await
}

/// Closes the stranded tickets of `id`: every open one issued before `issued_before` (database time), with outcome
/// `expired`, and wakes the importer as an outcome would. Answers how many were closed.
///
/// Only the importer (or `magpie import release <id> --force`, after inspection) calls this, and only with evidence
/// that no write under those tickets can still reach the old child or be in flight in it: pigeon never starts a
/// legacy write under a ticket older than [`magpie_api::legacy::TICKET_HOLD_MAX`], so with `issued_before` at most
/// the start of an old-child read (running list, and `pendingUserMessages`/`preparing` for the conversation) that
/// showed nothing for it, minus that bound, every such write has either not happened or settled. The quiet period
/// and the two running-set reads of the release rule still follow (switch-final 8.1, C5.2).
///
/// # Errors
/// When a statement fails.
pub async fn expire(pool: &Pool, id: Uuid, issued_before: OffsetDateTime) -> Result<u64> {
    aviary_pg::tx!(pool, |t| -> Result<u64> {
        let n = t
            .execute(
                "update legacy_tickets set done_at = clock_timestamp(), outcome = 'expired'
                 where conversation_id = $1 and done_at is null and issued_at < $2",
                &[&id, &issued_before],
            )
            .await?;
        if n > 0 {
            tracing::warn!(conversation = %id, tickets = n, "stranded tickets expired");
            t.notify(CHANNEL, &format!("{id}:0")).await?;
        }
        Ok(n)
    })
    .await
}
