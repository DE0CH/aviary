//! The switch window's ticket rules (`magpie_api::legacy`; WP-51's contract with pigeon), from the outside: a
//! send's ticket and its reuse under a new take, the one writer of a reused ticket (claim), a `refused` from an earlier
//! take that changes nothing, `prior` and `settleMs`, the hold, a superseded ticket, what closes which kind, and the
//! `native` answer. It runs against magpie (`crates/magpie/tests/legacy.rs`) and against pigeon's in-memory model of
//! magpie (`crates/pigeon/tests/common/model.rs`, `tests/legacy.rs`), so pigeon's exactly-once tests rest on the
//! rules magpie keeps. Each check uses a conversation of its own; the hooks make one, age a ticket (as if issued that
//! much earlier), read a ticket's stored outcome, and release a conversation.

use std::time::Duration;

use futures_util::future::BoxFuture;
use magpie_api::client::Client;
use magpie_api::legacy::{
    PriorSend, TicketClaim, TicketClaimed, TicketDone, TicketKind, TicketOutcome, TicketRequest, TicketResponse,
    TicketRoute,
};
use uuid::Uuid;

use crate::report::{Outcome, Report};
use crate::{ensure, fail};

/// What the suite asks of the implementation beyond its routes.
pub struct Hooks {
    /// A new conversation of the suite's person that the old stack owns (`migrating`).
    pub legacy_conversation: Box<dyn Fn() -> BoxFuture<'static, Uuid> + Send + Sync>,
    /// Moves a ticket's issue time back by the duration.
    pub age_ticket: Box<dyn Fn(Uuid, Duration) -> BoxFuture<'static, ()> + Send + Sync>,
    /// A ticket's stored outcome (`None` while open).
    pub outcome: Box<dyn Fn(Uuid) -> BoxFuture<'static, Option<TicketOutcome>> + Send + Sync>,
    /// The importer's release of a conversation with no open ticket (magpie owns it from then).
    pub release: Box<dyn Fn(Uuid) -> BoxFuture<'static, ()> + Send + Sync>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hooks(..)")
    }
}

/// The suite's inputs.
#[derive(Debug)]
pub struct Options {
    /// pigeon's client of the implementation.
    pub client: Client,
    /// The person the conversations belong to.
    pub person: String,
    pub hooks: Hooks,
}

fn send(cmid: &str) -> TicketRequest {
    TicketRequest {
        kind: TicketKind::Send,
        client_message_id: Some(cmid.to_owned()),
    }
}

fn of(kind: TicketKind) -> TicketRequest {
    TicketRequest {
        kind,
        client_message_id: None,
    }
}

struct S<'a>(&'a Options);

impl S<'_> {
    async fn ticket(&self, c: Uuid, r: &TicketRequest) -> Result<TicketResponse, String> {
        self.0
            .client
            .person(&self.0.person)
            .legacy_ticket(c, r)
            .await
            .map_err(|e| format!("ticket: {e}"))
    }

    async fn claim(&self, t: Uuid, take: Uuid) -> Result<TicketClaimed, String> {
        self.0
            .client
            .person(&self.0.person)
            .legacy_ticket_claim(t, &TicketClaim { take })
            .await
            .map_err(|e| format!("claim: {e}"))
    }

    async fn done(&self, t: Uuid, outcome: TicketOutcome, take: Option<Uuid>) -> Result<(), String> {
        self.0
            .client
            .person(&self.0.person)
            .legacy_ticket_done(t, &TicketDone { outcome, take })
            .await
            .map_err(|e| format!("done: {e}"))
    }

    /// An outcome that does not close the ticket's kind is refused (400).
    async fn not_closing(&self, t: Uuid, outcome: TicketOutcome) -> Result<(), String> {
        match self
            .0
            .client
            .person(&self.0.person)
            .legacy_ticket_done(t, &TicketDone { outcome, take: None })
            .await
        {
            Err(e) if e.api().is_some_and(|r| r.status == 400) => Ok(()),
            other => Err(format!("{outcome:?} on a ticket it does not close: {other:?}")),
        }
    }

    async fn outcome(&self, t: Uuid) -> Option<TicketOutcome> {
        (self.0.hooks.outcome)(t).await
    }

    async fn conversation(&self) -> Uuid {
        (self.0.hooks.legacy_conversation)().await
    }
}

fn held(t: &TicketResponse) -> Result<(Uuid, Uuid), String> {
    match (t.route, t.ticket, t.take) {
        (TicketRoute::Legacy, Some(ticket), Some(take)) => Ok((ticket, take)),
        _ => Err(format!("not a legacy hand-out: {t:?}")),
    }
}

