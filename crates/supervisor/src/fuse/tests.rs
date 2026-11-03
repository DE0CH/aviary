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
    /// Shut down with a bound in `drop` (a plain drop waits for every blocking task, which a test that failed while a
    /// start retried through an outage would leave running for ever).
    rt: std::mem::ManuallyDrop<tokio::runtime::Runtime>,
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
        rt: std::mem::ManuallyDrop::new(rt),
        magpie,
        served,
        dir,
        cid: Uuid::now_v7(),
        sentry: Sentry::new(),
    }
}

/// Bounds every FUSE test's teardown, also after a failed assertion: the sentry's handles close (the workers' reads
/// end), the runtime gets [`TEARDOWN`] for its blocking tasks, and the directory goes.
impl Drop for World {
    fn drop(&mut self) {
        self.sentry.close_all();
        // SAFETY: `rt` is taken once, here, and never used again (the struct is being dropped).
        #[allow(unsafe_code)]
        let rt = unsafe { std::mem::ManuallyDrop::take(&mut self.rt) };
        rt.shutdown_timeout(TEARDOWN);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How long a test's teardown waits for the runtime's blocking tasks.
const TEARDOWN: Duration = Duration::from_secs(5);

/// The bound on a start in these tests (a start retries through magpie's unavailability for ever).
const START_BOUND: Duration = Duration::from_secs(30);

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
        self.serve(fs, self.sentry.conn(), boot, restart, &lease).unwrap()
    }

