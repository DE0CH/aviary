//! Keeper protocol tests (WP-21 acceptance): spawn, stdin written whole and at most once per `msg_id`, an
//! incomplete message discarded, the fd store, the exits ring, signals, the supervisor's restart with backoff, and
//! the stop. The keeper runs on a thread with its own directories; it reaps only its own children, so other tests
//! in the process are not disturbed.

#![allow(clippy::unwrap_used, clippy::expect_used, unsafe_code)]

use std::os::fd::AsRawFd as _;
use std::time::{Duration, Instant};

use super::proto::{Op, STDIN_HEADER, Writer};
use super::server::{Config, Keeper, Signals};
use super::testing::Running;
use super::{Client, KeeperError, StdinOutcome, sys};

/// Waits (at most 30 s) until `f` holds.
fn until(what: &str, f: impl Fn() -> bool) {
    let guard = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < guard, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `f` on a thread, which must be done within 30 s (a call the keeper holds up never is).
fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(30))
        .unwrap_or_else(|_| panic!("{what}: held up"))
}

/// The bytes waiting in `pid`'s stdin pipe (what it has not read yet).
fn unread_stdin(pid: i32) -> usize {
    let Ok(f) = std::fs::File::open(format!("/proc/{pid}/fd/0")) else {
        return 0;
    };
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD writes one int through a valid pointer, on a descriptor this function owns.
    #[allow(unsafe_code)]
    let r = unsafe { libc::ioctl(f.as_raw_fd(), libc::FIONREAD, &raw mut n) };
    if r == 0 { usize::try_from(n).unwrap_or(0) } else { 0 }
}

/// A shell line that waits until `flag` exists.
fn wait_for(flag: &std::path::Path) -> String {
    format!("while [ ! -e '{}' ]; do sleep 0.02; done", flag.display())
}

#[test]
fn spawn_runs_with_exactly_the_given_environment_and_files() {
    let k = Running::start(None);
    let pid = k
        .client
        .spawn(&k.spec("exec", &["/bin/sh", "-c", "env | sort; pwd; echo oops >&2; exit 7"]))
        .unwrap();
    let e = k.wait_exit(pid);
    assert_eq!((e.name.as_str(), e.code, e.signal), ("exec", Some(7), None));
    let out = std::fs::read_to_string(k.file("exec.out")).unwrap();
    // sh adds PWD (and may add SHLVL/_); nothing of the test process's environment leaks in
    assert!(
        out.contains("ONLY=this\n") && out.contains("PATH=/usr/bin:/bin\n"),
        "{out}"
    );
    assert!(!out.contains("HOME="), "{out}");
    assert!(out.trim_end().ends_with(k.dir.to_str().unwrap()), "{out}");
    assert_eq!(std::fs::read_to_string(k.file("exec.err")).unwrap(), "oops\n");
    // its own session
    let status = k.client.status().unwrap();
    assert_eq!(status.boot_id.len(), 16);
    assert!(status.children.is_empty(), "{:?}", status.children);
}

#[test]
fn spawn_failures_are_refused_with_the_errno() {
    let k = Running::start(None);
    let err = k.client.spawn(&k.spec("exec", &["/nonexistent/binary"])).unwrap_err();
    assert_eq!(err.errno(), Some(libc::ENOENT), "{err}");
    let mut s = k.spec("exec", &["/bin/true"]);
    s.cwd = Some("/nonexistent-dir".into());
    assert_eq!(k.client.spawn(&s).unwrap_err().errno(), Some(libc::ENOENT));
}

