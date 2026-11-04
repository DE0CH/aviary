//! Tickets (switch-final 4.3 and 23 C5.1, cutover 6.4): pigeon takes one before every write it sends to a legacy
//! conversation through its legacy upstream, and reports the old stack's outcome. The importer fences only a
//! conversation without an open ticket, so a write the old stack is about to make is never cut off by a release.
//!
//! A ticket whose outcome never arrives (pigeon died or rolled out between the ticket and the report, or a ticket call
//! that timed out after it committed was not retried with its `clientMessageId`) would keep its conversation legacy
//! for ever. A send's ticket call repeated with the same `clientMessageId` returns the open ticket instead of a second
//! one while it is younger than [`TICKET_REUSE_MAX`]; past that the old ticket is superseded (left open: a write
//! started under it may still be in flight), the repeat gets a new one and the answer names the old one, so a retried
//! send is never stuck behind a ticket pigeon may no longer write under. Anything still stranded is closed by the
//! importer with [`expire_settled`] once the old child shows that no write under it can still be in flight
//! (switch-final 8.1's "done or expired"; the design gap is recorded in `crates/magpie/README.md`).
//!
//! A reused ticket has two holders. Each hand-out gets a take of its own (the latest is kept), a send's write is
//! claimed with it right before it starts ([`claim`]: only the latest take may), and a `refused` from an older take
//! changes nothing ([`done`]), so a holder that gave up without writing never closes the ticket under the one that
//! writes; nor does a `refused` from a take that never claimed while another take's claim stands (that write may be
//! running). The reuse answer's `settleMs` covers a write claimed before it; a send's answer names its `prior` ticket.

use aviary_pg::{Pool, Tx};
use magpie_api::Timestamp;
use magpie_api::legacy::{
    PriorSend, Superseded, TICKET_CLAIM_SETTLE, TICKET_HOLD_MAX, TICKET_REUSE_MAX, TICKET_SETTLE_MAX, TicketClaim,
    TicketClaimed, TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketResponse, TicketRoute,
};
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
    Legacy(Issued),
    Wait,
}

struct Issued {
    ticket: Uuid,
    at: OffsetDateTime,
    hold_ms: u64,
    supersedes: Option<Superseded>,
    reused: bool,
    take: Uuid,
    settle_ms: Option<u64>,
    prior: Option<PriorSend>,
}

/// A `native` answer.
fn native() -> TicketResponse {
    TicketResponse {
        route: TicketRoute::Native,
        ticket: None,
        issued_at: None,
        hold_ms: None,
        supersedes: None,
        reused: false,
        take: None,
        settle_ms: None,
        prior: None,
    }
}

/// Milliseconds left of `bound` since `issued_at`, on the database clock (0 once it has passed).
fn left_sql(issued_at: &str, bound: &str) -> String {
    format!(
        "greatest(0, floor(1000 * ({bound}::float8 - extract(epoch from clock_timestamp() - {issued_at})::float8)))::int8"
    )
}

fn ms(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// `POST /v1/conversations/{id}/legacy/tickets`: `native` once magpie owns the conversation; a ticket while it is
/// legacy (a lapsed fence is cleared first); while it is fenced, waits for the release or the fence's end. A send's
/// `clientMessageId` repeated returns the open ticket for it while that ticket is younger than [`TICKET_REUSE_MAX`]
/// (pigeon's retry of a call that timed out after it committed opens no second one); an older one is superseded and a
/// new ticket issued (the old one stays open until its outcome or the importer's expiry, and the answer's
/// `supersedes` names it). Every legacy answer carries `holdMs`, what is left of [`TICKET_HOLD_MAX`].
///
/// # Errors
/// 400 `bad_request` (a `clientMessageId` over 200 bytes, or on a kind other than `send`), 404, 503 when fences never
/// end; a statement error.
pub async fn take(app: &Magpie, person: &Person, id: Uuid, req: &TicketRequest) -> Result<TicketResponse> {
    if req
        .client_message_id
        .as_ref()
        .is_some_and(|c| c.len() > limits::IDEMPOTENCY_KEY_MAX)
    {
        return Err(Error::bad_request("clientMessageId must be at most 200 bytes"));
    }
    if req.client_message_id.is_some() && req.kind != TicketKind::Send {
        // one key per write: a send's upload tickets sharing the send's id would share one ticket, closed by the
        // first file's `applied` while the other copies still write under it
        return Err(Error::bad_request("only a send's ticket carries a clientMessageId"));
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
            Ok(Take::Legacy(issue_in(t, id, kind, cmid).await?))
        })
        .await?;
        match taken {
            Take::Native => return Ok(native()),
            Take::Legacy(issued) => {
                return Ok(TicketResponse {
                    route: TicketRoute::Legacy,
                    ticket: Some(issued.ticket),
                    issued_at: Some(Timestamp(issued.at)),
                    hold_ms: Some(issued.hold_ms),
                    supersedes: issued.supersedes,
                    reused: issued.reused,
                    take: Some(issued.take),
                    settle_ms: issued.settle_ms,
                    prior: issued.prior,
                });
            }
            Take::Wait => match fence::await_unfenced(app, id).await? {
                None => return Err(Error::not_found()),
                Some(Owner::Native) => return Ok(native()),
                Some(_) => {}
            },
        }
    }
    Err(fence::still_fenced())
}