    /// `fs.serve` on `dev`, failing the test after [`START_BOUND`].
    fn serve(
        &self,
        fs: &FuseFs,
        dev: Arc<dyn super::Device>,
        boot: Uuid,
        restart: bool,
        lease: &LeaseResponse,
    ) -> std::io::Result<Arc<Core>> {
        self.rt
            .block_on(async { tokio::time::timeout(START_BOUND, fs.serve(dev, boot, restart, lease)).await })
            .expect("the start finishes in time")
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

/// The distinct `opId`s magpie received on `route` (query or JSON body).
fn op_ids(w: &World, route: &str) -> std::collections::BTreeSet<String> {
    w.magpie
        .script()
        .calls_to(route)
        .iter()
        .filter_map(|c| {
            c.query
                .as_ref()
                .and_then(|q| q.get("opId"))
                .or_else(|| c.body.as_ref().and_then(|b| b.get("opId")))
                .map(ToString::to_string)
        })
        .collect()
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

    // every opcode of the table against a file and a directory of each scope
    let lpath = "sessions/13.json";
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("o.json", O_RDWR | libc::O_CREAT, 0o600),
    );
    ok(&r, "create o.json");
    let mf = out::node(&r);
    for (scope, file, dir, name) in [("magpie", mf, ws, "o.json"), ("local", lf, sessions, "13.json")] {
        let what = |o: u32| format!("{scope} {}", opname(o));
        let r = s.call(op::OPEN, file, PID, &body::open(O_RDWR | libc::O_APPEND));
        ok(&r, &what(op::OPEN));
        let h = out::open_fh(&r);
        ok(&s.call(op::FLUSH, file, PID, &body::fh(h)), &what(op::FLUSH));
        ok(&s.call(op::FSYNC, file, PID, &body::fh(h)), &what(op::FSYNC));
        ok(&s.call(op::GETATTR, file, PID, &body::getattr()), &what(op::GETATTR));
        ok(&s.call(op::ACCESS, file, PID, &body::fh(0)), &what(op::ACCESS));
        assert_eq!(
            s.call(op::GETXATTR, file, PID, &body::fh(0)).errno,
            libc::ENODATA,
            "{}",
            what(op::GETXATTR)
        );
        ok(&s.call(op::LISTXATTR, file, PID, &body::fh(0)), &what(op::LISTXATTR));
        for o in [op::SETXATTR, op::REMOVEXATTR] {
            assert_eq!(s.call(o, file, PID, &body::fh(0)).errno, libc::ENOTSUP, "{}", what(o));
        }
        assert_eq!(
            s.call(op::FALLOCATE, file, PID, &body::fh(h)).errno,
            libc::EOPNOTSUPP,
            "{}",
            what(op::FALLOCATE)
        );
        for o in [op::GETLK, op::SETLK, op::SETLKW, op::READDIRPLUS] {
            assert_eq!(s.call(o, file, PID, &body::fh(h)).errno, libc::ENOSYS, "{}", what(o));
        }
        assert_eq!(s.call(op::LINK, file, PID, &body::link(file, "h")).errno, libc::EPERM);
        ok(&s.call(op::RELEASE, file, PID, &body::fh(h)), &what(op::RELEASE));
        ok(&s.call(op::OPENDIR, dir, PID, &body::open(0)), &what(op::OPENDIR));
        ok(&s.call(op::READDIR, dir, PID, &body::readdir(0)), &what(op::READDIR));
        ok(&s.call(op::FSYNCDIR, dir, PID, &body::fh(0)), &what(op::FSYNCDIR));
        ok(&s.call(op::RELEASEDIR, dir, PID, &body::fh(0)), &what(op::RELEASEDIR));
        // RENAME2 without flags renames; NOREPLACE/EXCHANGE are refused (2.5)
        ok(
            &s.call(op::RENAME2, dir, PID, &body::rename2(dir, 0, name, "r2")),
            &what(op::RENAME2),
        );
        assert_eq!(out::node(&s.call(op::LOOKUP, dir, PID, &body::name("r2"))), file);
        assert_eq!(
            s.call(op::RENAME2, dir, PID, &body::rename2(dir, 1, "r2", name)).errno,
            libc::EINVAL,
            "{}",
            what(op::RENAME2)
        );
        ok(
            &s.call(op::RENAME2, dir, PID, &body::rename2(dir, 0, "r2", name)),
            &what(op::RENAME2),
        );
        // FORGET and BATCH_FORGET get no reply; the next request is served
        s.submit(op::FORGET, file, PID, &1u64.to_le_bytes());
        s.submit(op::BATCH_FORGET, 0, PID, &[0u8; 8]);
        assert_eq!(
            out::node(&s.call(op::LOOKUP, dir, PID, &body::name(name))),
            file,
            "{scope} after FORGET"
        );
    }
    assert!(w.dir.join("local").join(lpath).exists());
    assert!(w.data("projects/-workspace/o.json").is_some());
    assert_eq!(
        s.call(op::BATCH_FORGET + 50, 1, PID, &[]).errno,
        libc::ENOSYS,
        "{}",
        opname(op::BATCH_FORGET + 50)
    );
    ok(&s.call(op::DESTROY, 0, PID, &[]), "destroy");
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
            // the previous request's worker has passed its own fault points (it replies before reply-written)
            assert!(core.settle(Duration::from_secs(5)));
            armed.store(true, Ordering::SeqCst);
            let b = match opcode {
                op::WRITE => body::write(fh, 4, b"two\n"),
                op::MKDIR => body::mkdir("t.lock", 0o700),
                op::CREATE => body::create("n.json", O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600),
                _ => body::rename(ws, "t.jsonl", "u.jsonl"),
            };
            let node = if opcode == op::WRITE { t } else { ws };
            let route = match opcode {
                op::WRITE | op::CREATE => "cc_write",
                op::MKDIR => "cc_mkdir",
                _ => "cc_rename",
            };
            let ids_before = op_ids(&w, route);
            let u = s.submit(opcode, node, PID, &b);
            for _ in 0..500 {
                if core.died() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(core.died(), "{point:?} {}: the fault fired", opname(opcode));
            let ids_killed = op_ids(&w, route);
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
            // magpie's rule 0 would hide a second application of the same bytes: the replay must reuse the derived
            // opId, so the request names exactly one opId whether or not the killed instance sent it
            let ids_after = op_ids(&w, route);
            let new: Vec<_> = ids_after.difference(&ids_before).collect();
            assert_eq!(
                new.len(),
                1,
                "{point:?} {}: one opId for the request: {new:?}",
                opname(opcode)
            );
            if point != Point::ReadReturned {
                assert_eq!(
                    ids_killed.difference(&ids_before).count(),
                    1,
                    "{point:?} {}: the killed instance sent it",
                    opname(opcode)
                );
                assert!(
                    ids_killed.is_superset(&ids_after),
                    "{point:?} {}: the replay reused it",
                    opname(opcode)
                );
            }
            if point == Point::ReplyWritten {
                assert_eq!(
                    s.refused(u),
                    1,
                    "{}: the replay's reply is refused: it was delivered before the kill",
                    opname(opcode)
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

/// 9.5 item 4 at a start (WP-22 review 3's open item): a write replayed from the journal that magpie holds, while
/// it answers everything else, ends in EIO and `cc.write_stuck` after the stuck time, though the heartbeat (4.3
/// step 7) has not started: the start's health probe records magpie's answers, and the start then completes.
#[test]
fn a_replayed_write_magpie_holds_is_stuck_without_the_heartbeat() {
    let w = world();
    let mut timing = fast();
    timing.attempt = Duration::from_millis(200);
    timing.stuck_op = Duration::from_millis(800);
    timing.healthy_within = Duration::from_millis(300);
    let fs = w.fs(timing);
    let boot = Uuid::new_v4();
    super::store_boot_id(&w.dir.join("state"), boot).unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let a = armed.clone();
    let hooks = Hooks {
        fault: Some(Arc::new(move |p, h| {
            p == Point::ReadReturned && h.opcode == op::WRITE && a.swap(false, Ordering::SeqCst)
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
    assert!(core.settle(Duration::from_secs(5)));
    armed.store(true, Ordering::SeqCst);
    let u = s.submit(op::WRITE, t, PID, &body::write(fh, 0, b"x\n"));
    wait_for("the fault", || core.died());
    // magpie holds every cc write from now on and answers the rest; no heartbeat runs (nothing calls record_ok)
    let hold = Hold::new();
    let f = w.magpie.faults().on("cc_write", FaultKind::Hold(hold.clone()));
    let stuck = Arc::new(Mutex::new(Vec::new()));
    let st = stuck.clone();
    let hooks = Hooks {
        write_stuck: Arc::new(move |o| st.lock().unwrap().push(o)),
        ..Hooks::default()
    };
    let t0 = std::time::Instant::now();
    let core2 = w.start(&fs, boot, true, hooks);
    let r = s
        .wait(u, Duration::from_secs(10))
        .expect("the replayed write is answered");
    assert_eq!(r.errno, libc::EIO, "stuck: EIO");
    assert!(t0.elapsed() >= Duration::from_millis(800), "{:?}", t0.elapsed());
    let got = stuck.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].what, "cc/write");
    assert_eq!(core2.stuck_count(), 1);
    assert!(w.calls("cc_tree") >= 3, "the probe asked magpie");
    // the start went on: a new request is served
    ok(
        &s.call(op::GETATTR, 1, PID, &body::getattr()),
        "getattr after the start",
    );
    w.magpie.faults().remove(f);
    hold.release();
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
fn the_quota_is_edquot_and_reported() {
    let w = world();
    let fs = w.fs(fast());
    let core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("t.jsonl", O_RDWR | libc::O_CREAT, 0o600),
    );
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    w.magpie.set_quota(4);
    assert_eq!(
        s.call(op::WRITE, t, PID, &body::write(fh, 0, b"12345\n")).errno,
        libc::EDQUOT
    );
    assert!(core.quota_exceeded());
    w.magpie.set_quota(1 << 30);
    ok(
        &s.call(op::WRITE, t, PID, &body::write(fh, 0, b"12345\n")),
        "under the quota",
    );
    assert!(!core.quota_exceeded());
    // a line over the cap (the fake refuses a body over 16 MiB with too_large) would be EFBIG: the map is unit-tested
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

/// The wire format against a real kernel FUSE client (root only; ci.yml runs it under sudo, as the dockerd test):
/// a fresh connection mounted with supervisor 2's options, then ordinary syscalls from another thread on both
/// scopes. The sandbox runtime's client is the target; the kernel's speaks the same protocol (it offers a newer minor
/// and the reply's 7.31 is accepted).
#[test]
fn a_kernel_mount_serves_real_syscalls() {
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    // SAFETY: geteuid(2) has no arguments.
    #[allow(unsafe_code)]
    let root = unsafe { libc::geteuid() } == 0;
    if !root || !std::path::Path::new("/dev/fuse").exists() {
        eprintln!("kernel fuse test skipped: not root or no /dev/fuse");
        return;
    }
    let w = world();
    let fs = w.fs(fast());
    let mnt = w.dir.join("mnt");
    let fd = super::mount_fresh(&mnt).unwrap();
    // dropped before `w`: whatever happens below, the connection is aborted and the mount detached
    let mount = KernelMount::new(&mnt);
    let lease = w.lease();
    let core = w
        .serve(&fs, Arc::new(super::FdDevice(fd)), Uuid::new_v4(), false, &lease)
        .unwrap();
    let m = mnt.clone();
    let worker = std::thread::spawn(move || {
        let ws = m.join("projects/-workspace");
        std::fs::create_dir_all(&ws).unwrap();
        let t = ws.join("s.jsonl");
        for line in ["{\"a\":1}\n", "{\"b\":2}\n"] {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&t)
                .unwrap();
            f.write_all(line.as_bytes()).unwrap();
        }
        let mut s = String::new();
        std::fs::File::open(&t).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "{\"a\":1}\n{\"b\":2}\n");
        let md = std::fs::metadata(&t).unwrap();
        assert_eq!(md.len(), 16);
        std::fs::rename(&t, ws.join("r.jsonl")).unwrap();
        assert!(std::fs::create_dir(ws.join("r.jsonl.lock")).is_ok());
        assert_eq!(
            std::fs::create_dir(ws.join("r.jsonl.lock")).unwrap_err().raw_os_error(),
            Some(libc::EEXIST)
        );
        std::fs::remove_dir(ws.join("r.jsonl.lock")).unwrap();
        let names: Vec<String> = std::fs::read_dir(&ws)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["r.jsonl"]);
        std::fs::create_dir_all(m.join("sessions")).unwrap();
        std::fs::write(m.join("sessions/1.json"), b"{}").unwrap();
        assert_eq!(std::fs::read(m.join("sessions/1.json")).unwrap(), b"{}");
        assert_eq!(
            std::fs::write(m.join("settings.json"), b"{}")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
        assert_eq!(
            std::fs::hard_link(ws.join("r.jsonl"), ws.join("h.jsonl"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
        std::fs::remove_file(ws.join("r.jsonl")).unwrap();
    });
    // the syscalls get a bound: past it the connection is aborted, so each pending one fails and the worker ends
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !worker.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let hung = !worker.is_finished();
    if hung {
        mount.abort();
    }
    let r = worker.join();
    mount.detach();
    core.stop();
    assert!(!hung, "the syscalls on the kernel mount gave no answer in 60 s");
    r.unwrap();
    assert!(w.dir.join("local/sessions/1.json").exists());
    assert!(w.data("projects/-workspace/r.jsonl").is_none());
    assert!(w.magpie.node(w.cid, "projects/-workspace").is_some());
    eprintln!("kernel fuse mount served real syscalls");
}

/// A test's kernel FUSE mount. Dropped (at the end, or by a failed assertion) it aborts its connection if it is
/// still mounted, so every request still pending in the kernel fails at once and nothing can wait on a server that is
/// gone, and detaches it. Only the test's own path is touched (never a fusectl connection number, which another
/// mount on the host could hold).
struct KernelMount {
    path: std::ffi::CString,
}

impl KernelMount {
    fn new(mnt: &std::path::Path) -> Self {
        Self {
            path: std::ffi::CString::new(mnt.to_string_lossy().as_bytes()).unwrap(),
        }
    }

    fn umount(&self, flags: libc::c_int) {
        // SAFETY: umount2(2) of a NUL-terminated path; an error (already unmounted) is ignored.
        #[allow(unsafe_code)]
        unsafe {
            libc::umount2(self.path.as_ptr(), flags);
        }
    }

    /// Aborts the connection (a forced unmount: FUSE's `umount_begin` aborts it) and detaches the mount; nothing
    /// when it is no longer mounted (EINVAL).
    fn abort(&self) {
        self.umount(libc::MNT_FORCE | libc::MNT_DETACH);
    }

    fn detach(&self) {
        self.umount(libc::MNT_DETACH);
    }
}

impl Drop for KernelMount {
    fn drop(&mut self) {
        self.abort();
    }
}

/// 8.2: a turn's end waits until magpie acknowledged the writes holding its uuids; a restarted server finds the
/// uuids written before it in the main transcript's tail; the stuck rule ends the wait.
#[test]
fn the_end_report_waits_for_the_uuids_magpie_acknowledged() {
    use crate::cc::{Stored, Transcript as _};
    let w = world();
    let fs = w.fs(fast());
    let boot = Uuid::new_v4();
    super::store_boot_id(&w.dir.join("state"), boot).unwrap();
    let core = w.start(&fs, boot, false, Hooks::default());
    core.set_main_transcript(Some("projects/-workspace/s.jsonl".into()));
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("s.jsonl", O_RDWR | libc::O_CREAT, 0o600),
    );
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    let (a, b) = (Uuid::now_v7().to_string(), Uuid::now_v7().to_string());
    let la = format!("{{\"type\":\"user\",\"uuid\":\"{a}\"}}\n");
    ok(&s.call(op::WRITE, t, PID, &body::write(fh, 0, la.as_bytes())), "line a");
    let wait = |u: Vec<String>, d: Duration| {
        w.rt.block_on(async { tokio::time::timeout(d, fs.wait_stored(u)).await })
            .ok()
    };
    assert_eq!(wait(vec![a.clone()], Duration::from_secs(5)), Some(Stored::All));
    assert_eq!(
        wait(vec![a.clone(), b.clone()], Duration::from_millis(1500)),
        None,
        "b is not written yet"
    );
    // b's line arrives in two writes while the wait runs
    let lb = format!("{{\"type\":\"assistant\",\"uuid\":\"{b}\"}}\n");
    let (b1, b2) = lb.as_bytes().split_at(20);
    let off = la.len() as u64;
    let waiting = w.rt.spawn({
        let fs = fs.clone();
        let u = vec![a.clone(), b.clone()];
        async move { fs.wait_stored(u).await }
    });
    std::thread::sleep(Duration::from_millis(200));
    ok(&s.call(op::WRITE, t, PID, &body::write(fh, off, b1)), "b, first half");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!waiting.is_finished());
    ok(
        &s.call(op::WRITE, t, PID, &body::write(fh, off + b1.len() as u64, b2)),
        "b, second half",
    );
    let r =
        w.rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), waiting).await });
    assert_eq!(r.unwrap().unwrap(), Stored::All);
    // a restarted server has not seen the writes: it finds them in what magpie holds
    core.stop();
    let core2 = w.start(&fs, boot, true, Hooks::default());
    core2.set_main_transcript(Some("projects/-workspace/s.jsonl".into()));
    assert_eq!(wait(vec![b.clone()], Duration::from_secs(5)), Some(Stored::All));
    // a uuid never written, then the stuck rule fires: the wait ends Stuck
    let c = vec![Uuid::now_v7().to_string()];
    let waiting = w.rt.spawn({
        let fs = fs.clone();
        async move { fs.wait_stored(c).await }
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(!waiting.is_finished());
    core2.stuck_for_test();
    let r =
        w.rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), waiting).await });
    assert_eq!(r.unwrap().unwrap(), Stored::Stuck);
}

/// The node of a path that exists in magpie (LOOKUP from the root).
fn lookup(w: &World, path: &str) -> u64 {
    let mut node = 1;
    for part in path.split('/') {
        let r = w.sentry.call(op::LOOKUP, node, PID, &body::name(part));
        ok(&r, &format!("lookup {part}"));
        node = out::node(&r);
    }
    node
}

fn lines(n: usize) -> Vec<u8> {
    let line = b"{\"x\":\"0123456789012345678901234567890123456789012345678901234567890123456789\"}\n";
    line.iter().copied().cycle().take(n - n % line.len()).collect()
}

/// 9.5 item 6 / magpie 6.10: an `O_APPEND` write carries `append`, so a write at a size magpie does not have (here
/// another writer made magpie's file longer than the sentry thinks) is refused with `offset_mismatch` and goes to the
/// repair, which finds magpie longer than the cache: the fork path (EIO, the agent stopped), never an overwrite.
#[test]
fn an_append_at_a_stale_offset_is_refused_not_overwritten() {
    let w = world();
    let fs = w.fs(fast());
    let forked = Arc::new(Mutex::new(Vec::new()));
    let fk = forked.clone();
    let hooks = Hooks {
        forked: Arc::new(move |m| fk.lock().unwrap().push(m)),
        ..Hooks::default()
    };
    let _core = w.start(&fs, Uuid::new_v4(), false, hooks);
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("t.jsonl", libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT, 0o600),
    );
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    ok(
        &s.call(op::WRITE, t, PID, &body::write(fh, 0, b"one\n")),
        "first append",
    );
    let last = w.magpie.script().calls_to("cc_write").pop().unwrap();
    assert!(
        last.query.as_ref().and_then(|q| q.get("append")).is_some(),
        "an O_APPEND write is sent as an append: {:?}",
        last.query
    );
    w.magpie.seed_file(w.cid, "projects/-workspace/t.jsonl", b"one\nXX\n");
    assert_eq!(
        s.call(op::WRITE, t, PID, &body::write(fh, 4, b"two\n")).errno,
        libc::EIO
    );
    assert_eq!(
        w.data("projects/-workspace/t.jsonl").unwrap(),
        b"one\nXX\n",
        "magpie's bytes are not overwritten"
    );
    assert_eq!(forked.lock().unwrap().len(), 1, "the agent is stopped");
    // a handle without O_APPEND keeps pwrite semantics (no append flag)
    let r = s.call(op::OPEN, t, PID, &body::open(O_RDWR));
    let h = out::open_fh(&r);
    w.magpie.seed_file(w.cid, "projects/-workspace/t.jsonl", b"one\n");
    let _ = s.call(op::READ, t, PID, &body::read(h, 0, 1));
    let before = w.calls("cc_write");
    let _ = s.call(op::WRITE, t, PID, &body::write(h, 0, b"ONE\n"));
    let q = w.magpie.script().calls_to("cc_write")[before].query.clone().unwrap();
    assert!(q.get("append").is_none(), "{q:?}");
}

