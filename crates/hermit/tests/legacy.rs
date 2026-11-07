//! The switch's one-off commands that outlive A6 (hermit 15.4, 22.3) on the CI Postgres and a plain directory standing
//! in for the `JuiceFS` mount: `jfs-owner` (ownership without following symlinks) and `legacy-cleanup` (refused
//! without `--confirm`, blocked while an old share is not carried over; keeps every folder magpie names, live
//! sandboxes' and recent ones and folders with content, removes orphan share clones, and re-checks each folder right
//! before its removal). `import-jarvis` went in A6; [`imported`] leaves what it recorded.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

mod common;

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{TestDb, fresh_db};
use hermit::fabric::Fut;
use hermit::legacy::{self, Cleanup, EXISTS_BATCH, LegacyError, Referenced, share_workspace};
use hermit::workspaces::{CliError, JfsCli, JuiceFs, Quota};
use time::OffsetDateTime;
use uuid::Uuid;

fn pool(db: &TestDb) -> aviary_pg::Pool {
    aviary_pg::connect(&db.url, "hermit-legacy-test", 4, Duration::from_secs(15)).unwrap()
}

/// hermit's database, migrated.
async fn hermit_db(test: &str) -> (TestDb, aviary_pg::Pool) {
    let h = fresh_db(&format!("{test}_h")).await;
    let hp = pool(&h);
    hermit::db::migrate(&hp).await.unwrap();
    (h, hp)
}

async fn exec(p: &aviary_pg::Pool, sql: &str) {
    p.get().await.unwrap().batch_execute(sql).await.unwrap();
}

async fn count(p: &aviary_pg::Pool, sql: &str) -> i64 {
    p.get().await.unwrap().query_one(sql, &[]).await.unwrap().get(0)
}

/// A scratch directory standing in for `/jfs`, removed with the guard.
struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hermit-legacy-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(p.join("ws")).unwrap();
        Self(p)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dst = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &dst)?;
        } else {
            std::fs::copy(e.path(), dst)?;
        }
    }
    Ok(())
}

/// The `juicefs` CLI on a plain directory: copy for `clone`, remove for `rmr`, quotas in memory.
#[derive(Default)]
struct Cli {
    root: PathBuf,
    quotas: Mutex<HashMap<String, (u32, u64)>>,
    calls: Mutex<Vec<String>>,
    /// `rmr` of a path containing one of these fails.
    fail_rmr: Mutex<Vec<String>>,
}

impl JfsCli for Cli {
    fn quota_get<'a>(&'a self, path: &'a str) -> Fut<'a, Result<Option<Quota>, CliError>> {
        Box::pin(async move {
            Ok(self.quotas.lock().unwrap().get(path).map(|&(g, i)| Quota {
                capacity_bytes: u64::from(g) << 30,
                inodes: i,
                used_bytes: 0,
                used_inodes: 0,
            }))
        })
    }
    fn quota_set<'a>(&'a self, path: &'a str, gib: u32, inodes: u64) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("quota set {path}"));
            self.quotas.lock().unwrap().insert(path.to_owned(), (gib, inodes));
            Ok(())
        })
    }
    fn quota_delete<'a>(&'a self, path: &'a str) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("quota delete {path}"));
            self.quotas.lock().unwrap().remove(path);
            Ok(())
        })
    }
    fn clone_tree<'a>(&'a self, from: &'a Path, to: &'a Path) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            let rel = from.strip_prefix(&self.root).unwrap_or(from).display().to_string();
            self.calls.lock().unwrap().push(format!("clone {rel}"));
            copy_tree(from, to).map_err(|e| CliError::Failed(e.to_string()))
        })
    }
    fn rmr<'a>(&'a self, path: &'a Path) -> Fut<'a, Result<(), CliError>> {
        Box::pin(async move {
            let rel = path.strip_prefix(&self.root).unwrap_or(path).display().to_string();
            self.calls.lock().unwrap().push(format!("rmr {rel}"));
            if self.fail_rmr.lock().unwrap().iter().any(|f| rel.contains(f.as_str())) {
                return Err(CliError::Failed("rmr refused (test)".into()));
            }
            std::fs::remove_dir_all(path).map_err(|e| CliError::Failed(e.to_string()))
        })
    }
}

