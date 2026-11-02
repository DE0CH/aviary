//! The Claude Code driver (supervisor 6-8, 10-14): launching Claude Code, the stream-json driver and run state
//! machine, magpie's commands, the reports, tasks and the hold, asks, uploads and the user message.
//!
//! [`run::Driver`] implements [`crate::lifecycle::Agent`]. What it needs of the `~/.claude` filesystem (WP-22) is
//! the [`Transcript`] seam: which turn a write belongs to, whether the main-transcript lines of a turn are stored
//! (8.2), and the FUSE facts of the heartbeat.

pub mod control;
pub mod launch;
pub mod managed;
pub mod prompt;
pub mod report;
pub mod run;
pub mod stream;
pub mod tasks;

use magpie_api::sandbox::CcFacts;
use uuid::Uuid;

use crate::lifecycle::BoxFuture;

/// How the wait for a turn's transcript lines ended (8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    /// magpie acknowledged a main-transcript write carrying every uuid.
    All,
    /// The one-stuck-operation rule of 9.5 item 4 fired: the turn is reported `failed` `cc.write_stuck`.
    Stuck,
}

/// The `~/.claude` filesystem as the driver sees it (implemented by WP-22's layer).
pub trait Transcript: Send + Sync + 'static {
    /// The turn the supervisor runs now: every write read from here on carries its `turnId` (8.1).
    fn set_turn(&self, turn: Option<Uuid>);
    /// Waits until magpie has acknowledged main-transcript writes holding every one of `uuids` (no time cap), or
    /// the stuck-operation rule fired.
    fn wait_stored(&self, uuids: Vec<String>) -> BoxFuture<'_, Stored>;
    /// Waits until the filesystem requests already in flight are answered (a generation that exited writes nothing
    /// more, but what it wrote may still be on its way to magpie).
    fn settle(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    /// The heartbeat's `cc` facts.
    fn facts(&self) -> CcFacts;
    /// The lease the filesystem holds now, when it has re-taken one (after a restore).
    fn lease(&self) -> Option<Uuid> {
        None
    }
    /// The main transcript's path, from the `running` report (the cache pins it).
    fn set_main_transcript(&self, _path: Option<String>) {}
    /// A heartbeat magpie answered: the stuck rule's evidence that magpie answers other calls (9.5 item 4).
    fn heartbeat_ok(&self) {}
}

/// The `~/.claude` layer of WP-22: the uuids of the lines magpie acknowledged come from its write path
/// ([`crate::fuse::seen`]); magpie's `409 transcript_pending` on `lastUuid` is the second check (8.2).
impl Transcript for crate::fuse::FuseFs {
    fn set_turn(&self, turn: Option<Uuid>) {
        if let Some(c) = self.core() {
            c.set_turn(turn);
        }
    }
    fn wait_stored(&self, uuids: Vec<String>) -> BoxFuture<'_, Stored> {
        Box::pin(async move {
            let Some(c) = self.core() else { return Stored::All };
            let before = c.stuck_count();
            let all = tokio::task::spawn_blocking(move || c.wait_uuids(&uuids, before))
                .await
                .unwrap_or(true);
            if all { Stored::All } else { Stored::Stuck }
        })
    }
    fn settle(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let Some(c) = self.core() else { return };
            loop {
                let c = c.clone();
                if tokio::task::spawn_blocking(move || c.settle(std::time::Duration::from_secs(1)))
                    .await
                    .unwrap_or(true)
                {
                    return;
                }
            }
        })
    }
    fn facts(&self) -> CcFacts {
        let (blocked, oldest) = self.remote().blocked();
        CcFacts {
            quota_exceeded: self.core().is_some_and(|c| c.quota_exceeded()),
            blocked_writes: blocked,
            oldest_blocked_ms: oldest.map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        }
    }
    fn lease(&self) -> Option<Uuid> {
        self.remote().lease().map(|l| l.id)
    }
    fn set_main_transcript(&self, path: Option<String>) {
        if let Some(c) = self.core() {
            c.set_main_transcript(path);
        }
    }
    fn heartbeat_ok(&self) {
        self.remote().record_ok();
    }
}

/// A transcript with nothing in flight: every line is stored at once (tests, and a build without the filesystem).
#[derive(Debug, Default)]
pub struct Stored0;

impl Transcript for Stored0 {
    fn set_turn(&self, _turn: Option<Uuid>) {}
    fn wait_stored(&self, _uuids: Vec<String>) -> BoxFuture<'_, Stored> {
        Box::pin(async { Stored::All })
    }
    fn facts(&self) -> CcFacts {
        CcFacts {
            quota_exceeded: false,
            blocked_writes: 0,
            oldest_blocked_ms: 0,
        }
    }
}
