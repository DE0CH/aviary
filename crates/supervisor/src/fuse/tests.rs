//! The FUSE layer against fake magpie through the sentry stand-in (supervisor 22's unit and integration list: every
//! opcode against both scopes, the errno map, the retry rule, the repair, the journal replay at each fault point,
//! the resend rule of 9.3, node ids across restarts, `lease_lost` and `not_current_sandbox`).

#![allow(clippy::many_single_char_names, clippy::similar_names)] // FUSE argument names: h(eader), b(ody), p(ath)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aviary_core::auth::Caller;
use aviary_fakes::fault::{FaultKind, Hold};
use aviary_fakes::magpie::FakeMagpie;
use magpie_api::sandbox::LeaseResponse;
use uuid::Uuid;

use super::dispatch::Core;
use super::proto::{op, opname};
use super::remote::Timing;
use super::testing::{Sentry, body, out};
use super::{Config, FuseFs, Hooks, Point};

const PID: u32 = 4242;
const O_RDWR: i32 = libc::O_RDWR;

struct World {
    rt: tokio::runtime::Runtime,
    magpie: Arc<FakeMagpie>,
    served: aviary_fakes::magpie::Running,
    dir: PathBuf,
    cid: Uuid,
    sentry: Arc<Sentry>,
}

fn world() -> World {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let magpie = FakeMagpie::new();
    let served = rt.block_on(magpie.serve()).unwrap();
    let dir = std::env::temp_dir().join(format!("fuse-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    World {
        rt,
        magpie,
        served,
        dir,
        cid: Uuid::now_v7(),
        sentry: Sentry::new(),
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.sentry.close_all();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl World {
    fn client(&self, sandbox: &str) -> magpie_api::client::Client {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("aviary-sandbox-id", sandbox.parse().unwrap());
        h.insert("aviary-sandbox-conversation", self.cid.to_string().parse().unwrap());
        h.insert("aviary-sandbox-person", "staff@example.org".parse().unwrap());
        let http = reqwest::Client::builder().default_headers(h).build().unwrap();
        self.served.client(http, Caller::Gateway)
    }

    fn cfg(&self, timing: Timing) -> Config {
        let mut c = Config::under(&self.dir, &self.dir.join("mnt"));
        c.slots = 8;
        c.timing = timing;
        c
    }

    fn fs(&self, timing: Timing) -> Arc<FuseFs> {
        FuseFs::new(self.cfg(timing), self.client("sbx-1"))
    }

    fn lease(&self) -> LeaseResponse {
        self.rt.block_on(self.client("sbx-1").sandbox(None).lease()).unwrap()
    }

    /// A fresh connection (restart = false) or the keeper's (restart = true), with `hooks`.
    fn start(&self, fs: &FuseFs, boot: Uuid, restart: bool, hooks: Hooks) -> Arc<Core> {
        fs.set_hooks(hooks);
        let lease = self.lease();
        let conn = self.sentry.conn();
        self.rt.block_on(fs.serve(conn, boot, restart, &lease)).unwrap()
    }

    fn data(&self, path: &str) -> Option<Vec<u8>> {
        self.magpie.node(self.cid, path).map(|n| n.data)
    }

    fn calls(&self, route: &str) -> usize {
        self.magpie.script().calls_to(route).len()
    }
}

fn fast() -> Timing {
    Timing {
        attempt: Duration::from_secs(5),
        retry_first: Duration::from_millis(20),
        retry_max: Duration::from_millis(100),
        stuck_op: Duration::from_secs(600),
        healthy_within: Duration::from_secs(30),
    }
}

fn ok(r: &super::proto::Reply, what: &str) {
    assert_eq!(r.errno, 0, "{what}: errno {}", r.errno);
}

/// projects/ and projects/-workspace/ exist; returns the -workspace dir's node.
fn workspace_dir(w: &World) -> u64 {
    let s = &w.sentry;
    let r = s.call(op::MKDIR, 1, PID, &body::mkdir("projects", 0o700));
    ok(&r, "mkdir projects");
    let projects = out::node(&r);
    let r = s.call(op::MKDIR, projects, PID, &body::mkdir("-workspace", 0o700));
    ok(&r, "mkdir -workspace");
    out::node(&r)
}

#[test]
fn every_opcode_against_both_scopes() {
    let w = world();
    let fs = w.fs(fast());
    let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;

    let r = s.call(op::INIT, 0, 0, &body::init());
    ok(&r, "init");
    assert_eq!(super::proto::u32_at(&r.payload, 4), 31);

    // magpie scope: dirs, create, write (append), read, stat
    let ws = workspace_dir(&w);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("s.jsonl", O_RDWR | libc::O_CREAT, 0o600),
    );
    ok(&r, "create");
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    for (off, line) in [(0u64, &b"{\"a\":1}\n"[..]), (8, b"{\"b\":2}\n")] {
        let r = s.call(op::WRITE, t, PID, &body::write(fh, off, line));
        ok(&r, "write");
        assert_eq!(super::proto::u32_at(&r.payload, 0), 8);
    }
    assert_eq!(
        w.data("projects/-workspace/s.jsonl").unwrap(),
        b"{\"a\":1}\n{\"b\":2}\n"
    );
    let r = s.call(op::READ, t, PID, &body::read(fh, 15, 1));
    ok(&r, "read last byte");
    assert_eq!(r.payload, b"\n");
    let r = s.call(op::GETATTR, t, PID, &body::getattr());
    ok(&r, "getattr");
    assert_eq!(out::size(&r), 16);
    assert_eq!(out::mode(&r), libc::S_IFREG | 0o600);
    let r = s.call(op::LOOKUP, ws, PID, &body::name("s.jsonl"));
    assert_eq!(out::node(&r), t, "a lookup gives the same node");
    assert_eq!(s.call(op::LOOKUP, ws, PID, &body::name("nope")).errno, libc::ENOENT);

    // a write into a file magpie already holds is read from the cache: no cc/file call
    assert_eq!(w.calls("cc_file"), 0);

    // tombstone: ftruncate then pwrite
    let r = s.call(
        op::SETATTR,
        t,
        PID,
        &body::setattr(super::proto::fattr::SIZE, 8, 0, 0, (0, 0)),
    );
    ok(&r, "truncate");
    assert_eq!(out::size(&r), 8);
    ok(
        &s.call(op::WRITE, t, PID, &body::write(fh, 8, b"{\"c\":3}\n")),
        "pwrite",
    );
    assert_eq!(
        w.data("projects/-workspace/s.jsonl").unwrap(),
        b"{\"a\":1}\n{\"c\":3}\n"
    );
    // mode, mtime (lock refresh by utimes), uid
    let r = s.call(
        op::SETATTR,
        t,
        PID,
        &body::setattr(super::proto::fattr::MODE, 0, 0o640, 0, (0, 0)),
    );
    ok(&r, "chmod");
    assert_eq!(out::mode(&r) & 0o777, 0o640);
    let r = s.call(
        op::SETATTR,
        t,
        PID,
        &body::setattr(super::proto::fattr::MTIME, 0, 0, 0, (1_700_000_000, 5)),
    );
    ok(&r, "utimes");
    assert_eq!(
        w.magpie.node(w.cid, "projects/-workspace/s.jsonl").unwrap().mtime_ns,
        1_700_000_000_000_000_005
    );
    assert_eq!(
        s.call(
            op::SETATTR,
            t,
            PID,
            &body::setattr(super::proto::fattr::UID, 0, 0, 0, (0, 0))
        )
        .errno,
        libc::EPERM
    );
    ok(
        &s.call(
            op::SETATTR,
            t,
            PID,
            &body::setattr(super::proto::fattr::UID, 0, 0, 1000, (0, 0)),
        ),
        "chown 1000",
    );

    // open O_TRUNC → cc/truncate before the reply
    let r = s.call(op::OPEN, t, PID, &body::open(O_RDWR | libc::O_TRUNC));
    ok(&r, "open O_TRUNC");
    assert_eq!(w.data("projects/-workspace/s.jsonl").unwrap(), b"");
    ok(
        &s.call(op::WRITE, t, PID, &body::write(out::open_fh(&r), 0, b"x\n")),
        "write after trunc",
    );

    // compaction: write a tmp lines file, rename over the original
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("s.jsonl.compact.tmp.ab", O_RDWR | libc::O_CREAT, 0o600),
    );
    let tmp = out::node(&r);
    ok(
        &s.call(op::WRITE, tmp, PID, &body::write(out::create_fh(&r), 0, b"c\n")),
        "write tmp",
    );
    ok(
        &s.call(
            op::RENAME,
            ws,
            PID,
            &body::rename(ws, "s.jsonl.compact.tmp.ab", "s.jsonl"),
        ),
        "rename over",
    );
    assert_eq!(w.data("projects/-workspace/s.jsonl").unwrap(), b"c\n");
    assert!(w.data("projects/-workspace/s.jsonl.compact.tmp.ab").is_none());
    let r = s.call(op::LOOKUP, ws, PID, &body::name("s.jsonl"));
    assert_eq!(out::node(&r), tmp, "the renamed node keeps its id");
    let r = s.call(op::READ, tmp, PID, &body::read(0, 0, 100));
    assert_eq!(r.payload, b"c\n");

    // namespace answers from the tree: EEXIST, ENOENT, ENOTEMPTY with no magpie call
    let before = w.calls("cc_mkdir") + w.calls("cc_rmdir") + w.calls("cc_unlink");
    assert_eq!(
        s.call(op::MKDIR, 1, PID, &body::mkdir("projects", 0o700)).errno,
        libc::EEXIST
    );
    let projects = out::node(&s.call(op::LOOKUP, 1, PID, &body::name("projects")));
    assert_eq!(
        s.call(op::RMDIR, projects, PID, &body::name("-workspace")).errno,
        libc::ENOTEMPTY
    );
    assert_eq!(s.call(op::UNLINK, ws, PID, &body::name("missing")).errno, libc::ENOENT);
    assert_eq!(
        s.call(
            op::CREATE,
            ws,
            PID,
            &body::create("s.jsonl", O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600)
        )
        .errno,
        libc::EEXIST
    );
    assert_eq!(w.calls("cc_mkdir") + w.calls("cc_rmdir") + w.calls("cc_unlink"), before);

    // mkdir lock, rmdir, unlink, protected main transcript
    let r = s.call(op::MKDIR, ws, PID, &body::mkdir("s.jsonl.lock", 0o700));
    ok(&r, "mkdir lock");
    assert_eq!(
        s.call(op::MKDIR, ws, PID, &body::mkdir("s.jsonl.lock", 0o700)).errno,
        libc::EEXIST
    );
    ok(&s.call(op::RMDIR, ws, PID, &body::name("s.jsonl.lock")), "rmdir lock");
    w.magpie.set_provider_session(w.cid, "s");
    assert_eq!(s.call(op::UNLINK, ws, PID, &body::name("s.jsonl")).errno, libc::EPERM);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("x.json", O_RDWR | libc::O_CREAT, 0o600),
    );
    ok(&s.call(op::UNLINK, ws, PID, &body::name("x.json")), "unlink");
    // an unlinked open file keeps its attributes
    ok(
        &s.call(op::GETATTR, out::node(&r), PID, &body::getattr()),
        "getattr after unlink",
    );

    // the rest of the table
    assert_eq!(s.call(op::LINK, t, PID, &body::link(t, "l.jsonl")).errno, libc::EPERM);
    assert_eq!(
        s.call(op::SYMLINK, ws, PID, &body::symlink("ln", "s.jsonl")).errno,
        libc::EPERM
    );
    assert_eq!(s.call(op::MKNOD, ws, PID, &body::mkdir("n", 0o600)).errno, libc::EPERM);
    assert_eq!(s.call(op::READLINK, t, PID, &[]).errno, libc::EINVAL);
    ok(&s.call(op::FLUSH, t, PID, &body::fh(0)), "flush");
    ok(&s.call(op::FSYNC, t, PID, &body::fh(0)), "fsync");
    ok(&s.call(op::RELEASE, t, PID, &body::fh(0)), "release");
    assert_eq!(s.call(op::GETXATTR, t, PID, &body::fh(0)).errno, libc::ENODATA);
    ok(&s.call(op::LISTXATTR, t, PID, &body::fh(0)), "listxattr");
    assert_eq!(s.call(op::SETXATTR, t, PID, &body::fh(0)).errno, libc::ENOTSUP);
    assert_eq!(s.call(op::REMOVEXATTR, t, PID, &body::fh(0)).errno, libc::ENOTSUP);
    assert_eq!(s.call(op::FALLOCATE, t, PID, &body::fh(0)).errno, libc::EOPNOTSUPP);
    assert_eq!(s.call(op::GETLK, t, PID, &body::fh(0)).errno, libc::ENOSYS);
    ok(&s.call(op::ACCESS, t, PID, &body::fh(0)), "access");
    let r = s.call(op::STATFS, 1, PID, &[]);
    ok(&r, "statfs");
    assert_eq!(super::proto::u64_at(&r.payload, 0), (2u64 << 30) / 4096);
    ok(&s.call(op::OPENDIR, ws, PID, &body::open(0)), "opendir");
    let r = s.call(op::READDIR, ws, PID, &body::readdir(0));
    assert_eq!(out::names(&r), [".", "..", "s.jsonl"]);
    ok(&s.call(op::RELEASEDIR, ws, PID, &body::fh(0)), "releasedir");

    // refused paths: absent, a create is EACCES
    assert_eq!(
        s.call(op::LOOKUP, 1, PID, &body::name("settings.json")).errno,
        libc::ENOENT
    );
    assert_eq!(
        s.call(
            op::CREATE,
            1,
            PID,
            &body::create("settings.json", O_RDWR | libc::O_CREAT, 0o600)
        )
        .errno,
        libc::EACCES
    );
    assert_eq!(
        s.call(op::MKDIR, 1, PID, &body::mkdir("skills", 0o700)).errno,
        libc::EACCES
    );
    assert_eq!(s.call(op::MKDIR, 1, PID, &body::mkdir("..", 0o700)).errno, libc::EINVAL);

    // local scope: passthrough, nothing reaches magpie
    let writes = w.calls("cc_write");
    let r = s.call(op::MKDIR, 1, PID, &body::mkdir("sessions", 0o700));
    ok(&r, "mkdir sessions");
    let sessions = out::node(&r);
    let r = s.call(
        op::CREATE,
        sessions,
        PID,
        &body::create("12.json", O_RDWR | libc::O_CREAT, 0o600),
    );
    ok(&r, "local create");
    let (lf, lfh) = (out::node(&r), out::create_fh(&r));
    ok(
        &s.call(op::WRITE, lf, PID, &body::write(lfh, 0, b"{\"pid\":12}")),
        "local write",
    );
    assert_eq!(
        s.call(op::READ, lf, PID, &body::read(lfh, 0, 100)).payload,
        b"{\"pid\":12}"
    );
    assert_eq!(out::size(&s.call(op::GETATTR, lf, PID, &body::getattr())), 10);
    ok(
        &s.call(
            op::SETATTR,
            lf,
            PID,
            &body::setattr(super::proto::fattr::SIZE, 3, 0, 0, (0, 0)),
        ),
        "local truncate",
    );
    ok(
        &s.call(
            op::SETATTR,
            lf,
            PID,
            &body::setattr(super::proto::fattr::MTIME, 0, 0, 0, (1_700_000_000, 0)),
        ),
        "local utimes",
    );
    ok(&s.call(op::FSYNC, lf, PID, &body::fh(lfh)), "local fsync");
    ok(
        &s.call(op::SYMLINK, sessions, PID, &body::symlink("ln", "12.json")),
        "local symlink",
    );
    let ln = out::node(&s.call(op::LOOKUP, sessions, PID, &body::name("ln")));
    assert_eq!(s.call(op::READLINK, ln, PID, &[]).payload, b"12.json");
    ok(
        &s.call(op::RENAME, sessions, PID, &body::rename(sessions, "12.json", "13.json")),
        "local rename",
    );
    assert_eq!(s.call(op::LOOKUP, sessions, PID, &body::name("13.json")).errno, 0);
    assert_eq!(
        s.call(op::MKNOD, sessions, PID, &body::mkdir("n", 0o600)).errno,
        libc::EPERM
    );
    assert_eq!(s.call(op::LINK, lf, PID, &body::link(lf, "h")).errno, libc::EPERM);
    let r = s.call(op::READDIR, sessions, PID, &body::readdir(0));
    assert_eq!(out::names(&r), [".", "..", "13.json", "ln"]);
    ok(&s.call(op::UNLINK, sessions, PID, &body::name("ln")), "local unlink");
    ok(&s.call(op::RELEASE, lf, PID, &body::fh(lfh)), "local release");
    ok(
        &s.call(op::MKDIR, sessions, PID, &body::mkdir("d", 0o700)),
        "local mkdir",
    );
    ok(&s.call(op::RMDIR, sessions, PID, &body::name("d")), "local rmdir");
    assert_eq!(w.calls("cc_write"), writes, "local scope never reaches magpie");
    assert!(w.dir.join("local/sessions/13.json").exists());
    // an unclassified name is local
    ok(
        &s.call(op::MKDIR, 1, PID, &body::mkdir("brand-new", 0o700)),
        "unclassified",
    );
    // across scopes: EXDEV
    assert_eq!(
        s.call(op::RENAME, sessions, PID, &body::rename(ws, "13.json", "13.json"))
            .errno,
        libc::EXDEV
    );
    // the root lists the union
    let r = s.call(op::READDIR, 1, PID, &body::readdir(0));
    assert_eq!(out::names(&r), [".", "..", "brand-new", "projects", "sessions"]);
    assert_eq!(
        s.call(op::BATCH_FORGET + 50, 1, PID, &[]).errno,
        libc::ENOSYS,
        "{}",
        opname(op::BATCH_FORGET + 50)
    );
}

