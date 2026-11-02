//! The keeper's exits as an async stream: one task polls `EXITS` every 100 ms and everyone who waits for a pid
//! (exec runs, dockerd, the Claude Code driver) is woken from it.
//!
//! The keeper keeps the last 64 exits of its own children. When more than that land between two polls, the answer
//! starts after `after + 1`: a gap. The watcher then asks `STATUS` and reads the exits again, and every pid someone
//! waits for that is neither among the keeper's live children nor in the exits is reported gone, with an unknown
//! code (a [`LOST`] record), so no waiter waits for ever.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;

use super::{Client, ExitRecord};

/// How many exits are kept for late waiters.
const KEPT: usize = 256;
const POLL: Duration = Duration::from_millis(100);
/// The name of an exit the keeper's ring no longer had (code and signal unknown).
pub const LOST: &str = "lost";

#[derive(Debug, Default)]
struct Seen {
    after: u64,
    list: VecDeque<ExitRecord>,
    /// pid -> how many wait for it.
    waiting: HashMap<i32, usize>,
}

impl Seen {
    fn push(&mut self, e: ExitRecord) {
        self.list.push_back(e);
        if self.list.len() > KEPT {
            self.list.pop_front();
        }
    }
    fn find(&self, pid: i32) -> Option<&ExitRecord> {
        self.list.iter().rev().find(|e| e.pid == pid)
    }
}

/// The exits seen so far.
#[derive(Debug)]
pub struct Exits {
    keeper: Client,
    seen: Mutex<Seen>,
    changed: watch::Sender<u64>,
}

/// Unregisters a waiter when its wait ends (or is dropped).
struct Waiting<'a> {
    exits: &'a Exits,
    pid: i32,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let mut g = self.exits.lock();
        if let Some(n) = g.waiting.get_mut(&self.pid) {
            *n -= 1;
            if *n == 0 {
                g.waiting.remove(&self.pid);
            }
        }
    }
}

impl Exits {
    /// Starts with the keeper's counter as it is now (`after`): exits before it are not reported again.
    #[must_use]
    pub fn new(keeper: Client, after: u64) -> Arc<Self> {
        Arc::new(Self {
            keeper,
            seen: Mutex::new(Seen {
                after,
                ..Seen::default()
            }),
            changed: watch::channel(after).0,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn exits_after(&self, after: u64) -> Option<Vec<ExitRecord>> {
        let k = self.keeper.clone();
        tokio::task::spawn_blocking(move || k.exits(after)).await.ok()?.ok()
    }

    /// One poll: new exits, and on a gap the pids it may have hidden.
    pub async fn poll_once(&self) {
        let after = self.lock().after;
        let Some(mut fresh) = self.exits_after(after).await else {
            return;
        };
        let gap = fresh.first().is_some_and(|e| e.n > after + 1);
        let mut status = None;
        if gap {
            tracing::warn!(
                after,
                first = fresh.first().map_or(0, |e| e.n),
                "keeper exits: a gap, checking STATUS"
            );
            let k = self.keeper.clone();
            status = tokio::task::spawn_blocking(move || k.status())
                .await
                .ok()
                .and_then(Result::ok);
            // exits that happened before STATUS answered are in the ring now
            if status.is_some()
                && let Some(again) = self.exits_after(after).await
            {
                fresh = again;
            }
        }
        let mut g = self.lock();
        let Some(last) = fresh.last().map(|e| e.n) else {
            return;
        };
        g.after = last;
        for e in fresh {
            g.push(e);
        }
        if let Some(st) = status {
            let gone: Vec<i32> = g
                .waiting
                .keys()
                .copied()
                .filter(|pid| g.find(*pid).is_none() && !st.children.iter().any(|(p, _)| p == pid))
                .collect();
            for pid in gone {
                tracing::warn!(pid, "keeper exits: the exit fell out of the ring");
                g.push(ExitRecord {
                    n: 0,
                    pid,
                    name: LOST.into(),
                    code: None,
                    signal: None,
                    at_ms: 0,
                });
            }
        }
        drop(g);
        self.changed.send_replace(last);
    }

    /// The polling task.
    pub async fn run(self: Arc<Self>) {
        loop {
            self.poll_once().await;
            tokio::time::sleep(POLL).await;
        }
    }

    /// The exit of `pid` if it has been seen.
    #[must_use]
    pub fn find(&self, pid: i32) -> Option<ExitRecord> {
        self.lock().find(pid).cloned()
    }

    /// Waits for `pid`'s exit (a [`LOST`] record when the keeper's ring overflowed before it was read).
    pub async fn wait(&self, pid: i32) -> ExitRecord {
        let mut rx = self.changed.subscribe();
        *self.lock().waiting.entry(pid).or_default() += 1;
        let _registered = Waiting { exits: self, pid };
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::time::Instant;

    use super::*;
    use crate::keeper::testing::Running;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_burst_that_overflows_the_ring_does_not_lose_a_waited_pid() {
        let k = Running::start(None);
        let victim = k.client.spawn(&k.spec("claude", &["/bin/sleep", "100"])).unwrap();
        let exits = Exits::new(k.client.clone(), 0);
        let w = {
            let e = exits.clone();
            tokio::spawn(async move { e.wait(victim).await })
        };
        // the waiter is registered; the victim dies and 70 tracked exits follow before the next poll
        tokio::time::sleep(Duration::from_millis(50)).await;
        k.client.signal(victim, libc::SIGKILL).unwrap();
        k.wait_exit(victim);
        let mut last = 0;
        for _ in 0..70 {
            last = k.client.spawn(&k.spec("exec", &["/bin/true"])).unwrap();
        }
        k.wait_exit(last);
        let deadline = Instant::now() + Duration::from_secs(10);
        while k.client.exits(0).unwrap().iter().any(|e| e.pid == victim) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        exits.poll_once().await;
        let e = tokio::time::timeout(Duration::from_secs(2), w).await.unwrap().unwrap();
        assert_eq!((e.pid, e.name.as_str(), e.code), (victim, LOST, None));
        // nothing else is reported lost: only waited pids are
        assert!(exits.find(last).is_some_and(|e| e.name == "exec"));
    }
}