fn store(root: &Path) -> (Arc<Cli>, JuiceFs) {
    let cli = Arc::new(Cli {
        root: root.to_owned(),
        ..Cli::default()
    });
    let fs = JuiceFs::new(root, cli.clone(), 20, 2_000_000);
    (cli, fs)
}

/// An old share folder with a file and a subfolder.
fn old_share(root: &Path, key: &str, id: &str, text: &str) {
    let ws = root.join("users").join(key).join("shares").join(id).join("ws");
    std::fs::create_dir_all(ws.join("out")).unwrap();
    std::fs::write(ws.join("out/report.md"), text).unwrap();
    std::fs::write(
        root.join("users")
            .join(key)
            .join("shares")
            .join(id)
            .join("messages.json"),
        "[]",
    )
    .unwrap();
}

/// What `import-jarvis` recorded for an old share before A6 (`legacy_shares`): a revoked one never cloned, any other
/// cloned into `ws/<uuidv5>` with its workspace row.
async fn imported(hp: &aviary_pg::Pool, root: &Path, key: &str, id: &str, revoked: bool) {
    let wid = share_workspace(id);
    if !revoked {
        let from = root.join("users").join(key).join("shares").join(id).join("ws");
        copy_tree(&from, &root.join("ws").join(wid.to_string())).unwrap();
        hermit::db::workspace_created(hp, wid, hermit_api::Timestamp::now(), true).await;
    }
    hp.get()
        .await
        .unwrap()
        .execute(
            "insert into legacy_shares (id, workspace, revoked, cloned_at)
             values ($1, $2, $3, case when $3 then null else now() end)",
            &[&id, &wid, &revoked],
        )
        .await
        .unwrap();
}

#[test]
fn jfs_owner_owns_every_entry_without_following_symlinks() {
    let root = Root::new();
    let ws = root.0.join("ws");
    let outside = root.0.join("outside");
    std::fs::write(&outside, "x").unwrap();
    for w in ["a", "b"] {
        std::fs::create_dir_all(ws.join(w).join("deep/er")).unwrap();
        std::fs::write(ws.join(w).join("deep/er/f"), "y").unwrap();
    }
    std::os::unix::fs::symlink(&outside, ws.join("a/link")).unwrap();
    let me = std::fs::metadata(&outside).unwrap();
    if me.uid() == 0 {
        // CI's root step and the runner: real ownership changes
        let r = legacy::fix_owner(&root.0, 1000, 1000, 4).unwrap();
        assert_eq!(r.error_count, 0, "{r:?}");
        assert_eq!(
            r.scanned, 10,
            "ws, 2 x (dir, deep, er, f), the link and nothing outside: {r:?}"
        );
        assert_eq!(r.changed, r.scanned);
        for p in ["", "a", "a/deep/er/f", "b/deep"] {
            let m = std::fs::symlink_metadata(ws.join(p)).unwrap();
            assert_eq!((m.uid(), m.gid()), (1000, 1000), "{p}");
        }
        let l = std::fs::symlink_metadata(ws.join("a/link")).unwrap();
        assert_eq!(l.uid(), 1000, "the link itself is owned");
        assert_eq!(std::fs::metadata(&outside).unwrap().uid(), 0, "its target is not");
        let again = legacy::fix_owner(&root.0, 1000, 1000, 4).unwrap();
        assert_eq!(again.changed, 0, "a second run changes nothing");
    } else {
        // as a user: entries already owned are left alone, the rest are counted as errors (EPERM)
        let r = legacy::fix_owner(&root.0, me.uid(), me.gid(), 4).unwrap();
        assert_eq!((r.scanned, r.changed, r.error_count), (10, 0, 0), "{r:?}");
        let r = legacy::fix_owner(&root.0, me.uid() + 1, me.gid(), 4).unwrap();
        assert_eq!(r.changed, 0);
        assert_eq!(r.error_count, 10);
        assert_eq!(r.errors.len(), 10);
    }
}

/// magpie's `workspaces/exists` as a set, with the batch sizes it was asked, and its `legacy-import` record
/// (complete unless `incomplete` says why not).
#[derive(Default)]
struct Magpie {
    known: Mutex<Vec<Uuid>>,
    batches: Mutex<Vec<usize>>,
    down: bool,
    incomplete: Option<String>,
}

impl Magpie {
    fn knowing(known: Vec<Uuid>) -> Self {
        Self {
            known: Mutex::new(known),
            ..Self::default()
        }
    }
}

