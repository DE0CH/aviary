//! The router is the truth about sessions: every few seconds hermit-jarvis lists the aviary sessions and ends each
//! live sandbox whose session is no longer up (Jarvis 4 paused it after its idle hour: `idle`; destroyed, failed or
//! gone from the list: `vanished`), and each resume whose machine never booted (`start_timed_out`). Every replica runs
//! it; each change is a row-locked update that does nothing once the row is gone, so two replicas agree.
//!
//! A conversation begun in the chat (Deyao, 2026-11-18) waits for the session hermit-jarvis proposed for it: its start
//! is in phase `approval` while the router lists the session as waiting for Deyao's approval, moves on to `host` (its
//! machine is being made) once it is approved, and ends `not_allowed` when the session is gone from the list (denied,
//! or its approval lapsed), which also takes its conversation, held message and all, out of the chat. [`verdict`] is
//! that rule; [`pass`] carries it out.
//!
//! The same pass keeps each session's conversation in the chat in step (Deyao, 2026-11-12: the lifecycle is managed
//! in Jarvis 4, not in the chat): a session that is not up has its conversation closed to its person's messages, with
//! a sentence saying where to resume it; one that is up again is opened; a destroyed one's conversation leaves the
//! chat. magpie's calls are idempotent, so two replicas doing the same one agree too.
//!
//! A conversation that left the chat comes back when its session is in the router's list again: the session was not
//! destroyed, the list had left it out (2026-11-16: the router hid a paused session while its resume waited for
//! Deyao's approval, and three restarted sessions lost their conversations until this pass learnt to restore).

use std::collections::HashMap;
use std::time::Duration;

use hermit_api::sandbox::{ReleaseReason, SandboxPhase, SandboxState};
use time::OffsetDateTime;

use crate::app::{App, RESUME_DEADLINE};
use crate::db::{self, SandboxRow};
use crate::jarvis::{APPROVAL, FROZEN, is_up};

/// How often the router is asked.
pub const EVERY: Duration = Duration::from_secs(10);

/// Runs for ever.
pub async fn run(app: App) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = pass(&app).await {
            // the router unreachable proves nothing about a machine: nothing is ended
            tracing::warn!(error = %e, "reconcile pass skipped");
        }
    }
}

/// One pass.
///
/// # Errors
/// The router or the database unreachable (nothing changed).
pub async fn pass(app: &App) -> anyhow::Result<()> {
    let live = db::live(&app.pool).await?;
    let chats = db::chats(&app.pool).await?;
    if live.is_empty() && chats.is_empty() {
        return Ok(());
    }
    let states: HashMap<String, String> = app
        .jarvis
        .sessions()
        .await?
        .into_iter()
        .map(|s| (s.id, s.state))
        .collect();
    let now = OffsetDateTime::now_utc();
    for row in live {
        let state = states.get(&row.session_id).map(String::as_str);
        let (reason, message) = match verdict(&row, state, now) {
            Verdict::Keep => continue,
            Verdict::Phase(phase) => {
                let (id, from) = (row.id, row.phase);
                db::update(&app.pool, id, |r| {
                    // only the start this pass judged: a boot that came meanwhile moved it on itself
                    if r.state != SandboxState::Starting || r.machine.is_some() || r.phase != from {
                        return false;
                    }
                    r.phase = Some(phase);
                    // the start's clock (resume_overdue) runs from here: the wait for an approval is the person's
                    r.last_active_at = OffsetDateTime::now_utc();
                    true
                })
                .await?;
                tracing::info!(sandbox = %row.id, session = %row.session_id, phase = phase.as_str(), "start moved on");
                continue;
            }
            Verdict::End(reason, message) => (reason, message),
        };
        let machine = row.machine.clone();
        db::update(&app.pool, row.id, |r| {
            // only the boot this pass judged: a newer one of the same row is left alone
            if r.state == SandboxState::Gone || r.machine != machine {
                return false;
            }
            db::end(r, reason, message);
            true
        })
        .await?;
        tracing::info!(sandbox = %row.id, session = %row.session_id, reason = reason.as_str(), "sandbox ended");
    }
    for (session, told) in chats {
        let want = chat_of(states.get(&session.session_id).map(String::as_str));
        if want.name() == told {
            continue;
        }
        let magpie = app.magpie.service(None);
        if told == Chat::Ended.name() {
            match magpie
                .hermit_restore_conversation(session.conversation_id)
                .await
            {
                Ok(_) => {
                    tracing::warn!(session = %session.session_id, "the session is alive: its conversation is back in the chat");
                }
                // purged from magpie meanwhile: there is no conversation to bring back
                Err(e) if e.code() == Some(&magpie_api::errors::ErrorCode::NotFound) => continue,
                Err(e) => {
                    tracing::warn!(session = %session.session_id, error = %e, "the conversation was not restored; next pass");
                    continue;
                }
            }
        }
        let done = match want {
            Chat::Open => {
                magpie
                    .hermit_close_conversation(session.conversation_id, None)
                    .await
            }
            Chat::Closed(why) => {
                magpie
                    .hermit_close_conversation(session.conversation_id, Some(why))
                    .await
            }
            Chat::Ended => {
                magpie
                    .hermit_delete_conversation(session.conversation_id)
                    .await
            }
        };
        match done {
            // the state the session pauses with: its next schedule, for the router
            Ok(c) => {
                app.report_chat_schedule(&session.session_id, c.next_schedule_at)
                    .await
            }
            // the conversation is already gone from magpie: nothing left to keep in step
            Err(e) if e.code() == Some(&magpie_api::errors::ErrorCode::NotFound) => {}
            Err(e) => {
                tracing::warn!(session = %session.session_id, error = %e, "the chat was not told; next pass");
                continue;
            }
        }
        db::set_chat(&app.pool, &session.session_id, want.name()).await?;
        tracing::info!(session = %session.session_id, chat = want.name(), "the session's conversation");
    }
    Ok(())
}