/// Issues a ticket in `t` (the conversation row is locked, legacy and unfenced), or hands back a send's open ticket
/// for `cmid` while it is younger than [`TICKET_REUSE_MAX`] (under a new take, with how long until a write claimed
/// under it has settled); an older one is superseded and named in the answer. A send's answer also names its prior
/// ticket ([`prior_in`]).
async fn issue_in(t: &Tx<'_>, id: Uuid, kind: &str, cmid: Option<&str>) -> Result<Issued> {
    let take = Uuid::now_v7();
    let mut supersedes = None;
    let mut reuse = None;
    if let Some(cmid) = cmid
        && let Some(r) = t
            .query_opt(
                &format!(
                    "select ticket, issued_at, clock_timestamp() - issued_at < make_interval(secs => $3),
                            {hold}, {settle}, case when claimed_at is null then 0::int8 else {claim} end
                     from legacy_tickets
                     where conversation_id = $1 and client_message_id = $2 and done_at is null
                       and superseded_at is null",
                    hold = left_sql("issued_at", "$4"),
                    settle = left_sql("issued_at", "$5"),
                    claim = left_sql("claimed_at", "$6"),
                ),
                &[
                    &id,
                    &cmid,
                    &TICKET_REUSE_MAX.as_secs_f64(),
                    &TICKET_HOLD_MAX.as_secs_f64(),
                    &TICKET_SETTLE_MAX.as_secs_f64(),
                    &TICKET_CLAIM_SETTLE.as_secs_f64(),
                ],
            )
            .await?
    {
        let ticket: Uuid = r.get(0);
        if r.get::<_, bool>(2) {
            // handed to this request under a take of its own: an older holder's `refused` no longer closes it, and
            // an older holder's claim no longer succeeds
            t.execute(
                "update legacy_tickets set take = $2 where ticket = $1",
                &[&ticket, &take],
            )
            .await?;
            reuse = Some((ticket, r.get::<_, OffsetDateTime>(1), ms(r.get(3)), ms(r.get(5))));
        } else {
            // too old to hand back (too little hold left): it stays open (a write may be in flight under it)
            // and the repeat gets a new ticket that names it, since the old child does not de-duplicate sends
            t.execute(
                "update legacy_tickets set superseded_at = clock_timestamp() where ticket = $1",
                &[&ticket],
            )
            .await?;
            tracing::info!(conversation = %id, %ticket, "a repeated send's ticket was past its reuse age: superseded");
            supersedes = Some(Superseded {
                ticket,
                settle_ms: ms(r.get(4)),
            });
        }
    }
    let (ticket, at, hold_ms, claim_settle) = if let Some(r) = reuse {
        r
    } else {
        let ticket = Uuid::now_v7();
        let r = t
            .query_one(
                &format!(
                    "insert into legacy_tickets (ticket, conversation_id, kind, client_message_id, issued_at, take)
                     values ($1, $2, $3, $4, clock_timestamp(), $6) returning issued_at, {hold}",
                    hold = left_sql("issued_at", "$5"),
                ),
                &[&ticket, &id, &kind, &cmid, &TICKET_HOLD_MAX.as_secs_f64(), &take],
            )
            .await?;
        (ticket, r.get(0), ms(r.get(1)), 0)
    };
    let (prior, prior_settle) = match cmid {
        Some(cmid) => prior_in(t, id, cmid, ticket, supersedes.as_ref().map(|s| s.ticket)).await?,
        None => (None, 0),
    };
    let reused = reuse.is_some();
    Ok(Issued {
        ticket,
        at,
        hold_ms,
        supersedes,
        reused,
        take,
        settle_ms: (reused || prior.is_some()).then_some(claim_settle.max(prior_settle)),
        prior,
    })
}

