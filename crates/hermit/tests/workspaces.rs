//! hermit 24.2 "Workspaces" on the `JuiceFs` store over a plain directory (the `juicefs` CLI replaced by
//! [`FakeCli`]: copy for `clone`, remove for `rmr`, an in-memory quota table): quota on every ensure, path escapes
//! refused, reads with Range, listing pages, clone keeps content and sets its own quota after flushing a live
//! sandbox, delete releases the sandbox first, a refused delete answers 503, a broken mount answers 503 and hands
//! starts over.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aviary_core::auth::Caller;
use common::{Harness, STAFF, eventually};
use hermit::fabric::{Fut, WorkspaceError, Workspaces};
use hermit::workspaces::{CLONE_PREFIX, CLONE_STALE, CliError, JfsCli, JuiceFs, MAX_READS, Quota, READ_WAIT};
use hermit_api::workspaces::{ByteRange, ENTRIES_PAGE, EntryType};
use serde_json::json;
use uuid::Uuid;

/// Run at the start of each `clone_tree` / `rmr`; its answer is logged with the call.
type Hook = Box<dyn Fn(&str) -> String + Send + Sync>;

/// The `juicefs` CLI on a plain directory.
#[derive(Default)]
struct FakeCli {
    quotas: Mutex<HashMap<String, (u64, u64)>>,
    sets: Mutex<Vec<String>>,
    log: Mutex<Vec<String>>,
    refuse_rmr: AtomicBool,
    /// `clone` copies the tree, then fails (a `juicefs clone` killed midway).
    fail_clone: AtomicBool,
    /// Each `clone` takes the next plan: it copies the tree, waits, then fails or not.
    clone_plan: Mutex<std::collections::VecDeque<(Duration, bool)>>,
    hook: Mutex<Option<Hook>>,
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let t = e.file_type()?;
        let dst = to.join(e.file_name());
        if t.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(e.path())?, dst)?;
        } else if t.is_dir() {
            copy_tree(&e.path(), &dst)?;
        } else {
            std::fs::copy(e.path(), dst)?;
        }
    }
    Ok(())
}

fn usage(dir: &Path) -> (u64, u64) {
    let (mut bytes, mut inodes) = (0, 0);
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            inodes += 1;
            let m = e.path().symlink_metadata().unwrap();
            if m.is_dir() {
                let (b, i) = usage(&e.path());
                bytes += b;
                inodes += i;
            } else {
                bytes += m.len();
            }
        }
    }
    (bytes, inodes)
}

struct Cli {
    fake: Arc<FakeCli>,
    root: PathBuf,
}

impl Cli {
    fn note(&self, what: &str) {
        let extra = self.fake.hook.lock().unwrap().as_ref().map(|h| h(what));
        self.fake
            .log
            .lock()
            .unwrap()
            .push(format!("{what}{}", extra.map(|e| format!(" {e}")).unwrap_or_default()));
    }
}

impl JfsCli for Cli {
    fn quota_get<'a>(&'a self, path: &'a str) -> Fut<'a, Result<Option<Quota>, CliError>> {
        Box::pin(async move {
            let q = self.fake.quotas.lock().unwrap().get(path).copied();
            let dir = self.root.join(path.trim_start_matches('/'));
            if !dir.exists() {
                return Err(CliError::Failed(format!(
                    "resolve dir {path}: no such file or directory"
                )));
            }
            let (b, i) = usage(&dir);
            Ok(q.map(|(cap, inodes)| Quota {
                capacity_bytes: cap,
                inodes,
                used_bytes: b,
                used_inodes: i,
            }))
        })
    }

    fn quota_set<'a>(&'a self, path: &'a str, capacity_gib: u32, inodes: u64) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.fake.sets.lock().unwrap().push(path.to_owned());
            self.fake
                .quotas
                .lock()
                .unwrap()
                .insert(path.to_owned(), (u64::from(capacity_gib) << 30, inodes));
            Ok(())
        })
    }

    fn quota_delete<'a>(&'a self, path: &'a str) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.fake.quotas.lock().unwrap().remove(path);
            Ok(())
        })
    }

    fn clone_tree<'a>(&'a self, from: &'a Path, to: &'a Path) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.note("clone");
            let plan = self.fake.clone_plan.lock().unwrap().pop_front();
            copy_tree(from, to).map_err(|e| CliError::Failed(e.to_string()))?;
            let (wait, fail) = plan.unwrap_or_default();
            tokio::time::sleep(wait).await;
            if fail || self.fake.fail_clone.load(Ordering::SeqCst) {
                return Err(CliError::Failed("juicefs clone: no answer in 900 s".into()));
            }
            Ok(())
        })
    }

    fn rmr<'a>(&'a self, path: &'a Path) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.note("rmr");
            if self.fake.refuse_rmr.load(Ordering::SeqCst) {
                return Err(CliError::PermissionDenied("rmr: permission denied".into()));
            }
            std::fs::remove_dir_all(path).map_err(|e| CliError::Failed(e.to_string()))
        })
    }
}