/// What a pass does with one live sandbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing.
    Keep,
    /// Its start (no machine has booted it yet) is now in this phase.
    Phase(SandboxPhase),
    /// It ends.
    End(ReleaseReason, &'static str),
}

/// The sentence a sandbox ends with when the session proposed for its conversation is gone before it was approved.
pub const NOT_APPROVED: &str = "the new session was not approved";

/// What becomes of a live sandbox whose session the router lists in `state` (`None`: not in its list) at `now`.
///
/// A booted one lives while its session is up. One no machine has booted yet is a start: of a new session waiting
/// for its approval (phase `approval` while the router says so, on to `host` once approved, ended when the session is
/// gone), or of a resume; either ends `start_timed_out` when no machine boots within [`RESUME_DEADLINE`] of the start
/// leaving the wait for its approval.
#[must_use]
pub fn verdict(row: &SandboxRow, state: Option<&str>, now: OffsetDateTime) -> Verdict {
    if row.machine.is_some() {
        return match state {
            // a frozen session's machine is kept, supervisor and all: its sandbox lives on, frozen with it
            Some(s) if is_up(s) || s == FROZEN => Verdict::Keep,
            Some("paused" | "pausing") => {
                Verdict::End(ReleaseReason::Idle, "Jarvis 4 paused the session")
            }
            _ => Verdict::End(ReleaseReason::Vanished, "the session is no longer running"),
        };
    }
    let approval = row.phase == Some(SandboxPhase::Approval);
    match state {
        Some(APPROVAL) if approval => Verdict::Keep,
        Some(APPROVAL) => Verdict::Phase(SandboxPhase::Approval),
        None if approval => Verdict::End(ReleaseReason::NotAllowed, NOT_APPROVED),
        None => Verdict::End(ReleaseReason::Vanished, "the session is no longer running"),
        Some(_) if approval => Verdict::Phase(SandboxPhase::Host),
        Some(_) if overdue(row, now) => Verdict::End(
            ReleaseReason::StartTimedOut,
            "the session's machine did not boot after the resume",
        ),
        // a resume in flight: its session is still paused for a moment
        Some(_) => Verdict::Keep,
    }
}

/// A start no machine booted within [`RESUME_DEADLINE`] of its clock (`last_active_at`: when it began, or left the
/// wait for its approval).
fn overdue(row: &SandboxRow, now: OffsetDateTime) -> bool {
    row.state == SandboxState::Starting
        && now - row.last_active_at > time::Duration::try_from(RESUME_DEADLINE).unwrap_or_default()
}

/// The phase a new session's start begins in, from the router's state of it.
#[must_use]
pub fn start_phase(state: &str) -> SandboxPhase {
    if state == APPROVAL {
        SandboxPhase::Approval
    } else {
        SandboxPhase::Host
    }
}

/// What the chat shows of a session's conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chat {
    /// Its person can message it.
    Open,
    /// The message box shows this sentence and takes nothing.
    Closed(&'static str),
    /// Gone from the chat.
    Ended,
}

impl Chat {
    /// As `sessions.chat` records it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed(_) => "closed",
            Self::Ended => "ended",
        }
    }
}

/// The sentence in the message box of a paused session's conversation.
pub const PAUSED: &str = "This session is paused. Resume it in the Jarvis 4 app to go on.";
/// The same for a session that is not running for another reason (a failed start or pause).
pub const NOT_RUNNING: &str = "This session is not running. Start it in the Jarvis 4 app to go on.";

/// What a session in the router's `state` (`None`: not in its list any more) shows in the chat.
#[must_use]
pub fn chat_of(state: Option<&str>) -> Chat {
    match state {
        // a new session waiting for its approval: its conversation's first message waits with it, and more may join; a
        // frozen one takes messages too, and the first thaws it (hermit's get-or-start resumes it through the router)
        Some(s) if is_up(s) || s == APPROVAL || s == FROZEN => Chat::Open,
        Some("paused" | "pausing") => Chat::Closed(PAUSED),
        None | Some("destroyed" | "destroying") => Chat::Ended,
        Some(_) => Chat::Closed(NOT_RUNNING),
    }
}