#[test]
fn stdin_is_written_whole_and_at_most_once_per_msg_id() {
    let k = Running::start(None);
    let mut s = k.spec("claude", &["/bin/cat"]);
    s.stdin_pipe = true;
    let pid = k.client.spawn(&s).unwrap();
    let a = b"{\"type\":\"user\",\"n\":1}\n".to_vec();
    let id_a = super::new_msg_id();
    assert_eq!(k.client.stdin(pid, id_a, &a).unwrap(), StdinOutcome::Written);
    assert_eq!(k.client.stdin(pid, id_a, &a).unwrap(), StdinOutcome::Already);
    // larger than a pooled buffer and than a packet: spooled, then copied as cat reads
    let mut b = vec![b'x'; 300 * 1024];
    b.push(b'\n');
    let id_b = super::new_msg_id();
    assert_eq!(k.client.stdin(pid, id_b, &b).unwrap(), StdinOutcome::Written);
    assert_eq!(k.client.stdin(pid, id_b, &b).unwrap(), StdinOutcome::Already);
    assert_eq!(k.client.stdin(pid, id_a, &a).unwrap(), StdinOutcome::Already);
    k.client.close_stdin(pid).unwrap();
    assert_eq!(k.wait_exit(pid).code, Some(0));
    let out = std::fs::read(k.file("claude.out")).unwrap();
    assert_eq!(out, [a, b].concat());
}

#[test]
fn the_ring_forgets_after_sixteen_messages() {
    let k = Running::start(None);
    let mut s = k.spec("claude", &["/bin/cat"]);
    s.stdin_pipe = true;
    let pid = k.client.spawn(&s).unwrap();
    let ids: Vec<_> = (0..17).map(|_| super::new_msg_id()).collect();
    for id in &ids {
        assert_eq!(k.client.stdin(pid, *id, b"m\n").unwrap(), StdinOutcome::Written);
    }
    assert_eq!(k.client.stdin(pid, ids[16], b"m\n").unwrap(), StdinOutcome::Already);
    assert_eq!(k.client.stdin(pid, ids[1], b"m\n").unwrap(), StdinOutcome::Already);
    // the oldest fell out of the 16-entry ring
    assert_eq!(k.client.stdin(pid, ids[0], b"m\n").unwrap(), StdinOutcome::Written);
}

/// Sends a `STDIN` header announcing `total` bytes with only `part` of them, then drops the connection (a
/// supervisor that died mid-send).
fn send_partial(k: &Running, pid: i32, total: usize, part: &[u8]) {
    use std::os::unix::ffi::OsStrExt as _;
    let fd = sys::connect_seqpacket(k.dir.join("run/keeper.sock").as_os_str().as_bytes()).unwrap();
    let mut buf = vec![0u8; 1024];
    let mut w = Writer::new(&mut buf);
    w.u8(Op::Stdin as u8)
        .i32(pid)
        .bytes_raw(&super::new_msg_id())
        .u64(total as u64)
        .bytes_raw(part);
    let p = w.finish().unwrap().to_vec();
    // closed at once: what was sent is still read, then the end
    sys::send_packet(fd, &p, None, true).unwrap();
    sys::close(fd);
}

#[test]
fn an_incomplete_message_is_discarded_never_half_written() {
    let k = Running::start(None);
    let mut s = k.spec("claude", &["/bin/cat"]);
    s.stdin_pipe = true;
    let pid = k.client.spawn(&s).unwrap();
    send_partial(&k, pid, 1000, b"{\"type\":\"us");
    // a large one cut in its spool phase too
    send_partial(&k, pid, 200 * 1024, &[b'y'; 500]);
    let c = b"{\"type\":\"user\",\"n\":3}\n";
    assert_eq!(
        k.client.stdin(pid, super::new_msg_id(), c).unwrap(),
        StdinOutcome::Written
    );
    k.client.close_stdin(pid).unwrap();
    k.wait_exit(pid);
    assert_eq!(std::fs::read(k.file("claude.out")).unwrap(), c);
}

