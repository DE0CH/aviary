//! The router is the truth about sessions: every few seconds hermit-jarvis lists the aviary sessions and ends each
//! live sandbox whose session is no longer up (Jarvis 4 paused it after its idle hour: `idle`; destroyed, failed or
//! gone from the list: `vanished`), and each resume whose machine never booted (`start_timed_out`). Every replica runs
//! it; each change is a row-locked update that does nothing once the row is gone, so two replicas agree.

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
    if live.is_empty() {
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
    Ok(())
}
