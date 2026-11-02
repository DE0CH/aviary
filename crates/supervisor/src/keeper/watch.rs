//! The keeper's exits as an async stream: one task polls `EXITS` every 100 ms and everyone who waits for a pid
//! (exec runs, dockerd, the Claude Code driver) is woken from it.
//!
//! The keeper keeps the last 64 exits of its own children. When more than that land between two polls, the answer
//! starts after `after + 1`: a gap. The watcher then asks `STATUS` and reads the exits again, and every pid someone
//! waits for that is neither among the keeper's live children nor in the exits is reported gone, with an unknown
//! code (a [`LOST`] record), so no waiter waits for ever. Until that `STATUS` has answered, the watcher does not move
//! past the gap: it reads the same exits again on the next poll.
//!
//! A wait names the pid and the keeper's exit counter when the process was forked (`SPAWN`'s `since`, or the counter
//! the watcher started from for a process found alive at start): only an exit recorded after that counter matches,
//! so a pid the kernel reused never picks up an older process's exit.

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
    /// (pid, since) -> how many wait for it.
    waiting: HashMap<(i32, u64), usize>,
}

impl Seen {
    fn push(&mut self, e: ExitRecord) {
        self.list.push_back(e);
        if self.list.len() > KEPT {
            self.list.pop_front();
        }
    }
    /// The first exit of `pid` recorded after counter `since`.
    fn find(&self, pid: i32, since: u64) -> Option<&ExitRecord> {
        self.list
            .iter()
            .filter(|e| e.pid == pid && e.n > since)
            .min_by_key(|e| e.n)
    }
}

/// The exits seen so far.
#[derive(Debug)]
pub struct Exits {
    /// The counter the watcher started from: the `since` of a process found alive at start.
    origin: u64,
    keeper: Client,
    seen: Mutex<Seen>,
    changed: watch::Sender<u64>,
    /// Tests: how many of the next `STATUS` calls fail.
    #[cfg(test)]
    status_failures: std::sync::atomic::AtomicU32,
}

/// Unregisters a waiter when its wait ends (or is dropped).
struct Waiting<'a> {
    exits: &'a Exits,
    key: (i32, u64),
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let mut g = self.exits.lock();
        if let Some(n) = g.waiting.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                g.waiting.remove(&self.key);
            }
        }
    }
}

impl Exits {
    /// Starts with the keeper's counter as it is now (`after`): exits before it are not reported again.
    #[must_use]
    pub fn new(keeper: Client, after: u64) -> Arc<Self> {
        Arc::new(Self {
            origin: after,
            keeper,
            seen: Mutex::new(Seen {
                after,
                ..Seen::default()
            }),
            changed: watch::channel(after).0,
            #[cfg(test)]
            status_failures: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// The keeper's exit counter this watcher started from: wait with it for a process that was already running
    /// then (a re-attached generation, a stray).
    #[must_use]
    pub fn origin(&self) -> u64 {
        self.origin
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn exits_after(&self, after: u64) -> Option<Vec<ExitRecord>> {
        let k = self.keeper.clone();
        tokio::task::spawn_blocking(move || k.exits(after)).await.ok()?.ok()
    }

    #[cfg(test)]
    fn take_status_failure(&self) -> bool {
        use std::sync::atomic::Ordering;
        let n = self.status_failures.load(Ordering::SeqCst);
        n > 0
            && self
                .status_failures
                .compare_exchange(n, n - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
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
            #[cfg(test)]
            if self.take_status_failure() {
                status = None;
            }
            if status.is_none() {
                // without STATUS the hidden exits cannot be told from live children: stay before the gap and look
                // again on the next poll
                tracing::warn!(after, "keeper exits: STATUS failed after a gap, retrying");
                return;
            }
            // exits that happened before STATUS answered are in the ring now
            if let Some(again) = self.exits_after(after).await {
                fresh = again;
            }
        }
        // the counter just before the first exit this answer has: a lost exit happened at or before it
        let lost_n = fresh.first().map_or(after, |e| e.n - 1);
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
                .filter(|&(pid, since)| {
                    since < lost_n && g.find(pid, since).is_none() && !st.children.iter().any(|(p, _)| *p == pid)
                })
                .map(|(pid, _)| pid)
                .collect();
            for pid in gone {
                tracing::warn!(pid, "keeper exits: the exit fell out of the ring");
                g.push(ExitRecord {
                    n: lost_n,
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

    /// The first exit of `pid` recorded after counter `since`, if it has been seen.
    #[must_use]
    pub fn find(&self, pid: i32, since: u64) -> Option<ExitRecord> {
        self.lock().find(pid, since).cloned()
    }

    /// Waits for the exit of `pid` forked at counter `since` ([`super::Spawned::since`], or [`Exits::origin`] for a
    /// process already running when the watcher started); a [`LOST`] record when the keeper's ring overflowed
    /// before it was read.
    pub async fn wait(&self, pid: i32, since: u64) -> ExitRecord {
        let mut rx = self.changed.subscribe();
        let key = (pid, since);
        *self.lock().waiting.entry(key).or_default() += 1;
        let _registered = Waiting { exits: self, key };
        loop {
            if let Some(e) = self.find(pid, since) {
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
            tokio::spawn(async move { e.wait(victim, 0).await })
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
        // the STATUS after the gap fails once: nothing is decided, the next poll looks again
        exits.status_failures.store(1, std::sync::atomic::Ordering::SeqCst);
        exits.poll_once().await;
        assert_eq!(exits.lock().after, 0, "not past the gap without STATUS");
        assert!(!w.is_finished());
        exits.poll_once().await;
        let e = tokio::time::timeout(Duration::from_secs(2), w).await.unwrap().unwrap();
        assert_eq!((e.pid, e.name.as_str(), e.code), (victim, LOST, None));
        // nothing else is reported lost: only waited pids are
        assert!(exits.find(last, 0).is_some_and(|e| e.name == "exec"));
    }

    #[tokio::test]
    async fn a_reused_pid_never_matches_an_older_exit() {
        let k = Running::start(None);
        let exits = Exits::new(k.client.clone(), 0);
        let rec = |n, code| ExitRecord {
            n,
            pid: 4242,
            name: "exec".into(),
            code: Some(code),
            signal: None,
            at_ms: 0,
        };
        // an older process with that pid exited at counter 3
        exits.lock().push(rec(3, 1));
        exits.lock().after = 3;
        // the new one was forked at counter 5: its wait does not end on the old record
        let w = {
            let e = exits.clone();
            tokio::spawn(async move { e.wait(4242, 5).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!w.is_finished());
        assert_eq!(exits.find(4242, 2).map(|e| e.code), Some(Some(1)));
        exits.lock().push(rec(7, 0));
        exits.lock().push(rec(9, 2));
        exits.changed.send_replace(9);
        let e = tokio::time::timeout(Duration::from_secs(2), w).await.unwrap().unwrap();
        // the first exit after its fork, not the latest with that pid
        assert_eq!((e.n, e.code), (7, Some(0)));
    }
}