#[test]
fn a_message_claude_has_not_read_holds_up_nothing_else() {
    let k = Running::start(None);
    // reads nothing until the test lets it: a 400 KiB message fills the pipe and waits
    let read = k.dir.join("read");
    let mut s = k.spec("claude", &["/bin/sh", "-c", &format!("{}; exec cat", wait_for(&read))]);
    s.stdin_pipe = true;
    let pid = k.client.spawn(&s).unwrap();
    let client = k.client.clone();
    let big = vec![b'z'; 400 * 1024];
    let writer = std::thread::spawn(move || client.stdin(pid, super::new_msg_id(), &big).unwrap());
    until("the pipe full", || unread_stdin(pid) >= 64 * 1024);
    let client = k.client.clone();
    let spec = k.spec("exec", &["/bin/true"]);
    let other = within("the keeper's other calls", move || {
        let other = client.spawn(&spec).unwrap();
        client.exits(0).unwrap();
        client.status().unwrap();
        other
    });
    assert!(!writer.is_finished());
    std::fs::write(&read, b"").unwrap();
    assert_eq!(writer.join().unwrap(), StdinOutcome::Written);
    k.wait_exit(other);
    k.client.close_stdin(pid).unwrap();
    k.wait_exit(pid);
    assert_eq!(std::fs::read(k.file("claude.out")).unwrap().len(), 400 * 1024);
}

#[test]
fn a_child_that_exits_mid_message_fails_the_write() {
    let k = Running::start(None);
    let mut s = k.spec("claude", &["/bin/sh", "-c", "head -c 10 >/dev/null"]);
    s.stdin_pipe = true;
    let pid = k.client.spawn(&s).unwrap();
    let err = k
        .client
        .stdin(pid, super::new_msg_id(), &vec![b'q'; 1024 * 1024])
        .unwrap_err();
    assert!(
        matches!(err, KeeperError::Refused { errno, .. } if errno == libc::EPIPE),
        "{err}"
    );
    k.wait_exit(pid);
    assert!(k.client.stdin(pid, super::new_msg_id(), b"x").is_err());
}

#[test]
fn the_fd_store_keeps_a_descriptor_across_clients() {
    let k = Running::start(None);
    assert!(k.client.get_fd("fuse").unwrap().is_none());
    let (r, w) = sys::pipe().unwrap();
    k.client.put_fd("fuse", r).unwrap();
    sys::close(r);
    // a new client (as after a supervisor restart) gets a working duplicate
    let got = Client::new(&k.dir.join("run")).get_fd("fuse").unwrap().unwrap();
    sys::write_all(w, b"ping").unwrap();
    let mut b = [0u8; 4];
    assert_eq!(sys::read(got.as_raw_fd(), &mut b).unwrap(), 4);
    assert_eq!(&b, b"ping");
    // replacing keeps one entry per name
    let (r2, w2) = sys::pipe().unwrap();
    k.client.put_fd("fuse", r2).unwrap();
    sys::close(r2);
    let got2 = k.client.get_fd("fuse").unwrap().unwrap();
    sys::write_all(w2, b"pong").unwrap();
    assert_eq!(sys::read(got2.as_raw_fd(), &mut b).unwrap(), 4);
    assert_eq!(&b, b"pong");
    for fd in [w, w2] {
        sys::close(fd);
    }
}

#[test]
fn the_exits_ring_keeps_the_last_sixty_four() {
    let k = Running::start(None);
    let mut last = 0;
    for _ in 0..70 {
        last = k.client.spawn(&k.spec("exec", &["/bin/true"])).unwrap();
    }
    k.wait_exit(last);
    let deadline = Instant::now() + Duration::from_secs(10);
    while k.client.exits(0).unwrap().last().map(|e| e.n) != Some(70) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let all = k.client.exits(0).unwrap();
    assert_eq!(all.len(), 64);
    assert_eq!(all.first().unwrap().n, 7);
    assert!(all.windows(2).all(|w| w[1].n == w[0].n + 1));
    let tail = k.client.exits(68).unwrap();
    assert_eq!(tail.iter().map(|e| e.n).collect::<Vec<_>>(), vec![69, 70]);
    assert_eq!(k.client.exits(70).unwrap(), vec![]);
}