/// 9.9 / 22 item 4: a "kill" at each fault point; the next instance (the keeper's connection) replays the slot.
#[test]
fn journal_replay_at_each_fault_point() {
    for point in [
        Point::ReadReturned,
        Point::CallSent,
        Point::CallAnswered,
        Point::ReplyWritten,
    ] {
        for opcode in [op::WRITE, op::MKDIR, op::CREATE, op::RENAME] {
            let w = world();
            let fs = w.fs(fast());
            let boot = Uuid::new_v4();
            super::store_boot_id(&w.dir.join("state"), boot).unwrap();
            let armed = Arc::new(AtomicBool::new(false));
            let a = armed.clone();
            let hooks = Hooks {
                fault: Some(Arc::new(move |p, h| {
                    p == point && h.opcode == opcode && a.swap(false, Ordering::SeqCst)
                })),
                ..Hooks::default()
            };
            let core = w.start(&fs, boot, false, hooks);
            let s = &w.sentry;
            let ws = workspace_dir(&w);
            let r = s.call(
                op::CREATE,
                ws,
                PID,
                &body::create("t.jsonl", O_RDWR | libc::O_CREAT, 0o600),
            );
            let (t, fh) = (out::node(&r), out::create_fh(&r));
            ok(&s.call(op::WRITE, t, PID, &body::write(fh, 0, b"one\n")), "first write");
            armed.store(true, Ordering::SeqCst);
            let b = match opcode {
                op::WRITE => body::write(fh, 4, b"two\n"),
                op::MKDIR => body::mkdir("t.lock", 0o700),
                op::CREATE => body::create("n.json", O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600),
                _ => body::rename(ws, "t.jsonl", "u.jsonl"),
            };
            let node = if opcode == op::WRITE { t } else { ws };
            let u = s.submit(opcode, node, PID, &b);
            for _ in 0..500 {
                if core.died() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(core.died(), "{point:?} {}: the fault fired", opname(opcode));
            // the next supervisor: same connection, same bootId, replays the journal
            let _core2 = w.start(&fs, boot, true, Hooks::default());
            let r = s
                .wait(u, Duration::from_secs(10))
                .expect("the caller's syscall completes");
            assert_eq!(
                r.errno,
                0,
                "{point:?} {}: one success, not EEXIST/ENOENT",
                opname(opcode)
            );
            match opcode {
                op::WRITE => {
                    assert_eq!(w.data("projects/-workspace/t.jsonl").unwrap(), b"one\ntwo\n");
                    assert!(w.calls("cc_write") >= 3);
                }
                op::MKDIR => assert!(w.magpie.node(w.cid, "projects/-workspace/t.lock").is_some()),
                op::CREATE => assert!(w.data("projects/-workspace/n.json").is_some()),
                _ => assert!(w.data("projects/-workspace/u.jsonl").is_some()),
            }
            if point == Point::ReplyWritten {
                assert_eq!(
                    s.refused(u),
                    1,
                    "the replay's reply is refused: it was delivered before the kill"
                );
            }
            // node ids survive: the file is still the same node
            let name = if opcode == op::RENAME { "u.jsonl" } else { "t.jsonl" };
            assert_eq!(out::node(&s.call(op::LOOKUP, ws, PID, &body::name(name))), t);
        }
    }
}

/// 9.3: a checkpoint forgets requests in flight and the sentry resends them under a new `unique` from the same
/// thread, with no INTERRUPT. Each op is applied once and both callers' syscalls get the original result, whether
/// the resend arrives while the original is still in flight or after its reply was refused with ENOENT.
#[test]
fn resends_after_a_checkpoint_apply_once() {
    for after_refusal in [false, true] {
        for opcode in [op::WRITE, op::CREATE, op::MKDIR, op::RENAME] {
            let w = world();
            let fs = w.fs(fast());
            let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
            let s = &w.sentry;
            let ws = workspace_dir(&w);
            let r = s.call(
                op::CREATE,
                ws,
                PID,
                &body::create("t.jsonl", O_RDWR | libc::O_CREAT, 0o600),
            );
            let (t, fh) = (out::node(&r), out::create_fh(&r));
            ok(&s.call(op::WRITE, t, PID, &body::write(fh, 0, b"one\n")), "first write");
            let (route, b, node) = match opcode {
                op::WRITE => ("cc_write", body::write(fh, 4, b"two\n"), t),
                op::CREATE => (
                    "cc_write",
                    body::create("n.json", O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600),
                    ws,
                ),
                op::MKDIR => ("cc_mkdir", body::mkdir("t.lock", 0o700), ws),
                _ => ("cc_rename", body::rename(ws, "t.jsonl", "u.jsonl"), ws),
            };
            let hold = Hold::new();
            let f = w.magpie.faults().times(route, FaultKind::Hold(hold.clone()), 1);
            let before = w.calls(route);
            let u1 = s.submit(opcode, node, PID, &b);
            assert!(w.rt.block_on(hold.wait_held(1, Duration::from_secs(5))));
            s.forget(u1); // the checkpoint
            let u2 = if after_refusal {
                hold.release();
                assert!(
                    s.wait_refused(u1, Duration::from_secs(5)),
                    "the original's reply is refused"
                );
                s.submit(opcode, node, PID, &b)
            } else {
                let u2 = s.submit(opcode, node, PID, &b);
                std::thread::sleep(Duration::from_millis(100));
                hold.release();
                u2
            };
            let r = s.wait(u2, Duration::from_secs(10)).expect("the resend completes");
            assert_eq!(
                r.errno,
                0,
                "{} (after refusal {after_refusal}): the original result",
                opname(opcode)
            );
            w.magpie.faults().remove(f);
            assert_eq!(w.calls(route) - before, 1, "{}: one magpie operation", opname(opcode));
            if opcode == op::WRITE {
                assert_eq!(w.data("projects/-workspace/t.jsonl").unwrap(), b"one\ntwo\n");
            }
            // the thread's next, different request is served normally
            ok(
                &s.call(op::MKDIR, ws, PID, &body::mkdir("after", 0o700)),
                "next request",
            );
        }
    }
}

#[test]
fn a_general_outage_blocks_without_eio() {
    let w = world();
    let fs = w.fs(fast());
    let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("t.jsonl", O_RDWR | libc::O_CREAT, 0o600),
    );
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    let f = w
        .magpie
        .faults()
        .everywhere(FaultKind::Unavailable { retry_after_secs: 0 });
    let u = s.submit(op::WRITE, t, PID, &body::write(fh, 0, b"x\n"));
    assert!(s.wait(u, Duration::from_millis(800)).is_none(), "blocked, not failed");
    assert_eq!(fs.remote().blocked().0, 1);
    w.magpie.faults().remove(f);
    let r = s.wait(u, Duration::from_secs(5)).unwrap();
    assert_eq!(r.errno, 0);
    assert_eq!(w.data("projects/-workspace/t.jsonl").unwrap(), b"x\n");
}

