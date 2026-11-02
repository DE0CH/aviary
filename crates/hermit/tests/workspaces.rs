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
use hermit::workspaces::{CliError, JfsCli, JuiceFs, Quota};
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
            copy_tree(from, to).map_err(|e| CliError::Failed(e.to_string()))
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