#[test]
fn signals_reach_children_and_their_groups_only() {
    let k = Running::start(None);
    let pid = k.client.spawn(&k.spec("claude", &["/bin/sleep", "100"])).unwrap();
    assert!(k.client.status().unwrap().children.contains(&(pid, "claude".into())));
    // not the keeper's child: refused, nothing sent
    assert_eq!(
        k.client.signal(sys::getpid(), libc::SIGTERM).unwrap_err().errno(),
        Some(libc::ESRCH)
    );
    assert_eq!(
        k.client.signal(-1, libc::SIGKILL).unwrap_err().errno(),
        Some(libc::ESRCH)
    );
    k.client.signal(-pid, libc::SIGTERM).unwrap();
    let e = k.wait_exit(pid);
    assert_eq!((e.code, e.signal), (None, Some(libc::SIGTERM)));
}

#[test]
fn the_supervisor_is_restarted_with_backoff() {
    // a stand-in that never finishes a release: its stop ends at a short grace
    let k = Running::start_with_grace(Duration::from_secs(1), |_| {
        Some(vec!["/bin/sh".into(), "-c".into(), "exit 3".into()])
    });
    let exits = || -> Vec<_> {
        k.client
            .exits(0)
            .unwrap()
            .into_iter()
            .filter(|e| e.name == "supervisor")
            .collect()
    };
    until("four exits", || exits().len() >= 4);
    // starts at 0, 0.1, 0.3, 0.8: each gap at least its delay, each exit code 3
    let exits = exits();
    assert!(exits.iter().all(|e| e.code == Some(3)));
    let gaps: Vec<u64> = exits.windows(2).map(|w| w[1].at_ms - w[0].at_ms).collect();
    assert!(gaps[0] >= 90 && gaps[1] >= 190 && gaps[2] >= 490, "{gaps:?}");
    assert!(k.client.status().unwrap().supervisor_restarts >= 4);
}

#[test]
fn sigterm_stops_the_supervisor_then_everything() {
    // SIGTERM is blocked across the supervisor's exec: a stand-in ends on it only with a trap
    let ready = std::env::temp_dir().join(format!("keeper-ready-{}", uuid::Uuid::new_v4().simple()));
    // a grace no test outlasts: the stand-in's end, not the grace, ends the stop
    let mut k = Running::start_with_grace(Duration::from_hours(1), |_| {
        Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "trap 'exit 0' TERM; touch '{}'; while :; do sleep 0.02; done",
                ready.display()
            ),
        ])
    });
    until("the stand-in's trap", || ready.exists());
    let _ = std::fs::remove_file(&ready);
    let st = k.client.status().unwrap();
    let sup = st.children.iter().find(|(_, n)| n == "supervisor").unwrap().0;
    let child = k.client.spawn(&k.spec("claude", &["/bin/sleep", "100"])).unwrap();
    // the stand-in's release is final (a real supervisor tells RELEASED before its exit 0)
    k.client.released().unwrap();
    // the stand-in ends on the forwarded SIGTERM; the child is killed after it (well before the 20 s grace)
    assert_eq!(within("the stop", move || k.stop()), 0);
    let dead = |pid: i32| {
        sys::kill(pid, 0).is_err()
            || std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z"))
    };
    until("both gone", || dead(sup) && dead(child));
}