const CLAIMED: TicketClaimed = TicketClaimed {
    claimed: true,
    held_elsewhere: false,
};
const ELSEWHERE: TicketClaimed = TicketClaimed {
    claimed: false,
    held_elsewhere: true,
};
const CLOSED: TicketClaimed = TicketClaimed {
    claimed: false,
    held_elsewhere: false,
};

/// A send's first ticket: legacy, a take, the whole hold left, nothing reused or prior.
async fn first(s: &S<'_>) -> Outcome {
    let c = s.conversation().await;
    let t = s.ticket(c, &send("cf:1")).await?;
    held(&t)?;
    let hold = t.hold_ms.unwrap_or(0);
    ensure!(hold > 55_000 && hold <= 60_000, "holdMs {hold}");
    ensure!(
        !t.reused && t.prior.is_none() && t.supersedes.is_none() && t.settle_ms.is_none(),
        "{t:?}"
    );
    Ok(())
}

/// A resend gets the open ticket under a new take with the claimed write's settle time; only the latest take
/// claims; an earlier take's `refused` changes nothing; a real outcome closes it whichever take reports it.
async fn reuse(s: &S<'_>) -> Outcome {
    let c = s.conversation().await;
    let (tk, a) = held(&s.ticket(c, &send("cf:r")).await?)?;
    ensure!(s.claim(tk, a).await? == CLAIMED, "the first take claims");
    let second = s.ticket(c, &send("cf:r")).await?;
    let (tk2, b) = held(&second)?;
    ensure!(tk2 == tk && second.reused && b != a, "{second:?}");
    let settle = second.settle_ms.unwrap_or(0);
    ensure!(settle > 25_000 && settle <= 30_000, "settleMs {settle}");
    ensure!(s.claim(tk, a).await? == ELSEWHERE, "the earlier take is held elsewhere");
    s.done(tk, TicketOutcome::Refused, Some(a)).await?;
    ensure!(s.outcome(tk).await.is_none(), "an earlier take's refused closed it");
    ensure!(s.claim(tk, b).await? == CLAIMED, "the latest take claims");
    s.done(tk, TicketOutcome::Started, Some(a)).await?;
    ensure!(
        s.outcome(tk).await == Some(TicketOutcome::Started),
        "a real outcome closes it"
    );
    Ok(())
}

/// A resend after the old child accepted the send: a new ticket naming the accepted one, nothing to settle, no
/// claim under it; its `refused` closes it.
async fn prior_accepted(s: &S<'_>) -> Outcome {
    let c = s.conversation().await;
    let (tk, a) = held(&s.ticket(c, &send("cf:p")).await?)?;
    ensure!(s.claim(tk, a).await? == CLAIMED, "claimed");
    s.done(tk, TicketOutcome::Started, Some(a)).await?;
    let third = s.ticket(c, &send("cf:p")).await?;
    let (t3, k3) = held(&third)?;
    ensure!(t3 != tk && !third.reused, "{third:?}");
    ensure!(
        third.prior
            == Some(PriorSend {
                ticket: tk,
                accepted: true
            }),
        "{third:?}"
    );
    ensure!(third.settle_ms == Some(0), "{third:?}");
    ensure!(s.claim(t3, k3).await? == CLOSED, "no write after an acceptance");
    s.done(t3, TicketOutcome::Refused, Some(k3)).await?;
    ensure!(s.outcome(t3).await == Some(TicketOutcome::Refused), "closed refused");
    Ok(())
}

/// A reuse of a ticket nobody claimed has nothing to settle; the latest take's `refused` closes it, and a ticket
/// closed `refused` (nothing written) is no prior.
async fn refused_is_no_prior(s: &S<'_>) -> Outcome {
    let c = s.conversation().await;
    let d = s.ticket(c, &send("cf:n")).await?;
    let e = s.ticket(c, &send("cf:n")).await?;
    let ((td, kd), (te, ke)) = (held(&d)?, held(&e)?);
    ensure!(te == td && e.settle_ms == Some(0) && e.prior.is_none(), "{e:?}");
    s.done(td, TicketOutcome::Refused, Some(kd)).await?;
    ensure!(s.outcome(td).await.is_none(), "an earlier take's refused closed it");
    s.done(te, TicketOutcome::Refused, Some(ke)).await?;
    ensure!(
        s.outcome(td).await == Some(TicketOutcome::Refused),
        "the latest take's refused closes it"
    );
    let f = s.ticket(c, &send("cf:n")).await?;
    let (tf, _) = held(&f)?;
    ensure!(tf != td && f.prior.is_none() && f.settle_ms.is_none(), "{f:?}");
    Ok(())
}