#[cfg(test)]
mod tests {
    use hermit_api::sandbox::{ReleaseReason, SandboxPhase, SandboxState};
    use time::OffsetDateTime;

    use super::{Chat, NOT_APPROVED, NOT_RUNNING, PAUSED, Verdict, chat_of, start_phase, verdict};
    use crate::db::SandboxRow;

    fn row(machine: Option<&str>, phase: SandboxPhase, started: OffsetDateTime) -> SandboxRow {
        SandboxRow {
            id: uuid::Uuid::nil(),
            conversation_id: uuid::Uuid::nil(),
            session_id: "s1".into(),
            person: "owner@example.com".into(),
            machine: machine.map(str::to_owned),
            state: SandboxState::Starting,
            phase: Some(phase),
            version: 1,
            created_at: started,
            ready_at: None,
            last_active_at: started,
            expires_at: started + time::Duration::days(30),
            released_at: None,
            busy: false,
            image: None,
            supervisor: None,
            release: None,
            pending: None,
            boot: serde_json::Value::Null,
        }
    }

    /// A conversation begun in the chat (Deyao, 2026-11-18): its start waits while the session awaits Deyao's
    /// approval, however long that takes, goes on once approved, and ends when the session is denied.
    #[test]
    fn a_new_conversations_start_follows_its_sessions_approval() {
        let now = OffsetDateTime::now_utc();
        let long_ago = now - time::Duration::hours(3);
        assert_eq!(start_phase("approval"), SandboxPhase::Approval);
        assert_eq!(start_phase("starting"), SandboxPhase::Host);
        let waiting = row(None, SandboxPhase::Approval, long_ago);
        assert_eq!(
            verdict(&waiting, Some("approval"), now),
            Verdict::Keep,
            "an approval spends no start time"
        );
        assert_eq!(
            verdict(&waiting, Some("starting"), now),
            Verdict::Phase(SandboxPhase::Host)
        );
        assert_eq!(
            verdict(&waiting, None, now),
            Verdict::End(ReleaseReason::NotAllowed, NOT_APPROVED),
            "denied (or lapsed): the session is gone from the list"
        );
        // a start the router lists as waiting for approval though it began elsewhere is moved there
        assert_eq!(
            verdict(&row(None, SandboxPhase::Host, now), Some("approval"), now),
            Verdict::Phase(SandboxPhase::Approval)
        );
        // approved: the machine has its ten minutes from then (the clock is reset as the phase moves)
        let approved = row(None, SandboxPhase::Host, now - time::Duration::minutes(9));
        assert_eq!(verdict(&approved, Some("starting"), now), Verdict::Keep);
        let late = row(None, SandboxPhase::Host, now - time::Duration::minutes(11));
        assert!(matches!(
            verdict(&late, Some("starting"), now),
            Verdict::End(ReleaseReason::StartTimedOut, _)
        ));
    }

    #[test]
    fn a_booted_sandbox_lives_while_its_session_is_up() {
        let now = OffsetDateTime::now_utc();
        let booted = row(Some("m1"), SandboxPhase::Supervisor, now);
        assert_eq!(verdict(&booted, Some("started"), now), Verdict::Keep);
        // a session in the time machine keeps its sandbox: it is frozen with the machine
        assert_eq!(verdict(&booted, Some("frozen"), now), Verdict::Keep);
        assert!(matches!(
            verdict(&booted, Some("paused"), now),
            Verdict::End(ReleaseReason::Idle, _)
        ));
        assert!(matches!(
            verdict(&booted, None, now),
            Verdict::End(ReleaseReason::Vanished, _)
        ));
        // a resume in flight is left alone, a destroyed session's ends
        let resume = row(None, SandboxPhase::Host, now);
        assert_eq!(verdict(&resume, Some("resuming"), now), Verdict::Keep);
        assert!(matches!(
            verdict(&resume, None, now),
            Verdict::End(ReleaseReason::Vanished, _)
        ));
    }

    #[test]
    fn a_sessions_state_decides_its_conversation() {
        for up in ["started", "starting", "resuming"] {
            assert_eq!(chat_of(Some(up)), Chat::Open);
        }
        assert_eq!(chat_of(Some("paused")), Chat::Closed(PAUSED));
        assert_eq!(chat_of(Some("pausing")), Chat::Closed(PAUSED));
        assert_eq!(chat_of(Some("failed")), Chat::Closed(NOT_RUNNING));
        // a new session waiting for its approval: its conversation takes more messages behind the first
        assert_eq!(chat_of(Some("approval")), Chat::Open);
        // one in the time machine takes messages: the first thaws it
        assert_eq!(chat_of(Some("frozen")), Chat::Open);
        assert_eq!(chat_of(Some("destroyed")), Chat::Ended);
        assert_eq!(chat_of(None), Chat::Ended);
    }
}
