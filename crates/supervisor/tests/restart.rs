//! A real supervisor process under the keeper (WP-21 acceptance "supervisor restart re-attaches to a running
//! child", and the release being final): the first start pre-spawns a stand-in Claude Code and records it in
//! `run.json`; a supervisor killed with SIGKILL is restarted by the keeper, reads `run.json`, takes the `fuse` fd back and
//! re-attaches to the live generation; a release over `:7070` ends with exit 0 and the keeper starts no
//! supervisor again, so the lease is not taken again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{Fakes, KeeperThread, lines};
use hermit_api::sandbox::ReleaseReason;
use supervisor_api::control::{Outcome, ReleaseRequest};

#[test]
#[ignore = "the supervisor process the keeper starts in this file's tests"]
fn child() {
    common::child_main();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_reattaches_and_a_release_is_final() {
    let fakes = Fakes::start().await;
    let dir = common::dir("supervisor-restart");
    let k = KeeperThread::start(&dir, fakes.proxy(), "child");

    // the first start: nothing to re-attach, a generation pre-spawned, a fresh mount
    let (_, first) = k.ready(&fakes).await;
    let agent = lines(&dir, "agent.log");
    assert_eq!(agent.len(), 1, "{agent:?}");
    let parts: Vec<&str> = agent[0].split(' ').collect();
    assert_eq!(&parts[..4], ["attach", "nothing", "spawned", "1"], "{agent:?}");
    let claude: i32 = parts[4].parse().unwrap();
    assert_eq!(lines(&dir, "fs.log"), vec!["mount false"]);
    let epoch = fakes.magpie.lease_epoch(fakes.cid);
    assert!(epoch >= 1);

    // the supervisor dies; the keeper starts another, which re-attaches to the same Claude Code
    assert_eq!(k.kill_supervisor(), first);
    let (cc, second) = k.ready(&fakes).await;
    assert_ne!(second, first);
    assert_eq!(k.client.status().unwrap().supervisor_restarts, 1);
    let agent = lines(&dir, "agent.log");
    assert_eq!(agent[1], format!("attach alive 1 {claude}"), "{agent:?}");
    assert_eq!(lines(&dir, "fs.log"), vec!["mount false", "mount true"]);
    assert!(
        k.client.status().unwrap().children.contains(&(claude, "claude".into())),
        "Claude Code lived through the restart"
    );
    let health = cc.health().await.unwrap();
    assert!(health.ready);

    // a release: exit 0, and the keeper does not start the supervisor again
    let epoch = fakes.magpie.lease_epoch(fakes.cid);
    let rel = ReleaseRequest::new(ReleaseReason::Idle, false, 30_000).unwrap();
    assert_eq!(cc.release(&rel).await.unwrap(), Outcome::Done);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let exits = k.client.exits(0).unwrap();
        if let Some(e) = exits.iter().find(|e| e.pid == second) {
            assert_eq!(e.code, Some(0), "{e:?}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the supervisor did not exit");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(k.supervisor(), None, "not restarted after a release");
    assert_eq!(k.client.status().unwrap().supervisor_restarts, 2);
    assert_eq!(
        fakes.magpie.lease_epoch(fakes.cid),
        epoch,
        "the lease was not taken again"
    );
    assert_eq!(lines(&dir, "agent.log").len(), 3, "{:?}", lines(&dir, "agent.log"));
    assert_eq!(lines(&dir, "agent.log")[2], "stop");
    assert!(dir.join("run").join(supervisor::keeper::RELEASED_NAME).exists());
    // hermit was told `released`
    let last = fakes.hermit.script().calls_to("sandbox_activity").pop().unwrap();
    assert_eq!(last.body.unwrap()["state"], "released");
    drop(k);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runtime_stop_is_a_clean_forced_release_within_the_keepers_grace() {
    let fakes = Fakes::start().await;
    let dir = common::dir("supervisor-sigterm");
    let mut k = KeeperThread::start(&dir, fakes.proxy(), "child");
    k.ready(&fakes).await;
    // SIGTERM to PID 1: forwarded, a forced release (Claude Code stopped through the keeper), exit 0
    let t = std::time::Instant::now();
    k.stop();
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "{:?}\n{}",
        t.elapsed(),
        lines(&dir, "child.log").join("\n")
    );
    let agent = lines(&dir, "agent.log");
    assert_eq!(agent.last().map(String::as_str), Some("stop"), "{agent:?}");
    assert_eq!(lines(&dir, "fs.log").last().map(String::as_str), Some("unmount"));
    let last = fakes.hermit.script().calls_to("sandbox_activity").pop().unwrap();
    assert_eq!(last.body.unwrap()["state"], "released");
    let _ = std::fs::remove_dir_all(dir);
}