/// A store on a fresh directory with `ws/` in it (a mounted volume).
struct Store {
    root: PathBuf,
    cli: Arc<FakeCli>,
    jfs: Arc<JuiceFs>,
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn store() -> Store {
    let root = std::env::temp_dir().join(format!("hermit-ws-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(root.join("ws")).unwrap();
    let cli = Arc::new(FakeCli::default());
    let store = Arc::new(JuiceFs::new(
        &root,
        Arc::new(Cli {
            fake: cli.clone(),
            root: root.clone(),
        }),
        1,
        1000,
    ));
    Store { root, cli, jfs: store }
}

impl Store {
    fn ws(&self, wid: Uuid) -> PathBuf {
        self.root.join("ws").join(wid.to_string())
    }

    /// Every name in `ws/`, sorted.
    fn ws_names(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.root.join("ws"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }
}

fn sorted(ids: &[Uuid]) -> Vec<String> {
    let mut v: Vec<String> = ids.iter().map(ToString::to_string).collect();
    v.sort();
    v
}

fn code(e: WorkspaceError) -> (u16, String) {
    match e {
        WorkspaceError::Api(a) => (a.status().as_u16(), a.code().to_owned()),
        WorkspaceError::Unavailable(m) => (503, format!("unavailable: {m}")),
        WorkspaceError::NotMounted(m) => (503, format!("not mounted: {m}")),
    }
}

async fn bytes(c: hermit_api::workspaces::FileContent) -> Vec<u8> {
    axum::body::to_bytes(c.body, usize::MAX).await.unwrap().to_vec()
}

#[tokio::test]
async fn every_ensure_sets_the_quota_when_absent_or_different() {
    let s = store();
    let wid = Uuid::new_v4();
    let (w, made) = s.jfs.ensure(wid).await.unwrap();
    assert!(made);
    assert_eq!((w.quota.bytes, w.quota.inodes), (1 << 30, 1000));
    assert!(s.ws(wid).is_dir());
    // the same quota again: nothing to set
    let (_, made) = s.jfs.ensure(wid).await.unwrap();
    assert!(!made);
    assert_eq!(s.cli.sets.lock().unwrap().len(), 1);
    // a folder something else made, or a quota that went missing or differs: set again
    s.cli.quotas.lock().unwrap().clear();
    s.jfs.ensure(wid).await.unwrap();
    s.cli.quotas.lock().unwrap().insert(format!("/ws/{wid}"), (5 << 30, 7));
    let (w, _) = s.jfs.ensure(wid).await.unwrap();
    assert_eq!((w.quota.bytes, w.quota.inodes), (1 << 30, 1000));
    assert_eq!(s.cli.sets.lock().unwrap().len(), 3);
    // usage is the CLI's
    std::fs::write(s.ws(wid).join("a.txt"), b"hello").unwrap();
    std::thread::sleep(Duration::from_millis(1));
    let w = s.jfs.get(Uuid::new_v4()).await.unwrap();
    assert!(w.is_none(), "an absent workspace is None");
    let (w, _) = s.jfs.ensure(wid).await.unwrap();
    assert_eq!((w.usage.bytes, w.usage.inodes), (5, 1));
}

#[tokio::test]
async fn paths_stay_beneath_the_workspace_and_symlinks_inside_are_followed() {
    let s = store();
    let wid = Uuid::new_v4();
    s.jfs.ensure(wid).await.unwrap();
    let ws = s.ws(wid);
    std::fs::create_dir(ws.join("out")).unwrap();
    std::fs::write(ws.join("out/report.txt"), b"0123456789abcdef").unwrap();
    std::os::unix::fs::symlink("/", ws.join("root")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", ws.join("passwd")).unwrap();
    std::os::unix::fs::symlink("../../", ws.join("up")).unwrap();
    std::os::unix::fs::symlink("out/report.txt", ws.join("inside")).unwrap();
    std::os::unix::fs::symlink("out", ws.join("outdir")).unwrap();
    // a neighbour's file, reached by `..` through a symlink
    let other = Uuid::new_v4();
    s.jfs.ensure(other).await.unwrap();
    std::fs::write(s.ws(other).join("secret"), b"x").unwrap();
    std::os::unix::fs::symlink(format!("../{other}/secret"), ws.join("neighbour")).unwrap();

    for p in ["root/etc/passwd", "passwd", "up/ws", "neighbour"] {
        let e = s.jfs.file(wid, p, None).await.map(drop).unwrap_err();
        assert_eq!(code(e).0, 400, "{p} escapes");
    }
    let e = s.jfs.entries(wid, "root", None).await.unwrap_err();
    assert_eq!(code(e), (400, "bad_path".into()));
    // inside: followed
    assert_eq!(
        bytes(s.jfs.file(wid, "inside", None).await.unwrap()).await,
        b"0123456789abcdef"
    );
    let e = s.jfs.entries(wid, "outdir", None).await.unwrap();
    assert_eq!(e.entries[0].name, "report.txt");
    // listing reports symlinks as such
    let root = s.jfs.entries(wid, "", None).await.unwrap();
    let kind = |n: &str| root.entries.iter().find(|e| e.name == n).unwrap().kind;
    assert_eq!(kind("root"), EntryType::Symlink);
    assert_eq!(kind("out"), EntryType::Dir);
    let names: Vec<_> = root.entries.iter().map(|e| e.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "sorted by name");
    // the errors of 3.8
    assert_eq!(
        code(s.jfs.entries(wid, "nope", None).await.unwrap_err()),
        (404, "no_path".into())
    );
    assert_eq!(
        code(s.jfs.entries(wid, "out/report.txt", None).await.unwrap_err()),
        (409, "not_a_dir".into())
    );
    assert_eq!(
        code(s.jfs.file(wid, "out", None).await.map(drop).unwrap_err()),
        (409, "not_a_file".into())
    );
    assert_eq!(
        code(s.jfs.file(Uuid::new_v4(), "x", None).await.map(drop).unwrap_err()),
        (404, "no_workspace".into())
    );
    // a FIFO the agent made never blocks a read
    rustix::fs::mknodat(
        rustix::fs::CWD,
        ws.join("fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    let e = tokio::time::timeout(Duration::from_secs(5), s.jfs.file(wid, "fifo", None))
        .await
        .expect("the open did not block")
        .map(drop)
        .unwrap_err();
    assert_eq!(code(e), (409, "not_a_file".into()));
}

#[tokio::test]
async fn reads_with_range_and_listing_pages() {
    let s = store();
    let wid = Uuid::new_v4();
    s.jfs.ensure(wid).await.unwrap();
    let ws = s.ws(wid);
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(ws.join("big.bin"), &big).unwrap();
    let whole = s.jfs.file(wid, "big.bin", None).await.unwrap();
    assert_eq!(whole.meta.size, 200_000);
    assert!(whole.meta.served.is_none());
    assert!(whole.meta.etag.starts_with("\"200000-"));
    assert_eq!(bytes(whole).await, big);
    let part = s
        .jfs
        .file(
            wid,
            "big.bin",
            Some(ByteRange {
                start: 70_000,
                end: Some(140_000),
            }),
        )
        .await
        .unwrap();
    assert_eq!(part.meta.served, Some((70_000, 140_000)));
    assert_eq!(bytes(part).await, &big[70_000..=140_000]);
    let e = s
        .jfs
        .file(
            wid,
            "big.bin",
            Some(ByteRange {
                start: 200_000,
                end: None,
            }),
        )
        .await
        .map(drop)
        .unwrap_err();
    assert_eq!(code(e).0, 416);

    std::fs::create_dir(ws.join("many")).unwrap();
    for i in 0..(ENTRIES_PAGE + 5) {
        std::fs::write(ws.join(format!("many/f{i:05}")), b"").unwrap();
    }
    let first = s.jfs.entries(wid, "many", None).await.unwrap();
    assert_eq!(first.entries.len(), ENTRIES_PAGE);
    let cursor = first.next_cursor.clone().unwrap();
    let second = s.jfs.entries(wid, "many", Some(&cursor)).await.unwrap();
    assert_eq!(second.entries.len(), 5);
    assert!(second.next_cursor.is_none());
    assert_eq!(second.entries[0].name, format!("f{ENTRIES_PAGE:05}"));
}

#[tokio::test]
async fn clone_keeps_content_with_its_own_quota_and_delete_removes_tree_and_quota() {
    let s = store();
    let (from, to) = (Uuid::new_v4(), Uuid::new_v4());
    s.jfs.ensure(from).await.unwrap();
    std::fs::create_dir(s.ws(from).join("d")).unwrap();
    std::fs::write(s.ws(from).join("d/a"), b"abc").unwrap();
    let w = s.jfs.clone_workspace(to, from).await.unwrap();
    assert_eq!((w.quota.bytes, w.quota.inodes), (1 << 30, 1000));
    assert_eq!(std::fs::read(s.ws(to).join("d/a")).unwrap(), b"abc");
    assert!(s.cli.quotas.lock().unwrap().contains_key(&format!("/ws/{to}")));
    assert_eq!(
        code(s.jfs.clone_workspace(to, from).await.unwrap_err()),
        (409, "exists".into())
    );
    assert_eq!(
        code(s.jfs.clone_workspace(Uuid::new_v4(), Uuid::new_v4()).await.unwrap_err()),
        (404, "no_workspace".into())
    );
    s.jfs.delete(to).await.unwrap();
    assert!(!s.ws(to).exists());
    assert!(!s.cli.quotas.lock().unwrap().contains_key(&format!("/ws/{to}")));
    // absent is fine
    s.jfs.delete(to).await.unwrap();
    // a refused delete (an entry hermit may not remove): 503, logged, and the verdict sees it
    s.cli.refuse_rmr.store(true, Ordering::SeqCst);
    let (st, m) = code(s.jfs.delete(from).await.unwrap_err());
    assert_eq!(st, 503);
    assert!(m.contains("workspace delete refused"), "{m}");
    assert_eq!(
        s.jfs.health().delete_refused.map(|(_, p)| p),
        Some(format!("ws/{from}"))
    );
}

#[tokio::test]
async fn a_broken_mount_answers_503_and_recovers() {
    let s = store();
    let wid = Uuid::new_v4();
    s.jfs.ensure(wid).await.unwrap();
    assert!(s.jfs.probe().await.unmounted.is_none());
    std::fs::rename(s.root.join("ws"), s.root.join("ws-away")).unwrap();
    let e = s.jfs.get(wid).await.unwrap_err();
    assert!(matches!(e, WorkspaceError::NotMounted(_)), "{e:?}");
    assert!(s.jfs.health().unmounted.is_some());
    let api: hermit_api::error::ApiError = s.jfs.ensure(wid).await.unwrap_err().into();
    assert_eq!((api.status().as_u16(), api.code()), (503, "workspace_unavailable"));
    std::fs::rename(s.root.join("ws-away"), s.root.join("ws")).unwrap();
    assert!(s.jfs.get(wid).await.unwrap().is_some());
    assert!(s.jfs.health().unmounted.is_none());
}

#[tokio::test]
async fn clone_flushes_a_live_sandbox_first_and_delete_releases_it_first() {
    let h = Harness::new("ws_flush").await;
    let s = store();
    let r = h.replica_with("hermit-a", s.jfs.clone()).await;
    let magpie = h.client(Caller::Magpie);
    let cid = Uuid::new_v4();
    let a = magpie
        .call(
            &r.url,
            "PUT",
            &format!("/v1/conversations/{cid}/sandbox"),
            Some(json!({"person": STAFF})),
            &[],
        )
        .await;
    assert_eq!(a.status, 202, "{:?}", a.body);
    assert!(
        eventually(Duration::from_secs(20), || async {
            let a = magpie
                .call(&r.url, "GET", &format!("/v1/conversations/{cid}/sandbox"), None, &[])
                .await;
            a.body["state"] == "ready"
        })
        .await
    );
    assert!(s.ws(cid).is_dir(), "the start ensured the workspace");
    std::fs::write(s.ws(cid).join("result.csv"), b"a,b\n").unwrap();
    let row: hermit::db::SandboxRow = hermit::db::latest_of(&r.core().pool, cid).await.unwrap().unwrap();
    let sup = h.fabric.supervisor(row.ip.unwrap()).unwrap();
    let name = row.name.clone();
    {
        let sup = sup.clone();
        let fabric = h.fabric.clone();
        *s.cli.hook.lock().unwrap() = Some(Box::new(move |what| match what {
            "clone" => format!("prepares={}", sup.script().calls_to("prepare").len()),
            _ => format!("destroyed={}", fabric.destroyed.lock().unwrap().contains(&name)),
        }));
    }
    let share = Uuid::new_v4();
    let a = magpie
        .call(
            &r.url,
            "POST",
            &format!("/v1/workspaces/{share}/clone"),
            Some(json!({"from": cid})),
            &[],
        )
        .await;
    assert_eq!(a.status, 201, "{:?}", a.body);
    assert_eq!(std::fs::read(s.ws(share).join("result.csv")).unwrap(), b"a,b\n");
    let prepares = sup.script().calls_to("prepare");
    assert_eq!(prepares.len(), 1);
    assert_eq!(prepares[0].body.as_ref().unwrap()["kind"], "snapshot");
    // a download through the router: range and headers
    let res = magpie
        .http
        .get(format!("{}/v1/workspaces/{share}/files?path=result.csv", r.url))
        .bearer_auth(&magpie.token)
        .header("aviary-protocol", "hermit-core=1")
        .header("range", "bytes=2-")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 206);
    assert_eq!(res.headers()["accept-ranges"], "bytes");
    assert_eq!(res.bytes().await.unwrap().as_ref(), b"b\n");

    let a = magpie
        .call(&r.url, "DELETE", &format!("/v1/workspaces/{cid}"), None, &[])
        .await;
    assert_eq!(a.status, 204, "{:?}", a.body);
    assert!(!s.ws(cid).exists());
    assert_eq!(
        *s.cli.log.lock().unwrap(),
        vec!["clone prepares=1".to_owned(), "rmr destroyed=true".to_owned()],
        "the flush came before the clone, the sandbox was gone before the tree"
    );
    let row = hermit::db::load(&r.core().pool, row.id).await.unwrap().unwrap();
    assert_eq!(row.release_reason.as_deref(), Some("deleted"));
}

// a workspace whose delete is in flight is never mounted again: from the delete's start (before it releases the
// sandbox) until the tree is gone, a start fails at its workspace step and an ensure answers 503; a delete that
// succeeds lifts the guard
#[tokio::test]
async fn a_workspace_being_deleted_is_refused_to_starts_and_ensures() {
    let h = Harness::new("ws_deleting").await;
    let s = store();
    let r = h.replica_with("hermit-a", s.jfs.clone()).await;
    let magpie = h.client(Caller::Magpie);
    let cid = Uuid::new_v4();
    let a = magpie
        .call(&r.url, "PUT", &format!("/v1/workspaces/{cid}"), None, &[])
        .await;
    assert!(a.status < 300, "{:?}", a.body);
    std::fs::write(s.ws(cid).join("a"), b"x").unwrap();
    // a delete whose rmr is refused stays in flight (magpie's outbox retries it)
    s.cli.refuse_rmr.store(true, Ordering::SeqCst);
    let a = magpie
        .call(&r.url, "DELETE", &format!("/v1/workspaces/{cid}"), None, &[])
        .await;
    assert_eq!(a.status, 503, "{:?}", a.body);
    let a = magpie
        .call(&r.url, "PUT", &format!("/v1/workspaces/{cid}"), None, &[])
        .await;
    assert_eq!(
        (a.status, a.body["code"].clone()),
        (503, json!("workspace_unavailable")),
        "{:?}",
        a.body
    );
    let a = magpie
        .call(
            &r.url,
            "PUT",
            &format!("/v1/conversations/{cid}/sandbox"),
            Some(json!({"person": STAFF})),
            &[],
        )
        .await;
    assert_eq!(a.status, 202, "{:?}", a.body);
    assert!(
        eventually(Duration::from_secs(20), || async {
            hermit::db::latest_of(&r.core().pool, cid)
                .await
                .unwrap()
                .is_some_and(|row| row.release_reason.as_deref() == Some("workspace_unavailable"))
        })
        .await,
        "the start failed at its workspace step: {:?}",
        hermit::db::latest_of(&r.core().pool, cid)
            .await
            .unwrap()
            .map(|r| r.release_reason)
    );
    assert!(
        h.fabric.created.lock().unwrap().is_empty(),
        "no sandbox was created on the workspace"
    );
    // the retried delete succeeds and lifts the guard
    s.cli.refuse_rmr.store(false, Ordering::SeqCst);
    let a = magpie
        .call(&r.url, "DELETE", &format!("/v1/workspaces/{cid}"), None, &[])
        .await;
    assert_eq!(a.status, 204, "{:?}", a.body);
    assert!(!s.ws(cid).exists());
    let a = magpie
        .call(&r.url, "PUT", &format!("/v1/workspaces/{cid}"), None, &[])
        .await;
    assert_eq!(a.status, 201, "{:?}", a.body);
}

#[tokio::test]
async fn a_start_on_a_replica_with_a_broken_mount_is_handed_to_one_whose_mount_works() {
    let h = Harness::new("ws_handover").await;
    let good = store();
    let broken = store();
    std::fs::remove_dir(broken.root.join("ws")).unwrap();
    let rb = h.replica_with("hermit-broken", broken.jfs.clone()).await;
    let rg = h.replica_with("hermit-good", good.jfs.clone()).await;
    // both replicas have reported
    hermit::app::report_replica(rb.core()).await.unwrap();
    hermit::app::report_replica(rg.core()).await.unwrap();
    let magpie = h.client(Caller::Magpie);
    let cid = Uuid::new_v4();
    let a = magpie
        .call(
            &rb.url,
            "PUT",
            &format!("/v1/conversations/{cid}/sandbox"),
            Some(json!({"person": STAFF})),
            &[],
        )
        .await;
    assert_eq!(a.status, 202, "{:?}", a.body);
    assert!(
        eventually(Duration::from_secs(30), || async {
            let a = magpie
                .call(&rg.url, "GET", &format!("/v1/conversations/{cid}/sandbox"), None, &[])
                .await;
            a.body["state"] == "ready"
        })
        .await,
        "the start finished on the replica whose mount works"
    );
    assert!(good.ws(cid).is_dir());
    // the broken replica's own workspace calls answer 503 with retry-after
    let a = magpie
        .call(&rb.url, "GET", &format!("/v1/workspaces/{cid}"), None, &[])
        .await;
    assert_eq!(
        (a.status, a.body["code"].clone()),
        (503, json!("workspace_unavailable"))
    );
    assert_eq!(a.headers["retry-after"], "2");
    // and the ops rows say which replica is broken
    let rows = h
        .pool()
        .get()
        .await
        .unwrap()
        .query("select pod from replicas where store_error is not null", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, String>(0), "hermit-broken");
}

#[tokio::test]
async fn a_clone_that_loses_the_race_to_its_target_answers_exists() {
    let s = store();
    let (from, wid) = (Uuid::new_v4(), Uuid::new_v4());
    s.jfs.ensure(from).await.unwrap();
    // another clone makes the target between this one's check and its `juicefs clone`
    let target = s.ws(wid);
    *s.cli.hook.lock().unwrap() = Some(Box::new(move |what| {
        if what == "clone" {
            std::fs::create_dir(&target).unwrap();
        }
        String::new()
    }));
    let e = s.jfs.clone_workspace(wid, from).await.unwrap_err();
    assert_eq!(code(e), (409, "exists".to_owned()));
    // the target is the other clone's, untouched; this clone's temporary copy is removed
    assert!(s.ws(wid).is_dir());
    assert!(
        eventually(Duration::from_secs(5), || async {
            s.ws_names() == sorted(&[from, wid])
        })
        .await,
        "{:?}",
        s.ws_names()
    );
}

// a clone that fails midway (killed at its timeout, or its request dropped) leaves no tree: not at ws/<wid>, which
// never holds a partial copy, and not under its temporary name
#[tokio::test]
async fn a_clone_that_fails_midway_leaves_no_tree() {
    let s = store();
    let (from, wid) = (Uuid::new_v4(), Uuid::new_v4());
    s.jfs.ensure(from).await.unwrap();
    std::fs::write(s.ws(from).join("a"), b"abc").unwrap();
    s.cli.fail_clone.store(true, Ordering::SeqCst);
    let (st, _) = code(s.jfs.clone_workspace(wid, from).await.unwrap_err());
    assert_eq!(st, 503);
    assert!(!s.ws(wid).exists());
    assert!(
        eventually(Duration::from_secs(5), || async { s.ws_names() == sorted(&[from]) }).await,
        "{:?}",
        s.ws_names()
    );
    assert!(!s.cli.quotas.lock().unwrap().contains_key(&format!("/ws/{wid}")));
    // the same id then clones fine (no 409 over a partial tree)
    s.cli.fail_clone.store(false, Ordering::SeqCst);
    s.jfs.clone_workspace(wid, from).await.unwrap();
    assert_eq!(std::fs::read(s.ws(wid).join("a")).unwrap(), b"abc");
    assert_eq!(s.ws_names(), sorted(&[from, wid]));
}

// two clones of the same id at once (a retry after a dropped connection, two magpie replicas freezing one share):
// each copies under its own temporary name, so the one that fails removes only its own copy and the winner's
// ws/<wid> is complete
#[tokio::test]
async fn two_clones_of_one_id_at_once_never_share_a_tree() {
    let s = store();
    let (from, wid) = (Uuid::new_v4(), Uuid::new_v4());
    s.jfs.ensure(from).await.unwrap();
    std::fs::create_dir(s.ws(from).join("d")).unwrap();
    for i in 0..50 {
        std::fs::write(s.ws(from).join(format!("d/f{i}")), format!("{i}")).unwrap();
    }
    // the first fails after the second has finished; both start in the same second
    s.cli
        .clone_plan
        .lock()
        .unwrap()
        .extend([(Duration::from_millis(400), true), (Duration::ZERO, false)]);
    let first = s.jfs.clone_workspace(wid, from);
    let second = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.jfs.clone_workspace(wid, from).await
    };
    let (a, b) = futures_util::future::join(first, second).await;
    assert_eq!(code(a.unwrap_err()).0, 503);
    let w = b.unwrap();
    assert_eq!((w.quota.bytes, w.quota.inodes), (1 << 30, 1000));
    // the failed clone's cleanup has run and the winner's tree is whole
    assert!(
        eventually(Duration::from_secs(5), || async {
            s.ws_names() == sorted(&[from, wid])
        })
        .await,
        "{:?}",
        s.ws_names()
    );
    assert!(
        eventually(Duration::from_secs(5), || async {
            s.cli.log.lock().unwrap().iter().any(|l| l == "rmr")
        })
        .await
    );
    for i in 0..50 {
        assert_eq!(
            std::fs::read_to_string(s.ws(wid).join(format!("d/f{i}"))).unwrap(),
            format!("{i}")
        );
    }
    assert!(s.cli.quotas.lock().unwrap().contains_key(&format!("/ws/{wid}")));
}

// a replica that died mid-clone left a temporary tree: the probe removes it once it is older than any clone can run
#[tokio::test]
async fn the_probe_sweeps_trees_of_abandoned_clones() {
    let s = store();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let old = s.root.join("ws").join(format!(
        "{CLONE_PREFIX}{}-{}",
        Uuid::new_v4(),
        now - CLONE_STALE.as_secs() - 60
    ));
    let running = s
        .root
        .join("ws")
        .join(format!("{CLONE_PREFIX}{}-{now}", Uuid::new_v4()));
    std::fs::create_dir_all(old.join("d")).unwrap();
    std::fs::create_dir_all(&running).unwrap();
    assert!(s.jfs.probe().await.unmounted.is_none());
    assert!(eventually(Duration::from_secs(5), || async { !old.exists() }).await);
    assert!(running.exists(), "a clone that may still run is left alone");
}

// ordinary concurrency on a healthy mount (the files panel's parallel reads, the replica probe) never answers 503
// and never marks the mount broken
#[tokio::test]
async fn concurrent_calls_on_a_healthy_mount_all_answer() {
    let s = store();
    let wid = Uuid::new_v4();
    s.jfs.ensure(wid).await.unwrap();
    for i in 0..20 {
        std::fs::write(s.ws(wid).join(format!("f{i}")), vec![b'x'; 1000 + i]).unwrap();
    }
    let jfs = &s.jfs;
    let lists = (0..60).map(|_| async { jfs.entries(wid, "", None).await.map(|e| e.entries.len()) });
    let reads = (0..60).map(|i| async move {
        let c = jfs.file(wid, &format!("f{}", i % 20), None).await?;
        Ok::<_, WorkspaceError>(bytes(c).await.len())
    });
    let probes = (0..20).map(|_| s.jfs.probe());
    let (lists, reads, probes) = futures_util::future::join3(
        futures_util::future::join_all(lists),
        futures_util::future::join_all(reads),
        futures_util::future::join_all(probes),
    )
    .await;
    for r in &lists {
        assert_eq!(r.as_ref().map_err(|e| format!("{e:?}")), Ok(&20));
    }
    for (i, r) in reads.iter().enumerate() {
        assert_eq!(r.as_ref().map_err(|e| format!("{e:?}")), Ok(&(1000 + i % 20)));
    }
    assert!(probes.iter().all(|h| h.unmounted.is_none()));
    assert_eq!(s.jfs.health().unmounted, None);
}

// 64 downloads that stall hold every read permit: the next read answers 503 after READ_WAIT instead of waiting for
// ever, and the permits come back when those bodies go
#[tokio::test]
async fn stalled_downloads_never_hang_the_next_read() {
    let s = store();
    let wid = Uuid::new_v4();
    s.jfs.ensure(wid).await.unwrap();
    std::fs::write(s.ws(wid).join("big"), vec![7u8; 512 * 1024]).unwrap();
    let mut stalled = Vec::new();
    for _ in 0..MAX_READS {
        stalled.push(s.jfs.file(wid, "big", None).await.unwrap());
    }
    let started = std::time::Instant::now();
    let busy = s.jfs.file(wid, "big", None).await.map(drop).unwrap_err();
    assert!(started.elapsed() >= READ_WAIT.checked_sub(Duration::from_millis(50)).unwrap());
    let (status, msg) = code(busy);
    assert_eq!(status, 503);
    assert!(msg.contains("file reads are running"), "{msg}");
    assert_eq!(s.jfs.health().unmounted, None, "busy is not broken");
    drop(stalled);
    let c = tokio::time::timeout(Duration::from_secs(2), s.jfs.file(wid, "big", None))
        .await
        .expect("the permits came back")
        .unwrap();
    assert_eq!(bytes(c).await.len(), 512 * 1024);
}

/// A store whose mount check never answers (a hung FUSE mount); everything else is the store's.
struct HungProbe(Arc<JuiceFs>);

impl Workspaces for HungProbe {
    fn ensure(&self, wid: Uuid) -> Fut<'_, Result<(hermit_api::workspaces::Workspace, bool), WorkspaceError>> {
        self.0.ensure(wid)
    }
    fn get(&self, wid: Uuid) -> Fut<'_, Result<Option<hermit_api::workspaces::Workspace>, WorkspaceError>> {
        self.0.get(wid)
    }
    fn entries<'a>(
        &'a self,
        wid: Uuid,
        path: &'a str,
        cursor: Option<&'a str>,
    ) -> Fut<'a, Result<hermit_api::workspaces::Entries, WorkspaceError>> {
        self.0.entries(wid, path, cursor)
    }
    fn file<'a>(
        &'a self,
        wid: Uuid,
        path: &'a str,
        range: Option<ByteRange>,
    ) -> Fut<'a, Result<hermit_api::workspaces::FileContent, WorkspaceError>> {
        self.0.file(wid, path, range)
    }
    fn clone_workspace(
        &self,
        wid: Uuid,
        from: Uuid,
    ) -> Fut<'_, Result<hermit_api::workspaces::Workspace, WorkspaceError>> {
        self.0.clone_workspace(wid, from)
    }
    fn delete(&self, wid: Uuid) -> Fut<'_, Result<(), WorkspaceError>> {
        self.0.delete(wid)
    }
    fn probe(&self) -> Fut<'_, hermit::fabric::StoreHealth> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn a_replica_whose_mount_check_hangs_still_reports_and_says_so() {
    let h = Harness::new("ws_hung_probe").await;
    let s = store();
    let r = h.replica_with("hermit-hung", Arc::new(HungProbe(s.jfs.clone()))).await;
    let t = tokio::time::Instant::now();
    hermit::app::report_replica(r.core()).await.unwrap();
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    let row = h
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select store_error from replicas where pod = 'hermit-hung'", &[])
        .await
        .unwrap();
    assert_eq!(
        row.get::<_, Option<String>>(0).as_deref(),
        Some("the mount check gave no answer in 3 s")
    );
}