/// The send's earlier ticket other than `ticket` and the one it supersedes: the newest that closed on the old
/// child's acceptance (`started`, `completed`, `failed`), else the newest that may have written (expired, or
/// superseded and still open), with what is left of the settle time of the open ones.
async fn prior_in(
    t: &Tx<'_>,
    id: Uuid,
    cmid: &str,
    ticket: Uuid,
    superseded: Option<Uuid>,
) -> Result<(Option<PriorSend>, u64)> {
    let r = t
        .query_one(
            &format!(
                "select (array_agg(ticket order by issued_at desc)
                           filter (where outcome in ('started', 'completed', 'failed')))[1],
                        (array_agg(ticket order by issued_at desc)
                           filter (where outcome = 'expired' or done_at is null))[1],
                        coalesce(max({settle}) filter (where done_at is null), 0)::int8
                 from legacy_tickets
                 where conversation_id = $1 and client_message_id = $2 and ticket <> $3
                   and ticket is distinct from $4",
                settle = left_sql("issued_at", "$5"),
            ),
            &[&id, &cmid, &ticket, &superseded, &TICKET_SETTLE_MAX.as_secs_f64()],
        )
        .await?;
    let prior = match (r.get::<_, Option<Uuid>>(0), r.get::<_, Option<Uuid>>(1)) {
        (Some(ticket), _) => Some(PriorSend { ticket, accepted: true }),
        (None, Some(ticket)) => Some(PriorSend {
            ticket,
            accepted: false,
        }),
        (None, None) => None,
    };
    Ok((prior, ms(r.get(2))))
}

fn kind_of(s: &str) -> Option<TicketKind> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).ok()
}

