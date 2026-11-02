//! Drain (platform 3.3), the same in every service.
//!
//! Kubernetes runs `preStop: sleep <N>` first, so the endpoint has left the Service before SIGTERM arrives. On the
//! first SIGTERM (or SIGINT) [`Drain::run`] marks the process draining (`/readyz` 503, `<svc>_draining` 1, every
//! [`Draining`] handle wakes so long-polls and streams can give their "reconnect" answer), then runs the service's
//! hooks in the order they were added, each bounded by its own deadline. A hook that overruns is abandoned (its
//! future dropped) and the next one runs. A second signal ends the drain at once: the work is designed to be
//! resumable elsewhere. [`exit`] then ends the process: 0 with log `drained`, or [`EXIT_INTERRUPTED`].

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use crate::ops::Readiness;

/// Exit code when a second signal cut the drain short (128 + SIGTERM).
pub const EXIT_INTERRUPTED: i32 = 143;

/// A termination signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// SIGTERM (Kubernetes, the keeper).
    Term,
    /// SIGINT (a terminal).
    Int,
}

/// The termination signals a drain listens to: the process's own ([`Signals::os`]) or a test channel
/// ([`Signals::channel`]).
#[derive(Debug)]
pub struct Signals {
    rx: mpsc::UnboundedReceiver<Signal>,
}

impl Signals {
    /// SIGTERM and SIGINT of this process. Installs the handlers now, so a signal arriving any time after this call
    /// is seen.
    ///
    /// # Errors
    /// When a signal handler cannot be installed.
    pub fn os() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        let (tx, rx) = mpsc::unbounded_channel();
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        tokio::spawn(async move {
            loop {
                let s = tokio::select! {
                    Some(()) = term.recv() => Signal::Term,
                    Some(()) = int.recv() => Signal::Int,
                    else => break,
                };
                if tx.send(s).is_err() {
                    break;
                }
            }
        });
        Ok(Self { rx })
    }

    /// A channel standing in for the process's signals.
    #[must_use]
    pub fn channel() -> (mpsc::UnboundedSender<Signal>, Self) {
        let (tx, rx) = mpsc::unbounded_channel();
        (tx, Self { rx })
    }

    /// The next signal; `None` when the source is gone (then no signal will come).
    pub async fn recv(&mut self) -> Option<Signal> {
        self.rx.recv().await
    }
}

/// A handle that tells long-running work the process is draining.
#[derive(Debug, Clone)]
pub struct Draining {
    rx: watch::Receiver<bool>,
}

impl Draining {
    /// Whether draining has started.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves once draining has started (at once if it already has).
    pub async fn wait(&mut self) {
        // an error means the drain is gone, which only happens as the process ends: treat it as draining
        let _ = self.rx.wait_for(|d| *d).await;
    }
}

/// How a drain ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every hook ran (or reached its deadline).
    Drained,
    /// A second signal arrived during the hooks.
    Interrupted,
}

type HookFn = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

struct Hook {
    name: String,
    deadline: Duration,
    run: HookFn,
}

/// The service's drain: its readiness and its ordered hooks.
pub struct Drain {
    readiness: Readiness,
    hooks: Vec<Hook>,
    tx: watch::Sender<bool>,
}

impl fmt::Debug for Drain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Drain")
            .field(
                "hooks",
                &self.hooks.iter().map(|h| (&h.name, h.deadline)).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Drain {
    /// A drain with no hooks that will mark `readiness` draining.
    #[must_use]
    pub fn new(readiness: Readiness) -> Self {
        Self {
            readiness,
            hooks: Vec::new(),
            tx: watch::channel(false).0,
        }
    }

    /// A handle for work that must notice the drain (long-polls, streams, loops).
    #[must_use]
    pub fn draining(&self) -> Draining {
        Draining {
            rx: self.tx.subscribe(),
        }
    }

    /// Adds a hook, run after the ones added before it and cut off after `deadline`.
    pub fn hook<F, Fut>(&mut self, name: &str, deadline: Duration, f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.hooks.push(Hook {
            name: name.to_owned(),
            deadline,
            run: Box::new(move || Box::pin(f())),
        });
    }

    /// Waits for the first signal, then drains; returns early with [`Outcome::Interrupted`] on a second signal.
    /// If the signal source ends before any signal, drains at once.
    pub async fn run(self, signals: &mut Signals) -> Outcome {
        let first = signals.recv().await;
        tracing::info!(signal = ?first, "draining");
        self.drain(signals).await
    }

    /// Drains now (without waiting for a first signal); a signal during the hooks interrupts it.
    pub async fn drain(self, signals: &mut Signals) -> Outcome {
        self.readiness.set_draining();
        let _ = self.tx.send(true);
        let mut source_open = true;
        for hook in self.hooks {
            let started = Instant::now();
            let fut = tokio::time::timeout(hook.deadline, (hook.run)());
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    done = &mut fut => {
                        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        if done.is_ok() {
                            tracing::info!(hook = %hook.name, ms, "drain hook done");
                        } else {
                            tracing::warn!(hook = %hook.name, ms, "drain hook timed out");
                        }
                        break;
                    }
                    s = signals.recv(), if source_open => {
                        if s.is_some() {
                            tracing::warn!(hook = %hook.name, "drain interrupted");
                            return Outcome::Interrupted;
                        }
                        source_open = false;
                    }
                }
            }
        }
        Outcome::Drained
    }
}