impl Referenced for Magpie {
    fn exists<'a>(&'a self, ids: &'a [Uuid]) -> Fut<'a, Result<Vec<Uuid>, String>> {
        Box::pin(async move {
            if self.down {
                return Err("magpie workspaces/exists: unreachable".into());
            }
            self.batches.lock().unwrap().push(ids.len());
            let known = self.known.lock().unwrap();
            Ok(ids.iter().filter(|i| known.contains(i)).copied().collect())
        })
    }
    fn import_incomplete(&self) -> Fut<'_, Result<Option<String>, String>> {
        Box::pin(async move {
            if self.down {
                return Err("magpie legacy-import: unreachable".into());
            }
            Ok(self.incomplete.clone())
        })
    }
}

/// Sets a folder's times two days back (older than legacy-cleanup's `RECENT`).
fn age(p: &Path) {
    let t = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: OffsetDateTime::now_utc().unix_timestamp() - 2 * 86_400,
            tv_nsec: 0,
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: OffsetDateTime::now_utc().unix_timestamp() - 2 * 86_400,
            tv_nsec: 0,
        },
    };
    rustix::fs::utimensat(rustix::fs::CWD, p, &t, rustix::fs::AtFlags::empty()).unwrap();
}

/// An old empty workspace folder.
fn old_folder(ws: &Path, id: Uuid) {
    std::fs::create_dir(ws.join(id.to_string())).unwrap();
    age(&ws.join(id.to_string()));
}

async fn live_sandbox(hp: &aviary_pg::Pool, conversation: Uuid) {
    let now = OffsetDateTime::now_utc();
    hp.get()
        .await
        .unwrap()
        .execute(
            "insert into sandboxes (id, conversation_id, person, status, name, token_kid, cpu, memory_bytes,
                                    start_deadline, expires_at, hard_expires_at)
             values ($1, $2, 'a@example.com', 'ready', $3, 1, 1, 1073741824, $4, $4, $4)",
            &[
                &Uuid::new_v4(),
                &conversation,
                &format!("sbx-{}", conversation.simple()),
                &now,
            ],
        )
        .await
        .unwrap();
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(0xa000_0000_0000_4000_8000_0000_0000_0000 + n)
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one volume, every kind of folder
async fn cleanup_refuses_without_confirm_and_keeps_what_is_referenced() {
    let (_h, hp) = hermit_db("cleanup").await;
    let root = Root::new();
    let (cli, _fs) = store(&root.0);
    let ws = root.0.join("ws");
    let (referenced, live, unreferenced, with_content, recent) = (id(1), id(2), id(3), id(4), id(5));
    for w in [referenced, live, unreferenced, with_content] {
        old_folder(&ws, w);
    }
    std::fs::create_dir(ws.join(recent.to_string())).unwrap();
    std::fs::write(ws.join(with_content.to_string()).join("f"), "x").unwrap();
    age(&ws.join(with_content.to_string()));
    // more empty unreferenced folders than one exists call takes
    let many: Vec<Uuid> = (100..100 + EXISTS_BATCH as u128 + 3).map(id).collect();
    for w in &many {
        old_folder(&ws, *w);
    }
    std::fs::create_dir(ws.join(format!(".clone-{unreferenced}-1-abc"))).unwrap();
    std::fs::create_dir(ws.join("not-a-uuid")).unwrap();
    std::fs::create_dir_all(root.0.join("users/u1/claude")).unwrap();
    std::fs::create_dir_all(root.0.join("sent/x")).unwrap();
    live_sandbox(&hp, live).await;
    exec(
        &hp,
        &format!("insert into workspaces (id, created_at) values ('{unreferenced}', now() - interval '3 days')"),
    )
    .await;
    // an old folder whose workspaces row is new: being made right now
    let new_row = many[1];
    exec(
        &hp,
        &format!("insert into workspaces (id, created_at) values ('{new_row}', now())"),
    )
    .await;
    let magpie = Magpie::knowing(vec![referenced]);

    let refused = legacy::legacy_cleanup(&root.0, &hp, &magpie, cli.as_ref(), false, &[])
        .await
        .unwrap();
    let Cleanup::Refused { plan } = refused else {
        panic!("not refused: {refused:?}")
    };
    assert_eq!(plan.trees, ["users", "sent"]);
    assert_eq!(plan.empty_workspaces.len(), many.len());
    assert!(plan.empty_workspaces.contains(&unreferenced));
    assert!(!plan.empty_workspaces.contains(&new_row));
    assert!(!plan.empty_workspaces.contains(&referenced) && !plan.empty_workspaces.contains(&live));
    assert_eq!(
        (
            plan.kept_referenced,
            plan.kept_live,
            plan.kept_recent,
            plan.kept_with_content
        ),
        (1, 1, 2, 1),
        "{plan:?}"
    );
    assert!(plan.shares_not_carried.is_empty() && plan.orphan_shares.is_empty());
    assert_eq!(
        *magpie.batches.lock().unwrap(),
        [EXISTS_BATCH, 4],
        "every old empty folder without a live sandbox (the referenced one too)"
    );
    assert!(
        cli.calls.lock().unwrap().is_empty(),
        "nothing removed without --confirm"
    );
    assert!(root.0.join("users").exists() && ws.join(unreferenced.to_string()).exists());

    // a folder with content by the time of the run is kept
    let Cleanup::Done { plan, done } = ({
        std::fs::write(ws.join(many[0].to_string()).join("late"), "x").unwrap();
        age(&ws.join(many[0].to_string()));
        legacy::legacy_cleanup(&root.0, &hp, &magpie, cli.as_ref(), true, &[])
            .await
            .unwrap()
    }) else {
        panic!("not done")
    };
    assert!(done.complete(&plan), "{done:?}");
    assert_eq!(done.trees_removed, ["users", "sent"]);
    assert_eq!(
        done.workspaces_removed,
        u64::try_from(plan.empty_workspaces.len()).unwrap()
    );
    assert!(!root.0.join("users").exists() && !root.0.join("sent").exists());
    assert!(!ws.join(unreferenced.to_string()).exists());
    for keep in [referenced, live, with_content, recent, new_row] {
        assert!(ws.join(keep.to_string()).exists(), "{keep}");
    }
    assert!(ws.join(format!(".clone-{unreferenced}-1-abc")).exists() && ws.join("not-a-uuid").exists());
    assert!(ws.join(many[0].to_string()).join("late").exists());
    assert_eq!(
        count(
            &hp,
            &format!("select count(*) from workspaces where id = '{unreferenced}'")
        )
        .await,
        0
    );
    assert_eq!(
        count(&hp, "select count(*) from workspace_deletes").await,
        0,
        "no mark left"
    );
    assert!(
        cli.calls
            .lock()
            .unwrap()
            .contains(&format!("quota delete /ws/{unreferenced}"))
    );

    // magpie away: no plan, nothing removed
    old_folder(&ws, unreferenced);
    let down = Magpie {
        down: true,
        ..Magpie::default()
    };
    let e = legacy::legacy_cleanup(&root.0, &hp, &down, cli.as_ref(), true, &[])
        .await
        .unwrap_err();
    assert!(
        matches!(e, LegacyError::Other(ref m) if m.contains("unreachable")),
        "{e}"
    );
    assert!(ws.join(unreferenced.to_string()).exists());
}