#[test]
fn while_stopping_the_keeper_still_answers_but_spawns_nothing() {
    // a supervisor whose release goes on after SIGTERM until the test ends it keeps the keeper in its grace period
    let d = std::env::temp_dir().join(format!("keeper-stopping-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).unwrap();
    let (ready, termed, finish) = (d.join("ready"), d.join("termed"), d.join("finish"));
    let mut k = Running::start_with_grace(Duration::from_hours(1), |_| {
        Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "trap 'touch {termed}; {wait}; exit 0' TERM; touch '{ready}'; while :; do sleep 0.05; done",
                termed = termed.display(),
                wait = wait_for(&finish),
                ready = ready.display()
            ),
        ])
    });
    until("the stand-in's trap", || ready.exists());
    let child = k.client.spawn(&k.spec("claude", &["/bin/sleep", "100"])).unwrap();
    let late = k.spec("exec", &["/bin/true"]);
    let client = k.client.clone();
    client.released().unwrap();
    let stopper = std::thread::spawn(move || k.stop());
    until("the SIGTERM forwarded", || termed.exists());
    // a forced release signals Claude Code and polls the exits through the keeper during the grace
    client.signal(-child, libc::SIGKILL).unwrap();
    until("the exit while stopping", || {
        client.exits(0).unwrap().iter().any(|e| e.pid == child)
    });
    assert_eq!(client.spawn(&late).unwrap_err().errno(), Some(libc::ESHUTDOWN));
    // the supervisor's exit ends the stop (well before the 20 s grace)
    std::fs::write(&finish, b"").unwrap();
    assert_eq!(within("the stop", move || stopper.join().unwrap()), 0);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn boot_id_survives_a_keeper_restart() {
    let k = Running::start(None);
    let id = k.client.status().unwrap().boot_id;
    let (stop_r, stop_w) = sys::pipe().unwrap();
    let cfg = Config {
        run_dir: k.dir.join("run"),
        mount_run_tmpfs: false,
        spool_dir: k.dir.join("spool2"),
        supervisor: None,
        protect_self: false,
        signals: Signals::Pipe(stop_r),
        stop_grace: crate::limits::KEEPER_STOP_GRACE,
        backoff: &crate::limits::KEEPER_BACKOFF,
    };
    let again = Keeper::new(cfg).unwrap();
    assert_eq!(again.boot_id(), id.as_bytes());
    sys::close(stop_w);
}

#[test]
fn a_supervisor_that_exits_0_after_a_release_is_not_restarted() {
    // the fake supervisor waits for `go` (the test has told the keeper RELEASED by then), then exits 0
    let waits = |d: &std::path::Path, then: &str| {
        let go = d.join("go");
        Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("while [ ! -e '{}' ]; do sleep 0.02; done; {then}", go.display()),
        ])
    };
    let sup = |k: &Running| {
        k.client
            .exits(0)
            .unwrap()
            .into_iter()
            .filter(|e| e.name == "supervisor")
            .map(|e| e.code)
            .collect::<Vec<_>>()
    };
    let k = Running::start_with(|d| waits(d, "exit 0"));
    assert!(!k.client.status().unwrap().released);
    k.client.released().unwrap();
    assert!(k.client.status().unwrap().released);
    std::fs::write(k.dir.join("go"), "").unwrap();
    until("the exit", || !sup(&k).is_empty());
    // the exit was a release: nothing is started after it
    assert_eq!(sup(&k), vec![Some(0)]);
    assert!(
        !k.client
            .status()
            .unwrap()
            .children
            .iter()
            .any(|(_, n)| n == "supervisor")
    );
    // exit 0 without RELEASED is not a release: restarted
    let k2 = Running::start_with_grace(Duration::from_secs(1), |_| {
        Some(vec!["/bin/sh".into(), "-c".into(), "exit 0".into()])
    });
    until("restarted twice", || sup(&k2).len() >= 3);
    // a crash after RELEASED (exit 70) is restarted too: the new supervisor sees `released` and only reports
    let k3 = Running::start_with(|d| {
        let done = d.join("done");
        waits(
            d,
            &format!("if [ -e '{0}' ]; then exit 0; fi; touch '{0}'; exit 70", done.display()),
        )
    });
    k3.client.released().unwrap();
    std::fs::write(k3.dir.join("go"), "").unwrap();
    until("the crash and the second exit", || sup(&k3).len() >= 2);
    assert_eq!(sup(&k3), vec![Some(70), Some(0)]);
}