/// Ends the process after a drain: exit 0 with log `drained`, or [`EXIT_INTERRUPTED`] after a second signal.
pub fn exit(outcome: Outcome) -> ! {
    match outcome {
        Outcome::Drained => {
            tracing::info!("drained");
            std::process::exit(0)
        }
        Outcome::Interrupted => {
            tracing::warn!("drain cut short");
            std::process::exit(EXIT_INTERRUPTED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn recorder() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    #[tokio::test]
    async fn hooks_run_in_order_after_draining_is_set() {
        let readiness = Readiness::detached();
        readiness.started();
        let mut drain = Drain::new(readiness.clone());
        let seen = recorder();
        for name in ["close listeners", "end long-polls", "hand back leases", "wait for work"] {
            let seen = seen.clone();
            let r = readiness.clone();
            drain.hook(name, Duration::from_secs(5), move || async move {
                assert!(r.is_draining(), "readiness is draining before the first hook");
                tokio::task::yield_now().await;
                seen.lock().unwrap().push(name.to_owned());
            });
        }
        let mut draining = drain.draining();
        assert!(!draining.is_draining());
        let (tx, mut signals) = Signals::channel();
        let task = tokio::spawn(async move { drain.run(&mut signals).await });
        tx.send(Signal::Term).unwrap();
        draining.wait().await;
        assert_eq!(task.await.unwrap(), Outcome::Drained);
        assert_eq!(
            *seen.lock().unwrap(),
            ["close listeners", "end long-polls", "hand back leases", "wait for work"]
        );
        assert!(draining.is_draining());
        assert_eq!(readiness.why(), ["draining"]);
    }

    #[tokio::test]
    async fn an_overrunning_hook_is_cut_at_its_deadline_and_the_next_runs() {
        let mut drain = Drain::new(Readiness::detached());
        let seen = recorder();
        let s1 = seen.clone();
        drain.hook("slow", Duration::from_millis(100), move || async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            s1.lock().unwrap().push("slow finished".into());
        });
        let s2 = seen.clone();
        drain.hook("next", Duration::from_secs(5), move || async move {
            s2.lock().unwrap().push("next".into());
        });
        let (_tx, mut signals) = Signals::channel();
        let started = Instant::now();
        assert_eq!(drain.drain(&mut signals).await, Outcome::Drained);
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(100) && took < Duration::from_secs(5),
            "{took:?}"
        );
        assert_eq!(*seen.lock().unwrap(), ["next"]);
    }

    #[tokio::test]
    async fn a_second_signal_ends_the_drain_at_once() {
        let mut drain = Drain::new(Readiness::detached());
        let seen = recorder();
        let s1 = seen.clone();
        drain.hook("long", Duration::from_secs(60), move || async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            s1.lock().unwrap().push("long".into());
        });
        let s2 = seen.clone();
        drain.hook("after", Duration::from_secs(60), move || async move {
            s2.lock().unwrap().push("after".into());
        });
        let (tx, mut signals) = Signals::channel();
        tx.send(Signal::Term).unwrap();
        let task = tokio::spawn(async move { drain.run(&mut signals).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        tx.send(Signal::Term).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome, Outcome::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(seen.lock().unwrap().is_empty(), "no later hook runs");
    }

    #[tokio::test]
    async fn a_closed_signal_source_does_not_interrupt() {
        let mut drain = Drain::new(Readiness::detached());
        drain.hook("short", Duration::from_secs(5), || async {
            tokio::time::sleep(Duration::from_millis(20)).await;
        });
        let (tx, mut signals) = Signals::channel();
        drop(tx);
        assert_eq!(drain.run(&mut signals).await, Outcome::Drained);
    }

    /// The real process signals: SIGTERM to this test process reaches the drain, and a second SIGTERM interrupts it.
    #[tokio::test]
    async fn os_sigterm_starts_the_drain_and_a_second_interrupts_it() {
        let readiness = Readiness::detached();
        let mut drain = Drain::new(readiness.clone());
        drain.hook("long", Duration::from_secs(60), || async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let mut draining = drain.draining();
        let mut signals = Signals::os().unwrap();
        let task = tokio::spawn(async move { drain.run(&mut signals).await });
        let me = rustix::process::getpid();
        rustix::process::kill_process(me, rustix::process::Signal::TERM).unwrap();
        tokio::time::timeout(Duration::from_secs(5), draining.wait())
            .await
            .unwrap();
        assert!(readiness.is_draining());
        rustix::process::kill_process(me, rustix::process::Signal::TERM).unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome, Outcome::Interrupted);
    }
}