#[tokio::test]
async fn cleanup_asks_magpie_even_with_nothing_to_ask_about() {
    let (_h, hp) = hermit_db("cleanupask").await;
    let root = Root::new();
    let (cli, _fs) = store(&root.0);
    std::fs::create_dir_all(root.0.join("users/u1/claude")).unwrap();
    let down = Magpie {
        down: true,
        ..Magpie::default()
    };
    legacy::legacy_cleanup(&root.0, &hp, &down, cli.as_ref(), true, &[])
        .await
        .unwrap_err();
    assert!(
        root.0.join("users").exists(),
        "users/ stays while magpie does not answer"
    );
    let up = Magpie::default();
    let Cleanup::Done { done, .. } = legacy::legacy_cleanup(&root.0, &hp, &up, cli.as_ref(), true, &[])
        .await
        .unwrap()
    else {
        panic!("not done")
    };
    assert_eq!(*up.batches.lock().unwrap(), [0]);
    assert_eq!(done.trees_removed, ["users"]);
}

/// `users/` and `sent/` stay while magpie does not record its import complete; the folder removals still go, and the
/// run is not complete.
#[tokio::test]
async fn the_old_trees_stay_until_magpie_records_its_import_complete() {
    let (_h, hp) = hermit_db("cleanupimport").await;
    let root = Root::new();
    let (cli, _fs) = store(&root.0);
    std::fs::create_dir_all(root.0.join("users/u1/claude")).unwrap();
    std::fs::create_dir_all(root.0.join("sent/c1")).unwrap();
    let empty = id(900);
    old_folder(&root.0.join("ws"), empty);
    let not_yet = Magpie {
        incomplete: Some("no clean verify --all with every conversation owned is recorded".into()),
        ..Magpie::default()
    };
    let Cleanup::Done { plan, done } = legacy::legacy_cleanup(&root.0, &hp, &not_yet, cli.as_ref(), true, &[])
        .await
        .unwrap()
    else {
        panic!("not done")
    };
    assert!(plan.trees_held.as_deref().unwrap().contains("verify --all"), "{plan:?}");
    assert!(done.trees_removed.is_empty(), "{done:?}");
    assert!(!done.complete(&plan));
    assert!(root.0.join("users/u1/claude").exists() && root.0.join("sent/c1").exists());
    assert_eq!(done.workspaces_removed, 1, "the empty folder still goes");
    // magpie unreachable: nothing at all
    let down = Magpie {
        down: true,
        ..Magpie::default()
    };
    legacy::legacy_cleanup(&root.0, &hp, &down, cli.as_ref(), true, &[])
        .await
        .unwrap_err();
    // recorded complete: the trees go
    let Cleanup::Done { plan, done } =
        legacy::legacy_cleanup(&root.0, &hp, &Magpie::default(), cli.as_ref(), true, &[])
            .await
            .unwrap()
    else {
        panic!("not done")
    };
    assert_eq!(plan.trees_held, None);
    assert_eq!(done.trees_removed, ["users", "sent"]);
    assert!(done.complete(&plan));
}