#[tokio::test]
async fn a_workspace_keeps_its_creation_time_and_loses_it_with_the_workspace() {
    let h = Harness::new("ws_created").await;
    let s = store();
    let r = h.replica_with("hermit-a", s.jfs.clone()).await;
    let magpie = h.client(Caller::Magpie);
    let wid = Uuid::new_v4();
    let a = magpie
        .call(&r.url, "PUT", &format!("/v1/workspaces/{wid}"), None, &[])
        .await;
    assert_eq!(a.status, 201, "{:?}", a.body);
    let made = a.body["createdAt"].clone();
    let stored = || async {
        h.pool()
            .get()
            .await
            .unwrap()
            .query_opt(
                "select to_char(created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') from workspaces
                  where id = $1",
                &[&wid],
            )
            .await
            .unwrap()
            .map(|r| r.get::<_, String>(0))
    };
    assert_eq!(stored().await.map(serde_json::Value::from), Some(made.clone()));
    // the folder changes (JuiceFS's only time for it would move); createdAt does not
    std::fs::write(s.ws(wid).join("new.txt"), b"x").unwrap();
    let a = magpie
        .call(&r.url, "GET", &format!("/v1/workspaces/{wid}"), None, &[])
        .await;
    assert_eq!(a.body["createdAt"], made);
    // the row is the source
    h.sql(&format!(
        "update workspaces set created_at = '2026-01-02T03:04:05.678Z' where id = '{wid}'"
    ))
    .await;
    let a = magpie
        .call(&r.url, "GET", &format!("/v1/workspaces/{wid}"), None, &[])
        .await;
    assert_eq!(a.body["createdAt"], "2026-01-02T03:04:05.678Z");
    // a workspace made before hermit kept the time gets its row on first read, and keeps it
    let old = Uuid::new_v4();
    std::fs::create_dir(s.ws(old)).unwrap();
    let a = magpie
        .call(&r.url, "GET", &format!("/v1/workspaces/{old}"), None, &[])
        .await;
    assert_eq!(a.status, 200, "{:?}", a.body);
    let first = a.body["createdAt"].clone();
    std::fs::write(s.ws(old).join("later.txt"), b"x").unwrap();
    let a = magpie
        .call(&r.url, "GET", &format!("/v1/workspaces/{old}"), None, &[])
        .await;
    assert_eq!(a.body["createdAt"], first);
    // delete removes the row
    let a = magpie
        .call(&r.url, "DELETE", &format!("/v1/workspaces/{wid}"), None, &[])
        .await;
    assert!(a.status < 300, "{:?}", a.body);
    assert_eq!(stored().await, None);
}