/// `POST /v1/legacy/tickets/{ticket}/done`: `delivered` is recorded and closes nothing; an outcome that fits the
/// ticket's kind closes it (and wakes the importer). A ticket already closed keeps its first outcome, and a `refused`
/// from a take that is no longer the ticket's latest changes nothing (that holder wrote nothing; the request that
/// holds the ticket now reports, or the importer expires it). Nor does a `refused` from a take other than the one the
/// write was last claimed under: the latest holder gave up without writing, but the claimed write may be running.
///
/// # Errors
/// 400 `bad_request` for an outcome that does not close this kind; 404 for an unknown ticket or another person's; a
/// statement error.
pub async fn done(app: &Magpie, person: &Person, ticket: Uuid, req: &TicketDone) -> Result<()> {
    let outcome = req.outcome;
    aviary_pg::tx!(&app.pool, |t| -> Result<()> {
        let r = t
            .query_opt(
                "select t.kind, t.done_at is not null, c.person, t.conversation_id, t.take, t.claim_take
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
        let latest: Option<Uuid> = r.get(4);
        if outcome == TicketOutcome::Refused && req.take.is_some() && latest.is_some() && req.take != latest {
            tracing::info!(%conversation, %ticket, "a refused from an earlier take: the ticket stays with its holder");
            return Ok(());
        }
        let claimer: Option<Uuid> = r.get(5);
        if outcome == TicketOutcome::Refused && req.take.is_some() && claimer.is_some() && req.take != claimer {
            // the latest holder gave up without writing, but an earlier take claimed the write and it may be running:
            // the ticket stays open for that holder's outcome or the importer's settled expiry
            tracing::info!(%conversation, %ticket, "a refused from a take that did not claim: an earlier claim stands");
            return Ok(());
        }
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

/// `POST /v1/legacy/tickets/{ticket}/claim`: a send's write under the ticket is about to start. `claimed` only for the
/// ticket's latest take while it is open, within [`TICKET_HOLD_MAX`] of its issue (database clock), and while no other
/// ticket of the same send closed on the old child's acceptance; the claim's time bounds the write for a later reuse
/// ([`TICKET_CLAIM_SETTLE`]).
///
/// # Errors
/// 404 for an unknown ticket or another person's; a statement error.
pub async fn claim(app: &Magpie, person: &Person, ticket: Uuid, req: &TicketClaim) -> Result<TicketClaimed> {
    aviary_pg::tx!(&app.pool, |t| -> Result<TicketClaimed> {
        let r = t
            .query_opt(
                "select c.person, t.done_at is not null or clock_timestamp() - t.issued_at >= make_interval(secs => $2),
                        t.take,
                        t.client_message_id is not null and exists (
                          select 1 from legacy_tickets o
                          where o.conversation_id = t.conversation_id and o.client_message_id = t.client_message_id
                            and o.ticket <> t.ticket and o.outcome in ('started', 'completed', 'failed'))
                 from legacy_tickets t join conversations c on c.id = t.conversation_id
                 where t.ticket = $1 for update of t",
                &[&ticket, &TICKET_HOLD_MAX.as_secs_f64()],
            )
            .await?
            .ok_or_else(Error::not_found)?;
        if r.get::<_, String>(0) != person.email {
            return Err(Error::not_found());
        }
        if r.get::<_, bool>(1) || r.get::<_, bool>(3) {
            return Ok(TicketClaimed {
                claimed: false,
                held_elsewhere: false,
            });
        }
        if r.get::<_, Option<Uuid>>(2).is_some_and(|latest| latest != req.take) {
            return Ok(TicketClaimed {
                claimed: false,
                held_elsewhere: true,
            });
        }
        t.execute(
            "update legacy_tickets set claimed_at = clock_timestamp(), claim_take = $2 where ticket = $1",
            &[&ticket, &req.take],
        )
        .await?;
        Ok(TicketClaimed {
            claimed: true,
            held_elsewhere: false,
        })
    })
    .await
}

/// Closes the stranded tickets of `id`: every open one (superseded ones included) issued before `issued_before`
/// (database time), with outcome `expired`, and wakes the importer as an outcome would. Answers how many were closed.
///
/// This is the raw form: `magpie import release <id> --force` calls it with [`now`] after the operator's inspection.
/// The hand-over loop calls [`expire_settled`], which derives the bound from an old-child read.
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

/// The database's `clock_timestamp()`: the clock tickets are issued on. The importer takes it right before an
/// old-child read and passes it to [`expire_settled`], so its own clock never enters the bound.
///
/// # Errors
/// When the statement fails.
pub async fn now(pool: &Pool) -> Result<OffsetDateTime> {
    Ok(pool
        .get()
        .await?
        .query_one("select clock_timestamp()", &[])
        .await?
        .get(0))
}

/// The hand-over loop's expiry: closes the open tickets of `id` issued more than [`TICKET_SETTLE_MAX`] before
/// `read_started` ([`now`] taken just before an old-child read, of the running list and of
/// `pendingUserMessages`/`preparing`, that showed nothing for the conversation). pigeon starts no write under a
/// ticket later than [`TICKET_HOLD_MAX`] after its issue (judged by `holdMs` from the moment it sent the call, so its
/// clock does not matter) and aborts a write that has no answer within `TICKET_WRITE_TIMEOUT`; with the 10 s of
/// transit slack in `TICKET_SETTLE_MAX`, any write under those tickets had either reached the old child before the
/// read (and would have shown in it until it settled) or never will. The quiet period and the two running-set reads
/// of the release rule still follow (switch-final 8.1, C5.2).
///
/// # Errors
/// When a statement fails.
pub async fn expire_settled(pool: &Pool, id: Uuid, read_started: OffsetDateTime) -> Result<u64> {
    let settle =
        time::Duration::try_from(TICKET_SETTLE_MAX).map_err(|e| Error::Internal(format!("TICKET_SETTLE_MAX: {e}")))?;
    expire(pool, id, read_started - settle).await
}
