//! The keeper's exits as an async stream: one task polls `EXITS` every 100 ms and everyone who waits for a pid
//! (exec runs, dockerd, the Claude Code driver) is woken from it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;

use super::{Client, ExitRecord};

/// How many exits are kept for late waiters.
const KEPT: usize = 256;
const POLL: Duration = Duration::from_millis(100);

/// The exits seen so far.
#[derive(Debug)]
pub struct Exits {
    keeper: Client,
    seen: Mutex<(u64, VecDeque<ExitRecord>)>,
    changed: watch::Sender<u64>,
}

impl Exits {
    /// Starts with the keeper's counter as it is now (`after`): exits before it are not reported again.
    #[must_use]
    pub fn new(keeper: Client, after: u64) -> Arc<Self> {
        Arc::new(Self {
            keeper,
            seen: Mutex::new((after, VecDeque::new())),
            changed: watch::channel(after).0,
        })
    }

    /// The polling task.
    pub async fn run(self: Arc<Self>) {
        loop {
            let after = self.seen.lock().unwrap_or_else(PoisonError::into_inner).0;
            let k = self.keeper.clone();
            if let Ok(Ok(list)) = tokio::task::spawn_blocking(move || k.exits(after)).await
                && let Some(last) = list.last().map(|e| e.n)
            {
                let mut g = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
                g.0 = last;
                for e in list {
                    g.1.push_back(e);
                    if g.1.len() > KEPT {
                        g.1.pop_front();
                    }
                }
                drop(g);
                self.changed.send_replace(last);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// The exit of `pid` if it has been seen.
    #[must_use]
    pub fn find(&self, pid: i32) -> Option<ExitRecord> {
        let g = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        g.1.iter().rev().find(|e| e.pid == pid).cloned()
    }

    /// Waits for `pid`'s exit.
    pub async fn wait(&self, pid: i32) -> ExitRecord {
        let mut rx = self.changed.subscribe();
        loop {
            if let Some(e) = self.find(pid) {
                return e;
            }
            if rx.changed().await.is_err() {
                // the sender lives in self: cannot happen; poll instead of spinning
                tokio::time::sleep(POLL).await;
            }
        }
    }
}