#[test]
fn a_sigterm_while_no_supervisor_runs_starts_one_at_once_and_hands_it_the_stop() {
    // the supervisor crashes four times (no delay, then a delay no test outlasts); the fifth records the SIGTERM it
    // gets. A SIGTERM sent during that delay starts it at once, and it gets the SIGTERM (blocked across its exec, so
    // even one sent before its trap exists is delivered)
    static BACKOFF: [Duration; 4] = [Duration::ZERO, Duration::ZERO, Duration::ZERO, Duration::from_hours(1)];
    let mut k = Running::start_with_backoff(crate::limits::KEEPER_STOP_GRACE, &BACKOFF, |d| {
        let count = d.join("count");
        let got = d.join("got-term");
        Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "echo x >> '{0}'; if [ $(wc -l < '{0}') -le 4 ]; then exit 3; fi; trap 'touch {1}; exit 0' TERM; \
                 while :; do sleep 0.02; done",
                count.display(),
                got.display()
            ),
        ])
    });
    let crashes = || {
        k.client
            .exits(0)
            .unwrap()
            .iter()
            .filter(|e| e.name == "supervisor")
            .count()
    };
    until("four crashes", || crashes() >= 4);
    k.client.released().unwrap();
    // no delay (an hour) waited out, and no grace (20 s)
    let (code, k) = within("the stop", move || (k.stop(), k));
    assert_eq!(code, 0);
    assert!(
        k.dir.join("got-term").exists(),
        "the started supervisor never got the SIGTERM"
    );
}

#[test]
fn a_supervisor_that_dies_during_the_stop_is_started_again_and_finishes_it() {
    // the first supervisor dies on the SIGTERM (a crash mid-release); the second finishes the release (RELEASED,
    // then exit 0) and records that it got the SIGTERM (outside the keeper's directory, which goes with it)
    let got = std::env::temp_dir().join(format!("got-term-{}", uuid::Uuid::new_v4().simple()));
    // a grace no test outlasts: the successor's end, not the grace, ends the stop
    let mut k = Running::start_with_grace(Duration::from_hours(1), |d| {
        let count = d.join("count");
        Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "echo x >> '{0}'; if [ $(wc -l < '{0}') -le 1 ]; then trap 'exit 70' TERM; touch '{2}'; \
                 else trap 'touch {1}; exit 0' TERM; fi; while :; do sleep 0.02; done",
                count.display(),
                got.display(),
                d.join("ready").display()
            ),
        ])
    });
    until("the first's trap", || k.dir.join("ready").exists());
    let child = k.client.spawn(&k.spec("claude", &["/bin/sleep", "100"])).unwrap();
    let client = k.client.clone();
    let stopper = std::thread::spawn(move || k.stop());
    // the crash is followed by a new supervisor while the stop goes on; Claude Code is not killed meanwhile
    until("the first supervisor's exit", || {
        client.exits(0).unwrap().iter().any(|e| e.name == "supervisor")
    });
    until("a supervisor started during the stop", || {
        client.status().unwrap().children.iter().any(|(_, n)| n == "supervisor")
    });
    assert!(
        client.status().unwrap().children.iter().any(|(p, _)| *p == child),
        "claude killed before the release finished"
    );
    let first = client.exits(0).unwrap();
    assert_eq!(
        first.iter().find(|e| e.name == "supervisor").map(|e| e.code),
        Some(Some(70))
    );
    // the successor finishes: RELEASED, exit 0
    client.released().unwrap();
    assert_eq!(within("the stop", move || stopper.join().unwrap()), 0);
    assert!(got.exists(), "the successor never got the SIGTERM");
    let _ = std::fs::remove_file(&got);
}

#[test]
fn spawn_answers_the_exit_counter_at_the_fork() {
    let k = Running::start(None);
    let a = k.client.spawn_tracked(&k.spec("exec", &["/bin/true"])).unwrap();
    k.wait_exit(a.pid);
    let b = k.client.spawn_tracked(&k.spec("exec", &["/bin/true"])).unwrap();
    assert_eq!((a.since, b.since), (0, 1));
}