#[test]
fn one_stuck_op_is_eio_and_write_stuck() {
    let w = world();
    let mut timing = fast();
    timing.stuck_op = Duration::from_millis(400);
    let fs = w.fs(timing);
    let stuck = Arc::new(Mutex::new(Vec::new()));
    let st = stuck.clone();
    let hooks = Hooks {
        write_stuck: Arc::new(move |s| st.lock().unwrap().push(s)),
        ..Hooks::default()
    };
    let _core = w.start(&fs, Uuid::new_v4(), false, hooks);
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("t.jsonl", O_RDWR | libc::O_CREAT, 0o600),
    );
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    let f = w
        .magpie
        .faults()
        .on("cc_write", FaultKind::Unavailable { retry_after_secs: 0 });
    // the heartbeat keeps succeeding (WP-23's records its successes)
    let alive = Arc::new(AtomicBool::new(true));
    let (a, remote) = (alive.clone(), fs.remote().clone());
    let hb = std::thread::spawn(move || {
        while a.load(Ordering::SeqCst) {
            remote.record_ok();
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let r = s.call(op::WRITE, t, PID, &body::write(fh, 0, b"x\n"));
    alive.store(false, Ordering::SeqCst);
    hb.join().unwrap();
    assert_eq!(r.errno, libc::EIO);
    let got = stuck.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].what, "cc/write");
    w.magpie.faults().remove(f);
}

