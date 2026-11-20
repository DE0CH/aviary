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
//! A message to a paused session resumes it (Deyao, 2026-11-20): magpie's get-or-start makes a start, and this pass
//! asks the router to resume its session for as long as the router lists it paused (a resume refused while the pause
//! still ran is asked again once it ends). The router's resume with no prompt needs no approval; one it sends to
//! Deyao's Inbox (a session whose harness never came up on its image goes onto the newest) puts the start in phase
//! `approval`, and a denial ends the start rather than asking again.
//!
//! The same pass keeps each session's conversation in the chat in step (Deyao, 2026-11-12: the lifecycle is managed
//! in Jarvis 4, not in the chat): a session takes its person's messages for as long as it exists, paused or not; a
//! destroyed one's conversation leaves the chat. magpie's calls are idempotent, so two replicas doing the same one
//! agree too.
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
            Verdict::Resume => {
                app.resume(&row.session_id).await;
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
            // open (a conversation a pause closed before 2026-11-20 is opened by the first pass that sees it)
            Chat::Open => {
                magpie
                    .hermit_close_conversation(session.conversation_id, None)
                    .await
            }
            Chat::Ended => {
                magpie
                    .hermit_delete_conversation(session.conversation_id)
                    .await
            }
        };
        match done {
            // its next schedule, for the router
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
    /// Its start asks the router to resume its paused session.
    Resume,
    /// It ends.
    End(ReleaseReason, &'static str),
}

/// The sentence a start ends with when Deyao did not approve its session: a new one proposed for its conversation, or a
/// resume the router sent to his Inbox.
pub const NOT_APPROVED: &str = "the session's start was not approved";

/// What becomes of a live sandbox whose session the router lists in `state` (`None`: not in its list) at `now`.
///
/// A booted one lives while its session is up. One no machine has booted yet is a start: it waits in phase `approval`
/// while the router says the session waits for Deyao's approval (a new session, or a resume onto a new image), goes on
/// to `host` once approved and ends when he did not approve (the session gone, or back to paused or failed); a start
/// whose session is paused asks for its resume. Either ends `start_timed_out` when no machine boots within
/// [`RESUME_DEADLINE`] of the start leaving the wait for its approval.
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
        None | Some("paused" | "failed") if approval => {
            Verdict::End(ReleaseReason::NotAllowed, NOT_APPROVED)
        }
        None => Verdict::End(ReleaseReason::Vanished, "the session is no longer running"),
        Some(_) if approval => Verdict::Phase(SandboxPhase::Host),
        Some(_) if overdue(row, now) => Verdict::End(
            ReleaseReason::StartTimedOut,
            "the session's machine did not boot after the resume",
        ),
        Some("paused") => Verdict::Resume,
        // a resume in flight, or a pause still running (its resume follows once it is paused)
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
    /// Gone from the chat.
    Ended,
}

impl Chat {
    /// As `sessions.chat` records it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Ended => "ended",
        }
    }
}

/// What a session in the router's `state` (`None`: not in its list any more) shows in the chat: open for as long as
/// the session exists. A message to one that is not up brings it up (hermit's get-or-start: a paused or failed
/// session resumes, a frozen one thaws, a new one's first message waits for its approval).
#[must_use]
pub fn chat_of(state: Option<&str>) -> Chat {
    match state {
        None | Some("destroyed" | "destroying") => Chat::Ended,
        Some(_) => Chat::Open,
    }
}

#[cfg(test)]
mod tests {
    use hermit_api::sandbox::{ReleaseReason, SandboxPhase, SandboxState};
    use time::OffsetDateTime;

    use super::{Chat, NOT_APPROVED, Verdict, chat_of, start_phase, verdict};
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

    /// A session takes its person's messages for as long as it exists (Deyao, 2026-11-20): a message to a paused,
    /// failed or frozen session brings it up (hermit's get-or-start), so its conversation is never closed.
    #[test]
    fn a_session_takes_messages_until_it_is_destroyed() {
        for state in [
            "started", "starting", "resuming", "approval", "frozen", "pausing", "paused", "failed",
        ] {
            assert_eq!(chat_of(Some(state)), Chat::Open, "{state}");
        }
        assert_eq!(chat_of(Some("destroying")), Chat::Ended);
        assert_eq!(chat_of(Some("destroyed")), Chat::Ended);
        assert_eq!(chat_of(None), Chat::Ended);
    }

    /// A message to a paused session (Deyao, 2026-11-20): its start asks the router to resume the session for as long
    /// as the router lists it paused (a resume refused while the pause still ran, `pausing`, is asked again), and
    /// leaves a resume in flight alone; the start's ten minutes end it if no machine boots.
    #[test]
    fn a_start_resumes_its_paused_session() {
        let now = OffsetDateTime::now_utc();
        let start = row(None, SandboxPhase::Host, now);
        assert_eq!(verdict(&start, Some("paused"), now), Verdict::Resume);
        assert_eq!(
            verdict(&start, Some("pausing"), now),
            Verdict::Keep,
            "the pause ends first"
        );
        assert_eq!(verdict(&start, Some("resuming"), now), Verdict::Keep);
        let late = row(None, SandboxPhase::Host, now - time::Duration::minutes(11));
        assert!(matches!(
            verdict(&late, Some("paused"), now),
            Verdict::End(ReleaseReason::StartTimedOut, _)
        ));
    }

    /// A resume that needs Deyao's approval (the router's: a session whose harness never came up on its image goes
    /// onto the newest one) waits for it, and ends when he denies it (the router puts the session back to paused):
    /// it is never asked again, so no second Inbox item follows a denial.
    #[test]
    fn a_denied_resume_ends_its_start() {
        let now = OffsetDateTime::now_utc();
        let start = row(None, SandboxPhase::Host, now);
        assert_eq!(
            verdict(&start, Some("approval"), now),
            Verdict::Phase(SandboxPhase::Approval)
        );
        let waiting = row(None, SandboxPhase::Approval, now);
        assert_eq!(
            verdict(&waiting, Some("paused"), now),
            Verdict::End(ReleaseReason::NotAllowed, NOT_APPROVED)
        );
        // a new session's start again that was denied stays as a failed session
        assert_eq!(
            verdict(&waiting, Some("failed"), now),
            Verdict::End(ReleaseReason::NotAllowed, NOT_APPROVED)
        );
        // approved: on to the machine's boot, whatever the router calls its way up
        for up in ["starting", "resuming", "initialising", "started"] {
            assert_eq!(
                verdict(&waiting, Some(up), now),
                Verdict::Phase(SandboxPhase::Host),
                "{up}"
            );
        }
    }
}
