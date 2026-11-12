//! The router is the truth about sessions: every few seconds hermit-jarvis lists the aviary sessions and ends each
//! live sandbox whose session is no longer up (Jarvis 4 paused it after its idle hour: `idle`; destroyed, failed or
//! gone from the list: `vanished`), and each resume whose machine never booted (`start_timed_out`). Every replica runs
//! it; each change is a row-locked update that does nothing once the row is gone, so two replicas agree.
//!
//! The same pass keeps each session's conversation in the chat in step (Deyao, 2026-11-12: the lifecycle is managed
//! in Jarvis 4, not in the chat): a session that is not up has its conversation closed to its person's messages, with
//! a sentence saying where to resume it; one that is up again is opened; a destroyed one's conversation leaves the
//! chat. magpie's calls are idempotent, so two replicas doing the same one agree too.

use std::collections::HashMap;
use std::time::Duration;

use hermit_api::sandbox::{ReleaseReason, SandboxState};

use crate::app::App;
use crate::db;
use crate::jarvis::is_up;

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
    for row in live {
        let state = states.get(&row.session_id).map(String::as_str);
        let (reason, message) = if crate::hermit::resume_overdue(&row) {
            (
                ReleaseReason::StartTimedOut,
                "the session's machine did not boot after the resume",
            )
        } else if row.machine.is_none() {
            // a resume in flight: its session is still paused for a moment
            continue;
        } else {
            match state {
                Some(s) if is_up(s) => continue,
                Some("paused" | "pausing") => (ReleaseReason::Idle, "Jarvis 4 paused the session"),
                _ => (ReleaseReason::Vanished, "the session is no longer running"),
            }
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
            Ok(_) => {}
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
        Some(s) if is_up(s) => Chat::Open,
        Some("paused" | "pausing") => Chat::Closed(PAUSED),
        None | Some("destroyed" | "destroying") => Chat::Ended,
        Some(_) => Chat::Closed(NOT_RUNNING),
    }
}

#[cfg(test)]
mod tests {
    use super::{Chat, NOT_RUNNING, PAUSED, chat_of};

    #[test]
    fn a_sessions_state_decides_its_conversation() {
        for up in ["started", "starting", "resuming"] {
            assert_eq!(chat_of(Some(up)), Chat::Open);
        }
        assert_eq!(chat_of(Some("paused")), Chat::Closed(PAUSED));
        assert_eq!(chat_of(Some("pausing")), Chat::Closed(PAUSED));
        assert_eq!(chat_of(Some("failed")), Chat::Closed(NOT_RUNNING));
        assert_eq!(chat_of(Some("destroyed")), Chat::Ended);
        assert_eq!(chat_of(None), Chat::Ended);
    }
}