/// 9.5 item 7: a `lease_lost` answered to a repair's write runs the lease loop like any request's: the hook, a new
/// lease, and magpie-scope requests served again (not EIO for good).
#[test]
fn a_lease_lost_on_a_repair_write_runs_the_lease_loop() {
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
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("t.jsonl", O_RDWR | libc::O_CREAT, 0o600),
    );
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    ok(&s.call(op::WRITE, t, PID, &body::write(fh, 0, b"one\n")), "1");
    ok(&s.call(op::WRITE, t, PID, &body::write(fh, 4, b"two\n")), "2");
    // magpie loses the second write; the repair's first write (at magpie's size, 4) is answered lease_lost
    w.magpie.seed_file(w.cid, "projects/-workspace/t.jsonl", b"one\n");
    let once = Arc::new(AtomicBool::new(true));
    w.magpie.script().handle("cc_write", move |c| {
        let off = c.query.as_ref().and_then(|q| q.get("offset")).cloned();
        let at4 = off.is_some_and(|v| v.as_u64() == Some(4) || v.as_str() == Some("4"));
        (at4 && once.swap(false, Ordering::SeqCst))
            .then(|| aviary_fakes::script::Reply::error(409, "lease_lost", "another lease"))
    });
    assert_eq!(
        s.call(op::WRITE, t, PID, &body::write(fh, 8, b"three\n")).errno,
        libc::EIO
    );
    assert_eq!(lost.load(Ordering::SeqCst), 1, "the lease_lost hook ran");
    for _ in 0..300 {
        if regained.load(Ordering::SeqCst) == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(regained.load(Ordering::SeqCst), 1, "a new lease was taken");
    ok(
        &s.call(op::MKDIR, ws, PID, &body::mkdir("after", 0o700)),
        "magpie scope is served again",
    );
}