/// Sets an old share folder's times (the folder and its `ws/`) two days back.
fn age_share(root: &Path, key: &str, id: &str) {
    let d = root.join("users").join(key).join("shares").join(id);
    age(&d.join("ws"));
    age(&d);
}

/// Deletes a share's clone the way hermit's `DELETE /v1/workspaces/<id>` does (magpie revoking the share or purging
/// its conversation): the record first, then the tree and the rows.
async fn delete_through_hermit(hp: &aviary_pg::Pool, fs: &JuiceFs, wid: Uuid) {
    use hermit::fabric::Workspaces as _;
    hermit::db::mark_workspace_deleting(hp, wid).await.unwrap();
    hermit::db::record_share_deleted(hp, wid).await.unwrap();
    fs.delete(wid).await.unwrap();
    hermit::db::forget_workspace(hp, wid).await;
}

#[tokio::test]
async fn cleanup_removes_stale_rows_and_marks_only_unnamed_folders() {
    let (_h, hp) = hermit_db("cleanupstale").await;
    let root = Root::new();
    let (cli, _fs) = store(&root.0);
    let ws = root.0.join("ws");
    let (stale, a, b, taken) = (id(700), id(701), id(702), id(703));
    exec(
        &hp,
        &format!("insert into workspaces (id, created_at) values ('{stale}', now() - interval '2 days')"),
    )
    .await;
    for f in [a, b, taken] {
        old_folder(&ws, f);
    }
    let magpie = Magpie::default();
    let plan = legacy::cleanup_plan(&root.0, &hp, &magpie, &[]).await.unwrap();
    assert_eq!(plan.stale_rows, [stale]);
    assert_eq!(plan.empty_workspaces, [a, b, taken]);
    magpie.known.lock().unwrap().push(taken);
    magpie.batches.lock().unwrap().clear();
    let done = legacy::cleanup_apply(&root.0, cli.as_ref(), &hp, &magpie, &plan)
        .await
        .unwrap();
    assert_eq!(
        *magpie.batches.lock().unwrap(),
        [3, 2],
        "asked first, then about the marked ones"
    );
    assert_eq!((done.workspaces_removed, done.rows_removed), (2, 1), "{done:?}");
    assert!(ws.join(taken.to_string()).exists());
    assert_eq!(
        count(&hp, &format!("select count(*) from workspaces where id = '{stale}'")).await,
        0
    );
    assert!(cli.calls.lock().unwrap().contains(&format!("quota delete /ws/{stale}")));
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // every state an old share can be in
async fn cleanup_needs_every_old_share_carried_over_and_removes_orphan_clones() {
    let (_h, hp) = hermit_db("cleanupshares").await;
    let root = Root::new();
    let (cli, fs) = store(&root.0);
    let ws = root.0.join("ws");
    let live = "liveShare0000000000000"; // magpie names its clone
    let revoked_late = "revokedLate00000000000"; // revoked in the old UI after its clone
    let revoked_early = "revokedEarly0000000000"; // revoked before: never cloned
    let deleted = "deletedInMagpie0000000"; // revoked in magpie after the switch: hermit deleted its clone
    let vanished = "vanishedClone000000000"; // cloned, the clone gone since without a record, magpie does not name it
    let missing = "missingNamed0000000000"; // cloned, the clone gone, and magpie still references it
    let unnamed = "liveUnnamed00000000000"; // live, its clone there, and magpie does not name it
    for (sid, revoked) in [
        (live, false),
        (revoked_late, false),
        (revoked_early, true),
        (deleted, false),
        (vanished, false),
        (missing, false),
        (unnamed, false),
    ] {
        old_share(&root.0, "k", sid, sid);
        imported(&hp, &root.0, "k", sid, revoked).await;
    }
    // the import's last reads: one revoked in the old UI after its clone, one deleted through hermit
    exec(
        &hp,
        &format!("update legacy_shares set revoked = true where id = '{revoked_late}'"),
    )
    .await;
    delete_through_hermit(&hp, &fs, share_workspace(deleted)).await;
    // after A6 (no import any more): two clones lost
    for sid in [vanished, missing] {
        std::fs::remove_dir_all(ws.join(share_workspace(sid).to_string())).unwrap();
    }
    exec(&hp, "update workspaces set created_at = now() - interval '3 days'").await;
    for sid in [live, revoked_late, unnamed] {
        let d = ws.join(share_workspace(sid).to_string());
        age(&d.join("out"));
        age(&d);
    }
    // a folder whose row never committed (a failed make of the old UI)
    let uncommitted = "uncommitted00000000000";
    cli.calls.lock().unwrap().clear();
    old_share(&root.0, "k", uncommitted, "half");
    let magpie = Magpie::knowing(vec![share_workspace(live), share_workspace(missing)]);

    let blocked = legacy::legacy_cleanup(&root.0, &hp, &magpie, cli.as_ref(), true, &[])
        .await
        .unwrap();
    let Cleanup::Blocked { plan } = blocked else {
        panic!("not blocked: {blocked:?}")
    };
    let blocks = |plan: &legacy::CleanupPlan, sid: &str, why: &str| {
        plan.shares_not_carried
            .iter()
            .any(|b| b.starts_with(&format!("{sid}: ")) && b.contains(why))
    };
    assert_eq!(plan.shares_not_carried.len(), 3, "{plan:?}");
    assert!(blocks(&plan, uncommitted, "no committed share row"), "{plan:?}");
    assert!(
        blocks(&plan, missing, "is missing and magpie references it"),
        "{plan:?}"
    );
    assert!(blocks(&plan, unnamed, "magpie does not reference"), "{plan:?}");
    assert_eq!(
        plan.shares_gone,
        [format!("{vanished}: ws/{}", share_workspace(vanished))],
        "a clone made once, gone since and not referenced is carried over"
    );
    assert!(root.0.join("users").exists(), "nothing removed");
    assert!(
        !cli.calls.lock().unwrap().iter().any(|c| c.starts_with("rmr")),
        "{:?}",
        cli.calls.lock().unwrap()
    );

    // --drop-share lets the half folder go only once it is 24 h old
    let drop = [uncommitted.to_owned()];
    let Cleanup::Blocked { plan } = legacy::legacy_cleanup(&root.0, &hp, &magpie, cli.as_ref(), true, &drop)
        .await
        .unwrap()
    else {
        panic!("not blocked")
    };
    assert!(blocks(&plan, uncommitted, "modified in the last 24 h"), "{plan:?}");
    // and never lets a share with a row go
    let Cleanup::Blocked { plan } =
        legacy::legacy_cleanup(&root.0, &hp, &magpie, cli.as_ref(), true, &[unnamed.to_owned()])
            .await
            .unwrap()
    else {
        panic!("not blocked")
    };
    assert!(blocks(&plan, unnamed, "magpie does not reference"), "{plan:?}");
    assert!(plan.dropped.is_empty(), "{plan:?}");

    // carried over: magpie names the live clone (its import fixed), drops the lost one (revoked), the half folder is
    // old and dropped
    age_share(&root.0, "k", uncommitted);
    {
        let mut known = magpie.known.lock().unwrap();
        known.retain(|w| *w != share_workspace(missing));
        known.push(share_workspace(unnamed));
    }
    let Cleanup::Done { plan, done } = legacy::legacy_cleanup(&root.0, &hp, &magpie, cli.as_ref(), true, &drop)
        .await
        .unwrap()
    else {
        panic!("not done")
    };
    let late = share_workspace(revoked_late);
    assert_eq!(plan.orphan_shares, [late], "{plan:?}");
    assert_eq!(plan.dropped.len(), 1, "{plan:?}");
    assert!(plan.dropped[0].starts_with(&format!("{uncommitted}: ")));
    assert_eq!(plan.shares_gone.len(), 2, "{plan:?}");
    assert!(done.complete(&plan), "{done:?}");
    assert!(
        !ws.join(late.to_string()).exists(),
        "the clone of a share revoked after it"
    );
    for sid in [live, unnamed] {
        assert!(ws.join(share_workspace(sid).to_string()).exists(), "{sid}");
    }
    assert!(!root.0.join("users").exists());
    assert_eq!(
        count(
            &hp,
            &format!(
                "select count(*) from legacy_shares s join legacy_share_deletes d using (workspace)
                  where s.id = '{revoked_late}'"
            )
        )
        .await,
        1,
        "the cleanup records the orphan it removed"
    );
}

#[tokio::test]
async fn cleanup_rechecks_each_folder_right_before_removing_it() {
    let (_h, hp) = hermit_db("cleanuprace").await;
    let root = Root::new();
    let (cli, _fs) = store(&root.0);
    let ws = root.0.join("ws");
    let (named_late, started_late, deleting, failing, untouched) = (id(1), id(2), id(3), id(4), id(5));
    for w in [named_late, started_late, deleting, untouched] {
        old_folder(&ws, w);
    }
    let magpie = Magpie::default();
    let plan = legacy::cleanup_plan(&root.0, &hp, &magpie, &[]).await.unwrap();
    assert_eq!(plan.empty_workspaces, [named_late, started_late, deleting, untouched]);
    // between the plan and its removal: magpie names one (a share of an empty workspace committed), a sandbox
    // starts on another, hermit deletes a third itself
    magpie.known.lock().unwrap().push(named_late);
    live_sandbox(&hp, started_late).await;
    hermit::db::mark_workspace_deleting(&hp, deleting).await.unwrap();
    let mut plan = plan;
    plan.empty_workspaces.push(failing);
    let done = legacy::cleanup_apply(&root.0, cli.as_ref(), &hp, &magpie, &plan)
        .await
        .unwrap();
    assert_eq!(done.workspaces_removed, 1, "{done:?}");
    assert!(!ws.join(untouched.to_string()).exists());
    for keep in [named_late, started_late, deleting] {
        assert!(ws.join(keep.to_string()).exists(), "{keep}");
    }
    assert_eq!(done.kept.len(), 4, "{done:?}");
    assert!(
        done.kept
            .iter()
            .any(|k| k.contains(&named_late.to_string()) && k.contains("magpie"))
    );
    assert!(
        done.kept
            .iter()
            .any(|k| k.contains(&started_late.to_string()) && k.contains("sandbox"))
    );
    assert!(
        done.kept
            .iter()
            .any(|k| k.contains(&deleting.to_string()) && k.contains("in flight"))
    );
    assert!(
        done.kept
            .iter()
            .any(|k| k.contains(&failing.to_string()) && k.contains("gone already"))
    );
    assert_eq!(
        count(&hp, "select count(*) from workspace_deletes").await,
        1,
        "only hermit's own mark is left"
    );

    // a removal that fails is a failure (the binary exits 1)
    old_folder(&ws, failing);
    std::fs::write(ws.join(failing.to_string()).join("x"), "y").unwrap();
    age(&ws.join(failing.to_string()));
    let plan = legacy::CleanupPlan {
        orphan_shares: vec![failing],
        ..legacy::CleanupPlan::default()
    };
    cli.fail_rmr.lock().unwrap().push(failing.to_string());
    let done = legacy::cleanup_apply(&root.0, cli.as_ref(), &hp, &magpie, &plan)
        .await
        .unwrap();
    assert!(!done.complete(&plan));
    assert_eq!(done.failed.len(), 1, "{done:?}");
    assert!(ws.join(failing.to_string()).exists());
}
