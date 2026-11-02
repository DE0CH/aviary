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
    sys::send_packet(fd, &p, None, true).unwrap();
    std::thread::sleep(Duration::from_millis(100));
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
    std::thread::sleep(Duration::from_millis(100));
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
    // reads nothing for 2 s: a 400 KiB message fills the pipe and waits
    let mut s = k.spec("claude", &["/bin/sh", "-c", "sleep 2; exec cat"]);
    s.stdin_pipe = true;
    let pid = k.client.spawn(&s).unwrap();
    let client = k.client.clone();
    let big = vec![b'z'; 400 * 1024];
    let writer = std::thread::spawn(move || client.stdin(pid, super::new_msg_id(), &big).unwrap());
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    let other = k.client.spawn(&k.spec("exec", &["/bin/true"])).unwrap();
    k.client.exits(0).unwrap();
    k.client.status().unwrap();
    assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
    assert!(!writer.is_finished());
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
    let k = Running::start(Some(vec!["/bin/sh".into(), "-c".into(), "exit 3".into()]));
    std::thread::sleep(Duration::from_millis(1500));
    let exits: Vec<_> = k
        .client
        .exits(0)
        .unwrap()
        .into_iter()
        .filter(|e| e.name == "supervisor")
        .collect();
    // starts at 0, 0.1, 0.3, 0.8 (then 1.8): four or five exits in 1.5 s, each code 3
    assert!((4..=5).contains(&exits.len()), "{exits:?}");
    assert!(exits.iter().all(|e| e.code == Some(3)));
    let gaps: Vec<u64> = exits.windows(2).map(|w| w[1].at_ms - w[0].at_ms).collect();
    assert!(gaps[0] >= 90 && gaps[1] >= 190 && gaps[2] >= 490, "{gaps:?}");
    assert!(k.client.status().unwrap().supervisor_restarts >= 4);
}

#[test]
fn sigterm_stops_the_supervisor_then_everything() {
    // SIGTERM is blocked across the supervisor's exec: a stand-in ends on it only with a trap
    let mut k = Running::start(Some(vec![
        "/bin/sh".into(),
        "-c".into(),
        "trap 'exit 0' TERM; while :; do sleep 0.02; done".into(),
    ]));
    std::thread::sleep(Duration::from_millis(200));
    let st = k.client.status().unwrap();
    let sup = st.children.iter().find(|(_, n)| n == "supervisor").unwrap().0;
    let child = k.client.spawn(&k.spec("claude", &["/bin/sleep", "100"])).unwrap();
    let t = Instant::now();
    assert_eq!(k.stop(), 0);
    // sleep ends on the forwarded SIGTERM at once; the child is killed after it
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    let dead = |pid: i32| {
        sys::kill(pid, 0).is_err()
            || std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z"))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(dead(sup) && dead(child)) {
        assert!(Instant::now() < deadline, "still alive");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn while_stopping_the_keeper_still_answers_but_spawns_nothing() {
    // a supervisor whose release takes 1.5 s after SIGTERM keeps the keeper in its grace period
    let mut k = Running::start(Some(vec![
        "/bin/sh".into(),
        "-c".into(),
        "trap 'sleep 1.5; exit 0' TERM; while :; do sleep 0.05; done".into(),
    ]));
    std::thread::sleep(Duration::from_millis(300));
    let child = k.client.spawn(&k.spec("claude", &["/bin/sleep", "100"])).unwrap();
    let late = k.spec("exec", &["/bin/true"]);
    let client = k.client.clone();
    let stopper = std::thread::spawn(move || k.stop());
    std::thread::sleep(Duration::from_millis(300));
    // a forced release signals Claude Code and polls the exits through the keeper during the grace
    client.signal(-child, libc::SIGKILL).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.exits(0).unwrap().iter().any(|e| e.pid == child) {
        assert!(Instant::now() < deadline, "no exit while stopping");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(client.spawn(&late).unwrap_err().errno(), Some(libc::ESHUTDOWN));
    // the supervisor's exit ends the stop
    let t = Instant::now();
    assert_eq!(stopper.join().unwrap(), 0);
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
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
    std::thread::sleep(Duration::from_millis(200));
    assert!(!k.client.status().unwrap().released);
    k.client.released().unwrap();
    assert!(k.client.status().unwrap().released);
    std::fs::write(k.dir.join("go"), "").unwrap();
    std::thread::sleep(Duration::from_millis(800));
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
    let k2 = Running::start(Some(vec!["/bin/sh".into(), "-c".into(), "exit 0".into()]));
    std::thread::sleep(Duration::from_millis(800));
    assert!(sup(&k2).len() >= 3, "{:?}", sup(&k2));
    // a crash after RELEASED (exit 70) is restarted too: the new supervisor sees `released` and only reports
    let k3 = Running::start_with(|d| {
        let done = d.join("done");
        waits(
            d,
            &format!("if [ -e '{0}' ]; then exit 0; fi; touch '{0}'; exit 70", done.display()),
        )
    });
    std::thread::sleep(Duration::from_millis(200));
    k3.client.released().unwrap();
    std::fs::write(k3.dir.join("go"), "").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(sup(&k3), vec![Some(70), Some(0)]);
}

#[test]
fn a_sigterm_while_no_supervisor_runs_starts_one_at_once_and_hands_it_the_stop() {
    // the supervisor crashes four times (backoff 0.1, 0.2, 0.5, then 1 s); the fifth records the SIGTERM it gets.
    // A SIGTERM sent during the 1 s backoff starts it at once, and it gets the SIGTERM (blocked across its exec,
    // so even one sent before its trap exists is delivered)
    let mut k = Running::start_with(|d| {
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while k
        .client
        .exits(0)
        .unwrap()
        .iter()
        .filter(|e| e.name == "supervisor")
        .count()
        < 4
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let t = Instant::now();
    assert_eq!(k.stop(), 0);
    // no backoff (1 s) waited out, and no grace (20 s)
    assert!(t.elapsed() < Duration::from_millis(900), "{:?}", t.elapsed());
    assert!(
        k.dir.join("got-term").exists(),
        "the started supervisor never got the SIGTERM"
    );
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
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    k.client.status().unwrap();
    k.client.exits(0).unwrap();
    let other = k.client.spawn(&k.spec("exec", &["/bin/true"])).unwrap();
    assert_eq!(k.wait_exit(other).code, Some(0));
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
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
    let mut s = k.spec("claude", &["/bin/sh", "-c", "sleep 1; exec cat"]);
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
    std::thread::sleep(Duration::from_millis(200));
    sys::close(fd);
    // the restarted supervisor sends it again: held until the first is in the pipe, then ALREADY
    assert_eq!(k.client.stdin(pid, id, &data).unwrap(), StdinOutcome::Already);
    k.client.close_stdin(pid).unwrap();
    assert_eq!(k.wait_exit(pid).code, Some(0));
    assert_eq!(std::fs::read(k.file("claude.out")).unwrap().len(), data.len());
}