/// Past the hold a claim is refused (a write needs a new ticket); a reuse reports what is left of the hold.
async fn hold(s: &S<'_>) -> Outcome {
    let c = s.conversation().await;
    let (tk, take) = held(&s.ticket(c, &send("cf:h")).await?)?;
    (s.0.hooks.age_ticket)(tk, Duration::from_secs(20)).await;
    let again = s.ticket(c, &send("cf:h")).await?;
    let left = again.hold_ms.unwrap_or(0);
    ensure!(again.reused && left <= 40_000 && left > 35_000, "{again:?}");
    let (_, latest) = held(&again)?;
    ensure!(s.claim(tk, take).await? == ELSEWHERE, "the earlier take");
    (s.0.hooks.age_ticket)(tk, Duration::from_secs(41)).await;
    ensure!(s.claim(tk, latest).await? == CLOSED, "claimed past the hold");
    Ok(())
}

/// A resend past the reuse age gets a new ticket that names the open one in `supersedes`, with what is left of
/// its settle time; the superseded ticket stays open.
async fn supersede(s: &S<'_>) -> Outcome {
    let c = s.conversation().await;
    let (tg, _) = held(&s.ticket(c, &send("cf:s")).await?)?;
    (s.0.hooks.age_ticket)(tg, Duration::from_secs(31)).await;
    let h = s.ticket(c, &send("cf:s")).await?;
    let (th, _) = held(&h)?;
    ensure!(th != tg && !h.reused, "{h:?}");
    let Some(sup) = &h.supersedes else {
        fail!("no supersedes: {h:?}")
    };
    ensure!(sup.ticket == tg, "{h:?}");
    ensure!(
        sup.settle_ms <= 59_000 && sup.settle_ms > 54_000,
        "settleMs {}",
        sup.settle_ms
    );
    ensure!(s.outcome(tg).await.is_none(), "the superseded ticket stays open");
    Ok(())
}

/// Only a send's ticket carries a `clientMessageId`; a send closes only on its turn's outcome, any other kind on
/// `applied` or `refused` (an outcome that does not close the kind is 400, `delivered` changes nothing, and a closed
/// ticket keeps its outcome).
async fn kinds(s: &S<'_>) -> Outcome {
    let c = s.conversation().await;
    let bad =
        s.0.client
            .person(&s.0.person)
            .legacy_ticket(
                c,
                &TicketRequest {
                    kind: TicketKind::Stop,
                    client_message_id: Some("cf:k".into()),
                },
            )
            .await;
    match bad {
        Err(e) if e.api().is_some_and(|r| r.status == 400) => {}
        other => fail!("a Stop with a clientMessageId: {other:?}"),
    }
    let (stop, _) = held(&s.ticket(c, &of(TicketKind::Stop)).await?)?;
    s.not_closing(stop, TicketOutcome::Started).await?;
    s.done(stop, TicketOutcome::Delivered, None).await?;
    ensure!(s.outcome(stop).await.is_none(), "a Stop closed on started or delivered");
    s.done(stop, TicketOutcome::Applied, None).await?;
    ensure!(
        s.outcome(stop).await == Some(TicketOutcome::Applied),
        "a Stop closes on applied"
    );
    s.done(stop, TicketOutcome::Refused, None).await?;
    ensure!(
        s.outcome(stop).await == Some(TicketOutcome::Applied),
        "a closed ticket keeps its outcome"
    );
    let (snd, _) = held(&s.ticket(c, &send("cf:k")).await?)?;
    s.not_closing(snd, TicketOutcome::Applied).await?;
    ensure!(s.outcome(snd).await.is_none(), "a send closed on applied");
    s.done(snd, TicketOutcome::Completed, None).await?;
    ensure!(
        s.outcome(snd).await == Some(TicketOutcome::Completed),
        "a send closes on its turn's end"
    );
    let (up, _) = held(&s.ticket(c, &of(TicketKind::Upload)).await?)?;
    ensure!(up != snd, "an upload ticket is its own");
    Ok(())
}

/// Once magpie owns the conversation every ticket call answers `native`, with no ticket.
async fn native(s: &S<'_>) -> Outcome {
    let c = s.conversation().await;
    (s.0.hooks.release)(c).await;
    let t = s.ticket(c, &send("cf:x")).await?;
    ensure!(
        t.route == TicketRoute::Native && t.ticket.is_none() && t.take.is_none(),
        "{t:?}"
    );
    Ok(())
}

/// Runs every check.
pub async fn run(o: Options, report: &Report) {
    let s = S(&o);
    report.check("tickets.first-send", first(&s)).await;
    report.check("tickets.reuse-one-writer", reuse(&s)).await;
    report.check("tickets.prior-accepted", prior_accepted(&s)).await;
    report
        .check("tickets.refused-is-no-prior", refused_is_no_prior(&s))
        .await;
    report.check("tickets.hold", hold(&s)).await;
    report.check("tickets.supersede", supersede(&s)).await;
    report.check("tickets.kinds", kinds(&s)).await;
    report.check("tickets.native", native(&s)).await;
}