#[test]
fn offset_repair_from_the_cache() {
    let w = world();
    let fs = w.fs(fast());
    let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("t.jsonl", O_RDWR | libc::O_CREAT, 0o600),
    );
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    ok(&s.call(op::WRITE, t, PID, &body::write(fh, 0, b"one\n")), "1");
    ok(&s.call(op::WRITE, t, PID, &body::write(fh, 4, b"two\n")), "2");
    // magpie loses the acknowledged second write (a restore)
    w.magpie.seed_file(w.cid, "projects/-workspace/t.jsonl", b"one\n");
    ok(
        &s.call(op::WRITE, t, PID, &body::write(fh, 8, b"three\n")),
        "3 after the repair",
    );
    assert_eq!(w.data("projects/-workspace/t.jsonl").unwrap(), b"one\ntwo\nthree\n");
    // a fork: magpie's bytes are not a prefix of the cache → EIO, the agent stopped
    w.magpie.seed_file(w.cid, "projects/-workspace/t.jsonl", b"ONE\n");
    assert_eq!(
        s.call(op::WRITE, t, PID, &body::write(fh, 14, b"four\n")).errno,
        libc::EIO
    );
}

#[test]
fn lease_lost_and_not_current_without_a_restart_loop() {
    let w = world();
    let fs = w.fs(fast());
    let lost = Arc::new(AtomicUsize::new(0));
    let regained = Arc::new(AtomicUsize::new(0));
    let (l, g) = (lost.clone(), regained.clone());
    let hooks = Hooks {
        lease_lost: Arc::new(move || {
            l.fetch_add(1, Ordering::SeqCst);
        }),
        lease_regained: Arc::new(move |_| {
            g.fetch_add(1, Ordering::SeqCst);
        }),
        ..Hooks::default()
    };
    let _core = w.start(&fs, Uuid::new_v4(), false, hooks);
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    // another grant makes ours dead
    let _ = w.lease();
    assert_eq!(s.call(op::MKDIR, ws, PID, &body::mkdir("a", 0o700)).errno, libc::EIO);
    assert_eq!(lost.load(Ordering::SeqCst), 1);
    // the lease loop takes a new one (no exit, no restart)
    for _ in 0..200 {
        if regained.load(Ordering::SeqCst) == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(regained.load(Ordering::SeqCst), 1);
    ok(
        &s.call(op::MKDIR, ws, PID, &body::mkdir("a", 0o700)),
        "after the new lease",
    );
    // now hermit names another sandbox: lost again, and the loop waits (30 s per try) instead of stealing it
    w.magpie.set_current_sandbox(w.cid, Some("sbx-other"));
    let _ = w.rt.block_on(w.client("sbx-other").sandbox(None).lease()).unwrap();
    let leases = w.calls("lease");
    assert_eq!(s.call(op::MKDIR, ws, PID, &body::mkdir("b", 0o700)).errno, libc::EIO);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(lost.load(Ordering::SeqCst), 2);
    assert_eq!(regained.load(Ordering::SeqCst), 1);
    assert_eq!(
        w.calls("lease") - leases,
        1,
        "one refused try, then a 30 s wait: no loop"
    );
    assert_eq!(
        s.call(op::LOOKUP, ws, PID, &body::name("a")).errno,
        libc::EIO,
        "unleased: magpie scope is EIO"
    );
    assert_eq!(
        s.call(op::READDIR, 1, PID, &body::readdir(0)).errno,
        0,
        "the root still answers"
    );
}

#[test]
fn boot_id_per_connection() {
    let dir = std::env::temp_dir().join(format!("boot-{}", Uuid::new_v4()));
    assert!(matches!(
        super::boot_id_for(&dir, true),
        Err(super::MountError::BootIdUnreadable(_))
    ));
    let a = super::boot_id_for(&dir, false).unwrap();
    assert_eq!(
        super::boot_id_for(&dir, true).unwrap(),
        a,
        "the keeper's connection keeps it"
    );
    let b = super::boot_id_for(&dir, false).unwrap();
    assert_ne!(a, b, "a fresh mount draws a new one");
    std::fs::write(dir.join("boot_id"), "garbage").unwrap();
    assert!(matches!(
        super::boot_id_for(&dir, true),
        Err(super::MountError::BootIdUnreadable(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_interrupt_before_any_attempt_is_eintr() {
    let w = world();
    let fs = w.fs(fast());
    let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let f = w
        .magpie
        .faults()
        .on("cc_mkdir", FaultKind::Unavailable { retry_after_secs: 0 });
    let u = s.submit(op::MKDIR, ws, PID, &body::mkdir("x", 0o700));
    std::thread::sleep(Duration::from_millis(100));
    s.submit(op::INTERRUPT, 0, PID, &body::interrupt(u));
    let r = s.wait(u, Duration::from_secs(5)).unwrap();
    assert_eq!(r.errno, libc::EINTR, "magpie refused every attempt unexecuted: EINTR");
    w.magpie.faults().remove(f);
    ok(
        &s.call(op::MKDIR, ws, PID, &body::mkdir("x", 0o700)),
        "the retry is served on its own",
    );
}