#[test]
fn a_child_slow_to_reach_exec_holds_up_only_its_own_spawn() {
    let k = Running::start(None);
    // its stdout is a FIFO nobody reads yet: the child blocks in open(2) before exec, as in a stalled chdir
    let fifo = k.file("out.fifo");
    let c = std::ffi::CString::new(fifo.clone()).unwrap();
    // SAFETY: mkfifo on a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let mut s = k.spec("exec", &["/bin/echo", "hi"]);
    s.stdout = Some(fifo.clone());
    let client = k.client.clone();
    let slow = std::thread::spawn(move || client.spawn(&s));
    // the other calls are answered meanwhile (one held up by the slow child would never be)
    let client = k.client.clone();
    let spec = k.spec("exec", &["/bin/true"]);
    let other = within("the keeper's other calls", move || {
        client.status().unwrap();
        client.exits(0).unwrap();
        client.spawn(&spec).unwrap()
    });
    assert_eq!(k.wait_exit(other).code, Some(0));
    assert!(!slow.is_finished(), "answered before exec");
    // a reader unblocks it: the spawn is answered with its pid
    let mut out = std::fs::File::open(&fifo).unwrap();
    let pid = slow.join().unwrap().unwrap();
    let mut got = String::new();
    std::io::Read::read_to_string(&mut out, &mut got).unwrap();
    assert_eq!(got, "hi\n");
    assert_eq!(k.wait_exit(pid).code, Some(0));
}

#[test]
fn a_resend_while_the_original_is_still_being_written_is_already() {
    use std::os::unix::ffi::OsStrExt as _;
    let k = Running::start(None);
    let read = k.dir.join("read");
    let mut s = k.spec("claude", &["/bin/sh", "-c", &format!("{}; exec cat", wait_for(&read))]);
    s.stdin_pipe = true;
    let pid = k.client.spawn(&s).unwrap();
    let data = vec![b'w'; 400 * 1024];
    let id = super::new_msg_id();
    // the supervisor sends the whole message, then dies before the answer
    let fd = sys::connect_seqpacket(k.dir.join("run/keeper.sock").as_os_str().as_bytes()).unwrap();
    let mut buf = vec![0u8; crate::limits::KEEPER_PACKET];
    let first = crate::limits::KEEPER_PACKET - STDIN_HEADER;
    let mut w = Writer::new(&mut buf);
    w.u8(Op::Stdin as u8)
        .i32(pid)
        .bytes_raw(&id)
        .u64(data.len() as u64)
        .bytes_raw(&data[..first]);
    let p = w.finish().unwrap().to_vec();
    sys::send_packet(fd, &p, None, true).unwrap();
    for chunk in data[first..].chunks(crate::limits::KEEPER_PACKET - 1) {
        let mut pkt = vec![Op::StdinMore as u8];
        pkt.extend_from_slice(chunk);
        sys::send_packet(fd, &pkt, None, true).unwrap();
    }
    // the first is being written (the pipe is full, nothing reads it yet) when its sender is gone
    until("the pipe full", || unread_stdin(pid) >= 64 * 1024);
    sys::close(fd);
    // the restarted supervisor sends it again: held until the first is in the pipe, then ALREADY
    let client = k.client.clone();
    let again = std::thread::spawn(move || client.stdin(pid, id, &data).unwrap());
    std::fs::write(&read, b"").unwrap();
    assert_eq!(
        within("the resend", move || again.join().unwrap()),
        StdinOutcome::Already
    );
    k.client.close_stdin(pid).unwrap();
    assert_eq!(k.wait_exit(pid).code, Some(0));
    assert_eq!(std::fs::read(k.file("claude.out")).unwrap().len(), 400 * 1024);
}
