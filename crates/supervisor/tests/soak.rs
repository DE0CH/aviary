//! The supervisor's RSS under load (supervisor 5.4, 20: 64 MiB budget, 40 MiB target; WP-21 acceptance). Fake
//! hermit and fake magpie run behind the reference gateway; the supervisor is a real child process of a keeper
//! (this test binary again, see `common::child_main`), with a recording agent that pre-spawns a stand-in Claude
//! Code and a stand-in filesystem. The test drives `:7070` (health, state, exec with 256 KiB of stdin, snapshot
//! prepares) and the local socket (secrets), kills the supervisor with SIGKILL (the keeper restarts it and it re-attaches),
//! and takes hermit's activity route and magpie's lease route away for a while (the reports are retried, the lease
//! waited for). It samples the current supervisor's `VmRSS` and checks the peak, and that each supervisor process
//! stays flat after its own warm-up (a restart starts a fresh process, so growth is compared within one pid). The
//! 24 h soak with synthetic turns and 1 MiB lines needs the Claude Code driver (WP-23); `soak_long`
//! (`SOAK_SECONDS`, 5 h there) is for `nightly.yml` (WP-28).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

mod common;

use std::time::{Duration, Instant};

use aviary_fakes::FaultKind;
use base64::Engine as _;
use common::{Fakes, KeeperThread, rss};
use supervisor::limits::SUPERVISOR_RSS_BUDGET;
use supervisor_api::ExecRequest;
use supervisor_api::control::{PrepareKind, PrepareRequest};

const SOAK: Duration = Duration::from_secs(20);
const MIB: u64 = 1024 * 1024;

#[test]
#[ignore = "the supervisor process the keeper starts in the soaks"]
fn child() {
    common::child_main();
}

#[allow(clippy::too_many_lines)] // setup, drive, measure
async fn soak(duration: Duration, restart_every: Duration) {
    let fakes = Fakes::start().await;
    let dir = common::dir("supervisor-soak");
    let k = KeeperThread::start(&dir, fakes.proxy(), "child");
    let (mut cc, mut pid) = k.ready(&fakes).await;

    let started = Instant::now();
    // each process's warm-up: half of the time between restarts
    let warm = (duration / 4).min(restart_every / 2);
    let (mut peak, mut n, mut restarts) = (0u64, 0u64, 0u32);
    let mut segment = Segment::new(pid);
    let mut segments: Vec<Segment> = Vec::new();
    let mut next_restart = started + restart_every;
    let payload = base64::engine::general_purpose::STANDARD.encode(vec![b'x'; 256 * 1024]);
    while started.elapsed() < duration {
        if Instant::now() >= next_restart {
            // a supervisor restart while hermit's activity route and magpie's lease route are away
            let a = fakes
                .hermit
                .faults()
                .on("sandbox_activity", FaultKind::Unavailable { retry_after_secs: 1 });
            let l = fakes
                .magpie
                .faults()
                .on("lease", FaultKind::Unavailable { retry_after_secs: 1 });
            k.kill_supervisor();
            tokio::time::sleep(Duration::from_secs(2)).await;
            fakes.hermit.faults().remove(a);
            fakes.magpie.faults().remove(l);
            (cc, pid) = k.ready(&fakes).await;
            segments.push(std::mem::replace(&mut segment, Segment::new(pid)));
            restarts += 1;
            next_restart = Instant::now() + restart_every;
        }
        let _ = cc.health().await.unwrap();
        let _ = cc.state().await.unwrap();
        let out = cc
            .exec(&ExecRequest {
                argv: vec!["/bin/cat".into()],
                cwd: None,
                stdin: Some(payload.clone()),
                timeout_ms: Some(10_000),
                env: std::collections::BTreeMap::default(),
            })
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        let _ = cc
            .prepare(&PrepareRequest {
                kind: PrepareKind::Snapshot,
                force: false,
                deadline_ms: 1000,
            })
            .await
            .unwrap();
        let _ = supervisor::local::ask_secret(&dir.join("agent.sock"), "NOT_GRANTED_KEY").await;
        n += 1;
        let r = rss(pid.cast_unsigned());
        peak = peak.max(r);
        segment.sample(r, warm);
    }
    segment.sample(rss(pid.cast_unsigned()), warm);
    segments.push(segment);
    let agent = common::lines(&dir, "agent.log");
    drop(k);
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "soak: {n} rounds in {duration:?}, {restarts} restarts, peak {} KiB, per process (after warm-up, end) KiB: {:?}",
        peak / 1024,
        segments
            .iter()
            .map(|s| (s.pid, s.baseline / 1024, s.last / 1024))
            .collect::<Vec<_>>()
    );
    assert!(n > 10, "only {n} rounds");
    assert!(restarts >= 1);
    // every restart re-attached to the one generation
    assert_eq!(agent.len(), 1 + restarts as usize, "{agent:?}");
    assert!(agent[1..].iter().all(|l| l.starts_with("attach alive 1 ")), "{agent:?}");
    assert!(peak > 0 && peak < SUPERVISOR_RSS_BUDGET, "peak RSS {} KiB", peak / 1024);
    // flat: no supervisor process grew more than 8 MiB after its own warm-up, and every one was measured
    assert!(
        segments.iter().all(|s| s.baseline > 0),
        "a segment shorter than the warm-up"
    );
    for s in &segments {
        assert!(
            s.last <= s.baseline + 8 * MIB,
            "supervisor {} grew from {} to {}",
            s.pid,
            s.baseline,
            s.last
        );
    }
}

/// One supervisor process's RSS: the sample after its warm-up and the last one.
#[derive(Debug)]
struct Segment {
    pid: i32,
    since: Instant,
    baseline: u64,
    last: u64,
}

impl Segment {
    fn new(pid: i32) -> Self {
        Self {
            pid,
            since: Instant::now(),
            baseline: 0,
            last: 0,
        }
    }

    fn sample(&mut self, r: u64, warm: Duration) {
        if self.baseline == 0 && self.since.elapsed() >= warm {
            self.baseline = r;
        }
        self.last = r;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rss_stays_under_the_budget() {
    soak(SOAK, SOAK / 2).await;
}

/// The longer soak for `nightly.yml`: `SOAK_SECONDS` long (default an hour; the nightly runs 5 h), a supervisor
/// restart (with hermit and magpie away) every 10 minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "an hour or more; nightly"]
#[allow(clippy::disallowed_methods)] // test-only configuration (platform 6.1)
async fn soak_long() {
    let secs = std::env::var("SOAK_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(3600);
    soak(Duration::from_secs(secs), Duration::from_secs(600)).await;
}