/// 9.5 item 4: a 500 from magpie is not definitive: the write is retried with the same `opId` and stored once.
#[test]
fn a_transient_500_on_cc_write_is_retried() {
    let w = world();
    let fs = w.fs(fast());
    let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let r = s.call(
        op::CREATE,
        ws,
        PID,
        &body::create("t.jsonl", O_RDWR | libc::O_APPEND | libc::O_CREAT, 0o600),
    );
    let (t, fh) = (out::node(&r), out::create_fh(&r));
    let before = w.calls("cc_write");
    w.magpie.script().once(
        "cc_write",
        aviary_fakes::script::Reply::error(500, "internal", "a database error"),
    );
    ok(&s.call(op::WRITE, t, PID, &body::write(fh, 0, b"one\n")), "after a 500");
    assert_eq!(w.data("projects/-workspace/t.jsonl").unwrap(), b"one\n");
    let calls = w.magpie.script().calls_to("cc_write");
    assert_eq!(calls.len() - before, 2, "one refused attempt, one retry");
    let ids: Vec<_> = calls[before..]
        .iter()
        .map(|c| c.query.as_ref().unwrap()["opId"].clone())
        .collect();
    assert_eq!(ids[0], ids[1], "the retry keeps the opId");
}

/// `Core::start` never replays or drops node ids against a tree that did not load: a 500 on cc/tree is retried
/// before the replay; a definitive failure fails the start and leaves the node ids alone.
#[test]
fn a_start_waits_for_the_tree_and_never_runs_on_an_empty_one() {
    let w = world();
    let fs = w.fs(fast());
    let boot = Uuid::new_v4();
    super::store_boot_id(&w.dir.join("state"), boot).unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let a = armed.clone();
    let hooks = Hooks {
        fault: Some(Arc::new(move |p, h| {
            p == Point::CallAnswered && h.opcode == op::RENAME && a.swap(false, Ordering::SeqCst)
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
    let t = out::node(&r);
    assert!(core.settle(Duration::from_secs(5)));
    armed.store(true, Ordering::SeqCst);
    let u = s.submit(op::RENAME, ws, PID, &body::rename(ws, "t.jsonl", "u.jsonl"));
    for _ in 0..500 {
        if core.died() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(core.died());
    // a definitive answer to the tree: the start fails, nothing is dropped
    w.magpie
        .script()
        .once("cc_tree", aviary_fakes::script::Reply::error(404, "not_found", "x"));
    let lease = w.lease();
    assert!(w.serve(&fs, w.sentry.conn(), boot, true, &lease).is_err());
    let log = std::fs::read_to_string(super::nodes::log_path(&w.dir.join("state"))).unwrap();
    assert!(!log.contains("drop"), "no node id dropped: {log}");
    // a 500 first, then the tree: the replay sees magpie's state
    w.magpie
        .script()
        .once("cc_tree", aviary_fakes::script::Reply::error(500, "internal", "x"));
    let _core2 = w.start(&fs, boot, true, Hooks::default());
    let r = s.wait(u, Duration::from_secs(10)).expect("the rename completes");
    assert_eq!(r.errno, 0, "the replayed rename succeeds");
    assert_eq!(out::node(&s.call(op::LOOKUP, ws, PID, &body::name("u.jsonl"))), t);
    let log = std::fs::read_to_string(super::nodes::log_path(&w.dir.join("state"))).unwrap();
    assert!(!log.contains("drop"), "no node id dropped: {log}");
}

/// 9.9 for local scope: a replayed mutation whose effect the killed instance already made answers success.
#[test]
fn journal_replay_at_each_fault_point_local_scope() {
    for point in [Point::ReadReturned, Point::CallAnswered, Point::ReplyWritten] {
        for opcode in [op::MKDIR, op::CREATE, op::RENAME, op::UNLINK, op::RMDIR, op::SYMLINK] {
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
            let r = s.call(op::MKDIR, 1, PID, &body::mkdir("sessions", 0o700));
            let d = out::node(&r);
            let r = s.call(
                op::CREATE,
                d,
                PID,
                &body::create("a.json", O_RDWR | libc::O_CREAT, 0o600),
            );
            let f = out::node(&r);
            ok(&s.call(op::RELEASE, f, PID, &body::fh(out::create_fh(&r))), "release");
            ok(&s.call(op::MKDIR, d, PID, &body::mkdir("old.lock", 0o700)), "mkdir");
            // the previous request's worker has passed its own fault points (it replies before reply-written)
            assert!(core.settle(Duration::from_secs(5)));
            armed.store(true, Ordering::SeqCst);
            let b = match opcode {
                op::MKDIR => body::mkdir("x.lock", 0o700),
                op::CREATE => body::create("n.json", O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600),
                op::RENAME => body::rename(d, "a.json", "b.json"),
                op::UNLINK => body::name("a.json"),
                op::RMDIR => body::name("old.lock"),
                _ => body::symlink("ln", "a.json"),
            };
            let u = s.submit(opcode, d, PID, &b);
            for _ in 0..500 {
                if core.died() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(core.died(), "{point:?} {}: the fault fired", opname(opcode));
            let _core2 = w.start(&fs, boot, true, Hooks::default());
            let r = s
                .wait(u, Duration::from_secs(10))
                .expect("the caller's syscall completes");
            let what = format!("{point:?} {}", opname(opcode));
            if point == Point::ReplyWritten {
                assert_eq!(r.errno, 0, "{what}: delivered before the kill");
                assert_eq!(s.refused(u), 1, "{what}: the replay's reply is refused");
            } else {
                assert_eq!(r.errno, 0, "{what}: one success, not EEXIST/ENOENT");
            }
            let l = w.dir.join("local/sessions");
            match opcode {
                op::MKDIR => assert!(l.join("x.lock").is_dir(), "{what}"),
                op::CREATE => assert!(l.join("n.json").is_file(), "{what}"),
                op::RENAME => {
                    assert!(l.join("b.json").is_file() && !l.join("a.json").exists(), "{what}");
                    assert_eq!(
                        out::node(&s.call(op::LOOKUP, d, PID, &body::name("b.json"))),
                        f,
                        "{what}"
                    );
                }
                op::UNLINK => assert!(!l.join("a.json").exists(), "{what}"),
                op::RMDIR => assert!(!l.join("old.lock").exists(), "{what}"),
                _ => assert!(l.join("ln").is_symlink(), "{what}"),
            }
        }
    }
}

/// 9.7: one whole-file fetch per node however many READs miss the cache at once.
#[test]
fn concurrent_cache_misses_share_one_fetch() {
    let w = world();
    let data = lines(3 << 20);
    w.magpie.seed_file(w.cid, "projects/-workspace/big.jsonl", &data);
    let fs = w.fs(fast());
    let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;
    let t = lookup(&w, "projects/-workspace/big.jsonl");
    let hold = Hold::new();
    let f = w.magpie.faults().times("cc_file", FaultKind::Hold(hold.clone()), 1);
    let offs = [0u64, 1 << 20, 2 << 20, (3 << 20) - 9000];
    let us: Vec<u64> = offs
        .iter()
        .enumerate()
        .map(|(i, off)| {
            s.submit(
                op::READ,
                t,
                PID + 1 + u32::try_from(i).unwrap(),
                &body::read(0, *off, 4096),
            )
        })
        .collect();
    assert!(w.rt.block_on(hold.wait_held(1, Duration::from_secs(5))));
    std::thread::sleep(Duration::from_millis(100));
    hold.release();
    for (u, off) in us.iter().zip(offs) {
        let r = s.wait(*u, Duration::from_secs(10)).expect("the read completes");
        ok(&r, "read");
        let off = usize::try_from(off).unwrap();
        let end = (off + 4096).min(data.len());
        assert_eq!(r.payload, &data[off..end]);
    }
    w.magpie.faults().remove(f);
    assert_eq!(w.calls("cc_file"), 1, "one fetch for every miss");
    ok(&s.call(op::READ, t, PID, &body::read(0, 0, 10)), "now cached");
    assert_eq!(w.calls("cc_file"), 1);
}

/// 9.7: a write that lands while the main transcript's prefetch streams is applied to the fetched copy, so the
/// pinned transcript is cached at the new version when the prefetch ends (not left uncached until the next READ).
#[test]
fn a_write_during_the_prefetch_leaves_the_main_transcript_cached() {
    let w = world();
    let data = lines(2 << 20);
    w.magpie.seed_file(w.cid, "projects/-workspace/s.jsonl", &data);
    w.magpie.set_provider_session(w.cid, "s");
    // the prefetch's answer is the file as magpie read it, delivered 1.5 s later (after the write below)
    let (first, old) = (Arc::new(AtomicBool::new(true)), data.clone());
    w.magpie.script().handle("cc_file", move |_| {
        first
            .swap(false, Ordering::SeqCst)
            .then(|| aviary_fakes::script::Reply::bytes(200, old.clone()))
    });
    let f = w
        .magpie
        .faults()
        .times("cc_file", FaultKind::Latency(Duration::from_millis(1500)), 1);
    let fs = w.fs(fast());
    let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    for _ in 0..500 {
        if w.magpie.faults().hits() > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(w.magpie.faults().hits() > 0, "the prefetch's fetch is under way");
    let s = &w.sentry;
    let t = lookup(&w, "projects/-workspace/s.jsonl");
    let r = s.call(op::OPEN, t, PID, &body::open(libc::O_WRONLY | libc::O_APPEND));
    let h = out::open_fh(&r);
    let n = data.len() as u64;
    ok(
        &s.call(op::WRITE, t, PID, &body::write(h, n, b"{\"new\":1}\n")),
        "append during the prefetch",
    );
    let r = s.call(op::READ, t, PID, &body::read(0, n - 4, 100));
    ok(&r, "read the tail");
    assert_eq!(&r.payload[..4], &data[data.len() - 4..]);
    assert_eq!(&r.payload[4..], b"{\"new\":1}\n");
    w.magpie.faults().remove(f);
    assert_eq!(w.calls("cc_file"), 1, "the prefetch's copy is current: no second fetch");
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

/// 9.9 (WP-22 review 3 item 1): a read that leaves no whole request in its slot. The header is zeroed before each
/// read, so a slot the read left untouched never answers the slot's previous, already answered request: nothing is
/// answered and the request is counted as lost. A header that landed without its `len` gets EIO under its own
/// `unique`.
#[test]
fn a_short_read_answers_only_the_request_it_names() {
    let w = world();
    let mut cfg = w.cfg(fast());
    cfg.slots = 1; // every request goes through the same slot
    let fs = FuseFs::new(cfg, w.client("sbx-1"));
    let core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;
    let u1 = s.submit(op::GETATTR, 1, PID, &body::getattr());
    ok(&s.wait(u1, Duration::from_secs(5)).unwrap(), "getattr");
    // nothing of the request landed (the WP-57 case: the slot kept len 0 while read returned 56)
    let u2 = s.submit_short(op::GETATTR, 1, PID, &body::getattr(), 0..0);
    wait_for("the short read", || core.short_reads() == 1);
    assert!(s.wait(u2, Duration::from_millis(300)).is_none(), "nothing names u2");
    assert_eq!(s.refused(u1), 0, "no EIO to the slot's previous, answered request");
    assert_eq!(core.lost_requests(), 1, "counted as lost");
    // the header without its len: EIO to the request it names
    let u3 = s.submit_short(op::GETATTR, 1, PID, &body::getattr(), 4..56);
    let r = s
        .wait(u3, Duration::from_secs(5))
        .expect("the named request is answered");
    assert_eq!(r.errno, libc::EIO);
    assert_eq!((core.short_reads(), core.lost_requests()), (2, 1));
    ok(&s.call(op::GETATTR, 1, PID, &body::getattr()), "the slot serves again");
}

/// 9.5 item 6 (WP-22 review 3 item 2): a repair candidate's cache file holds the only copy of what magpie lost, so
/// a repair that fails for a reason that proves nothing about the bytes (quota, lease) keeps it for the next tree
/// load; only a mismatch discards it.
#[test]
fn a_repair_that_fails_without_a_mismatch_keeps_the_cached_copy() {
    for (status, code) in [(507, "quota_exceeded"), (409, "lease_lost")] {
        let w = world();
        let fs = w.fs(fast());
        let regained = Arc::new(AtomicUsize::new(0));
        let g = regained.clone();
        let hooks = Hooks {
            lease_regained: Arc::new(move |_| {
                g.fetch_add(1, Ordering::SeqCst);
            }),
            ..Hooks::default()
        };
        let core = w.start(&fs, Uuid::new_v4(), false, hooks);
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
        let path = "projects/-workspace/t.jsonl";
        let v = w.magpie.node(w.cid, path).unwrap().version;
        // a restore lost the second write: magpie is one version behind the cache
        w.magpie.restore_file(w.cid, path, b"one\n", v - 1);
        w.magpie
            .script()
            .once("cc_write", aviary_fakes::script::Reply::error(status, code, "x"));
        let cached = || {
            std::fs::read_dir(w.dir.join("cache"))
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{t}.")))
                .map(|e| std::fs::read(e.path()).unwrap())
                .collect::<Vec<_>>()
        };
        core.resumed();
        if status == 507 {
            assert_eq!(w.data(path).unwrap(), b"one\n", "{code}: the repair write was refused");
            assert_eq!(cached(), vec![b"one\ntwo\n".to_vec()], "{code}: the candidate is kept");
            core.resumed();
        }
        // the next tree load (the second `resumed`, or the lease loop's reload) repairs from the kept copy
        wait_for("the repair", || w.data(path).unwrap() == b"one\ntwo\n");
        if status == 409 {
            wait_for("the new lease", || regained.load(Ordering::SeqCst) == 1);
        }
        ok(&s.call(op::WRITE, t, PID, &body::write(fh, 8, b"three\n")), "3");
        assert_eq!(w.data(path).unwrap(), b"one\ntwo\nthree\n", "{code}");
    }
}

/// 9.3 (WP-22 review 3 item 4): a kept interrupted result survives the thread's non-mutating requests (a restarted
/// syscall may walk its path first) but expires `kept_ttl` after it was kept or the server resumed, so a much later
/// identical request from a reused thread id is executed, not answered with the old result.
#[test]
fn a_kept_result_survives_a_path_walk_and_expires() {
    let w = world();
    let mut cfg = w.cfg(fast());
    cfg.kept_ttl = Duration::from_millis(500);
    let fs = FuseFs::new(cfg, w.client("sbx-1"));
    let _core = w.start(&fs, Uuid::new_v4(), false, Hooks::default());
    let s = &w.sentry;
    let ws = workspace_dir(&w);
    let keep = |name: &str| {
        let b = body::mkdir(name, 0o700);
        let hold = Hold::new();
        let f = w.magpie.faults().times("cc_mkdir", FaultKind::Hold(hold.clone()), 1);
        let u1 = s.submit(op::MKDIR, ws, PID, &b);
        assert!(w.rt.block_on(hold.wait_held(1, Duration::from_secs(5))));
        s.forget(u1); // the checkpoint
        hold.release();
        assert!(
            s.wait_refused(u1, Duration::from_secs(5)),
            "the original's reply is refused"
        );
        w.magpie.faults().remove(f);
        b
    };
    // the resend after a path walk: the kept result
    let b = keep("a.lock");
    ok(&s.call(op::GETATTR, ws, PID, &body::getattr()), "path walk");
    let before = w.calls("cc_mkdir");
    ok(&s.call(op::MKDIR, ws, PID, &b), "the resend gets the kept result");
    assert_eq!(w.calls("cc_mkdir"), before, "not executed again");
    // later: another thread removed the lock and took it again; the thread's identical mkdir is a new request
    let b = keep("b.lock");
    ok(&s.call(op::GETATTR, ws, PID, &body::getattr()), "path walk");
    ok(
        &s.call(op::RMDIR, ws, PID + 1, &body::name("b.lock")),
        "released by another thread",
    );
    ok(&s.call(op::MKDIR, ws, PID + 1, &b), "taken by another thread");
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(
        s.call(op::MKDIR, ws, PID, &b).errno,
        libc::EEXIST,
        "the expired result is not handed to a later identical mkdir: the lock is held"
    );
}

/// 9.9 (WP-22 review 3 item 6): a replayed local change answers success only when its slot's done mark says the
/// killed instance made it; one that was never applicable keeps its error.
#[test]
fn a_local_replay_that_was_never_applicable_keeps_its_error() {
    for opcode in [op::UNLINK, op::RENAME, op::MKDIR] {
        let w = world();
        let fs = w.fs(fast());
        let boot = Uuid::new_v4();
        super::store_boot_id(&w.dir.join("state"), boot).unwrap();
        let armed = Arc::new(AtomicBool::new(false));
        let a = armed.clone();
        let hooks = Hooks {
            fault: Some(Arc::new(move |p, h| {
                p == Point::ReadReturned && h.opcode == opcode && a.swap(false, Ordering::SeqCst)
            })),
            ..Hooks::default()
        };
        let core = w.start(&fs, boot, false, hooks);
        let s = &w.sentry;
        let d = out::node(&s.call(op::MKDIR, 1, PID, &body::mkdir("sessions", 0o700)));
        let r = s.call(
            op::CREATE,
            d,
            PID,
            &body::create("b.json", O_RDWR | libc::O_CREAT, 0o600),
        );
        ok(
            &s.call(op::RELEASE, out::node(&r), PID, &body::fh(out::create_fh(&r))),
            "release",
        );
        ok(
            &s.call(op::MKDIR, d, PID, &body::mkdir("held.lock", 0o700)),
            "a lock held already",
        );
        assert!(core.settle(Duration::from_secs(5)));
        armed.store(true, Ordering::SeqCst);
        let (b, want) = match opcode {
            op::UNLINK => (body::name("missing.json"), libc::ENOENT),
            // `from` is missing and `to` existed before: not a rename already made
            op::RENAME => (body::rename(d, "a.json", "b.json"), libc::ENOENT),
            _ => (body::mkdir("held.lock", 0o700), libc::EEXIST),
        };
        let u = s.submit(opcode, d, PID, &b);
        wait_for("the fault", || core.died());
        let _core2 = w.start(&fs, boot, true, Hooks::default());
        let r = s.wait(u, Duration::from_secs(10)).expect("the replay answers");
        assert_eq!(r.errno, want, "{}: the error, not a replayed success", opname(opcode));
    }
}

/// 9.10 as built (WP-22 review 3 item 3, recorded in GATE.md): a supervisor restart during a magpie outage serves
/// nothing until magpie answers (4.3 step 5 takes the lease before the mount, and the start loads the tree), then
/// completes the journal's requests and serves new ones.
#[test]
fn a_restart_during_an_outage_waits_for_magpie_then_completes() {
    let w = world();
    let fs = w.fs(fast());
    let boot = Uuid::new_v4();
    super::store_boot_id(&w.dir.join("state"), boot).unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let a = armed.clone();
    let hooks = Hooks {
        fault: Some(Arc::new(move |p, h| {
            p == Point::ReadReturned && h.opcode == op::MKDIR && a.swap(false, Ordering::SeqCst)
        })),
        ..Hooks::default()
    };
    let core = w.start(&fs, boot, false, hooks);
    let s = &w.sentry;
    assert!(core.settle(Duration::from_secs(5)));
    armed.store(true, Ordering::SeqCst);
    let u = s.submit(op::MKDIR, 1, PID, &body::mkdir("sessions", 0o700)); // local scope
    wait_for("the fault", || core.died());
    let lease = w.lease();
    let f = w
        .magpie
        .faults()
        .on("cc_tree", FaultKind::Unavailable { retry_after_secs: 0 });
    std::thread::scope(|sc| {
        let start = sc.spawn(|| w.serve(&fs, w.sentry.conn(), boot, true, &lease));
        let g = s.submit(op::GETATTR, 1, PID + 1, &body::getattr());
        std::thread::sleep(Duration::from_millis(800));
        let (replay_waited, new_waited) = (s.wait(u, Duration::ZERO).is_none(), s.wait(g, Duration::ZERO).is_none());
        // magpie comes back before any assertion: a failed one would otherwise leave the scope joining a start that
        // retries for ever
        w.magpie.faults().remove(f);
        assert!(replay_waited, "the local replay waits for magpie");
        assert!(new_waited, "a new request waits too");
        assert!(start.join().unwrap().is_ok());
        ok(&s.wait(u, Duration::from_secs(10)).unwrap(), "the replayed mkdir");
        ok(&s.wait(g, Duration::from_secs(10)).unwrap(), "the new getattr");
    });
}

/// 9.7 (WP-22 review 3 item 7): the main transcript is prefetched after every start, also when the first start
/// failed and the retry runs on the keeper's connection.
#[test]
fn the_main_transcript_is_prefetched_after_a_retried_start() {
    let w = world();
    let data = lines(64 << 10);
    w.magpie.seed_file(w.cid, "projects/-workspace/s.jsonl", &data);
    w.magpie.set_provider_session(w.cid, "s");
    let fs = w.fs(fast());
    let boot = Uuid::new_v4();
    w.magpie
        .script()
        .once("cc_tree", aviary_fakes::script::Reply::error(404, "not_found", "x"));
    let lease = w.lease();
    assert!(w.serve(&fs, w.sentry.conn(), boot, false, &lease).is_err());
    let _core = w.start(&fs, boot, true, Hooks::default());
    wait_for("the prefetch", || w.calls("cc_file") == 1);
    let t = lookup(&w, "projects/-workspace/s.jsonl");
    let r = w.sentry.call(op::READ, t, PID, &body::read(0, 0, 100));
    ok(&r, "read");
    assert_eq!(r.payload, &data[..100]);
    assert_eq!(w.calls("cc_file"), 1, "served from the prefetched copy");
}
