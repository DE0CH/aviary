//! The switch's one-off commands on a real `JuiceFS` volume (hermit 15.4, 22.3; cutover 8.1): the `juicefs` 1.4.1 CLI
//! of hermit's image and two FUSE mounts of one volume (SQLite metadata, a local bucket), one squashed to 1000:1000 as
//! hermit's own mount is and one not, as Job `hermit-jfs-owner`'s is. `import-jarvis` clones a committed share's
//! frozen folder from `users/<key>/shares/<id>/ws` into `ws/<uuidv5>` through the squashed mount (`juicefs clone`, the
//! `RENAME_NOREPLACE` into place, the quota), skips a share with no committed row and one revoked, and finds the
//! target on the next pass; `jfs-owner` `lchown`s every entry over FUSE without following a symlink;
//! `legacy-cleanup` removes `users/` with `juicefs rmr` and an old empty folder, and keeps the clone magpie names.
//!
//! Runs only as root with `AVIARY_TEST_JUICEFS=1` and `juicefs` in `/usr/local/bin` (ci.yml's test job, under sudo;
//! every wait here is bounded and the mounts are unmounted when the test ends, also on a panic).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::missing_panics_doc)]

mod common;

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::fresh_db;
use hermit::fabric::Fut;
use hermit::legacy::{self, Cleanup, ImportOptions, Referenced, ShareSource, share_workspace};
use hermit::workspaces::{JfsCli, JuiceCli, JuiceFs};
use uuid::Uuid;

const JUICEFS: &str = "/usr/local/bin/juicefs";

fn juicefs(args: &[&str]) {
    let out = Command::new("timeout")
        .arg("120")
        .arg(JUICEFS)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "juicefs {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The volume: its scratch directory and its two mounts, unmounted and removed with the guard.
struct Volume {
    dir: PathBuf,
    meta: String,
    squashed: PathBuf,
    plain: PathBuf,
}

impl Volume {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("hermit-jfs-{}", Uuid::new_v4().simple()));
        let (squashed, plain) = (dir.join("jfs"), dir.join("root"));
        for d in [&squashed, &plain, &dir.join("bucket")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let meta = format!("sqlite3://{}", dir.join("meta.db").display());
        let bucket = format!("{}/", dir.join("bucket").display());
        let v = Self {
            dir,
            meta,
            squashed,
            plain,
        };
        juicefs(&[
            "format",
            "--storage",
            "file",
            "--bucket",
            &bucket,
            "--trash-days",
            "0",
            &v.meta,
            "legacy",
        ]);
        juicefs(&[
            "mount",
            "-d",
            "--all-squash",
            "1000:1000",
            &v.meta,
            &v.squashed.display().to_string(),
        ]);
        juicefs(&["mount", "-d", &v.meta, &v.plain.display().to_string()]);
        v
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        for m in [&self.squashed, &self.plain] {
            let _ = Command::new("timeout")
                .args(["60", JUICEFS, "umount", "--force"])
                .arg(m)
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// magpie's `workspaces/exists` as a set.
struct Magpie(Mutex<Vec<Uuid>>);

impl Referenced for Magpie {
    fn exists<'a>(&'a self, ids: &'a [Uuid]) -> Fut<'a, Result<Vec<Uuid>, String>> {
        Box::pin(async move {
            let known = self.0.lock().unwrap();
            Ok(ids.iter().filter(|i| known.contains(i)).copied().collect())
        })
    }
}

fn pool(url: &str) -> aviary_pg::Pool {
    aviary_pg::connect(url, "hermit-jfs-test", 4, Duration::from_secs(15)).unwrap()
}

/// Two days back (older than legacy-cleanup's recent window).
fn age(p: &Path) {
    let back = time::OffsetDateTime::now_utc().unix_timestamp() - 2 * 86_400;
    let t = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: back,
            tv_nsec: 0,
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: back,
            tv_nsec: 0,
        },
    };
    rustix::fs::utimensat(rustix::fs::CWD, p, &t, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines, clippy::many_single_char_names)] // one volume through every command
async fn the_commands_on_a_real_juicefs_volume() {
    let root = std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0);
    if aviary_core::env::test_only("AVIARY_TEST_JUICEFS").as_deref() != Some("1") || !root {
        eprintln!("skipped: needs root and AVIARY_TEST_JUICEFS=1 (ci.yml's test job)");
        return;
    }
    let v = Volume::new();
    // the old stack's tree, written as root through the plain mount and then owned by 1000:1000 as the live volume's
    // is (the old pods ran as uid 1000; share folders 0700)
    let key = "uc1d7ac9eb7211ea16c6b8e79";
    let share = |id: &str| v.plain.join("users").join(key).join("shares").join(id).join("ws");
    for id in [
        "liveShare0000000000000",
        "noRowYet00000000000000",
        "revoked000000000000000",
    ] {
        std::fs::create_dir_all(share(id).join("out/deep")).unwrap();
        std::fs::write(share(id).join("out/deep/report.md"), id).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", share(id).join("link")).unwrap();
    }
    std::fs::create_dir_all(v.plain.join("sent/x")).unwrap();
    std::fs::create_dir_all(v.plain.join("ws")).unwrap();
    let empty = Uuid::new_v4();
    std::fs::create_dir(v.plain.join("ws").join(empty.to_string())).unwrap();
    for t in ["users", "sent"] {
        let ok = Command::new("chown")
            .args(["-R", "1000:1000"])
            .arg(v.plain.join(t))
            .status()
            .unwrap();
        assert!(ok.success());
    }
    for id in [
        "liveShare0000000000000",
        "noRowYet00000000000000",
        "revoked000000000000000",
    ] {
        let mut perm = std::fs::metadata(share(id).parent().unwrap()).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o700);
        std::fs::set_permissions(share(id).parent().unwrap(), perm).unwrap();
    }
    // A2's first jfs-owner run, before the first import: ws/ (made by root here) to 1000:1000
    let first = legacy::fix_owner(&v.plain, 1000, 1000, 4).unwrap();
    assert_eq!((first.error_count, first.changed), (0, 2), "{first:?}");
    age(&v.plain.join("ws").join(empty.to_string()));

    let (h, o, c) = (
        fresh_db("jfs_h").await,
        fresh_db("jfs_o").await,
        fresh_db("jfs_c").await,
    );
    let (hp, op, cp) = (pool(&h.url), pool(&o.url), pool(&c.url));
    hermit::db::migrate(&hp).await.unwrap();
    op.get()
        .await
        .unwrap()
        .batch_execute(include_str!("fixtures/jarvis-legacy.sql"))
        .await
        .unwrap();
    cp.get()
        .await
        .unwrap()
        .batch_execute(&format!(
            "create schema cc_shared;
             create table cc_shared.shares (id text primary key, owner text not null, owner_name text not null,
               session_id text not null, title text not null, dir text not null, files jsonb not null default '[]',
               created_at timestamptz not null default now(), revoked_at timestamptz);
             insert into cc_shared.shares (id, owner, owner_name, session_id, title, dir, revoked_at) values
               ('liveShare0000000000000', 'a@example.com', 'a', 's', 't', '/jfs/users/{key}/shares/liveShare0000000000000', null),
               ('revoked000000000000000', 'a@example.com', 'a', 's', 't', '/jfs/users/{key}/shares/revoked000000000000000', now())"
        ))
        .await
        .unwrap();

    let cli = Arc::new(JuiceCli::new(v.meta.clone(), String::new()));
    let fs = JuiceFs::new(&v.squashed, cli.clone(), 1, 10_000);
    let src = ShareSource {
        cloudcli: &cp,
        store: &fs,
    };
    let r = legacy::import_jarvis(&hp, &op, Some(src), &v.squashed, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(fs.settle(Duration::from_secs(30)).await, 0);
    assert!(r.complete(), "{r:?}");
    let s = r.shares.clone().unwrap();
    assert_eq!((s.cloned, s.revoked), (1, 1), "{s:?}");
    assert_eq!(s.uncommitted, ["noRowYet00000000000000"]);
    let wid = share_workspace("liveShare0000000000000");
    let clone = v.plain.join("ws").join(wid.to_string());
    assert_eq!(
        std::fs::read_to_string(clone.join("out/deep/report.md")).unwrap(),
        "liveShare0000000000000"
    );
    assert_eq!(
        std::fs::read_link(clone.join("link")).unwrap(),
        PathBuf::from("/etc/hostname"),
        "a symlink is cloned as a link"
    );
    for id in ["noRowYet00000000000000", "revoked000000000000000"] {
        assert!(
            !v.plain.join("ws").join(share_workspace(id).to_string()).exists(),
            "{id}"
        );
    }
    let q = cli.quota_get(&format!("/ws/{wid}")).await.unwrap().unwrap();
    assert_eq!((q.capacity_bytes, q.inodes), (1 << 30, 10_000), "the clone's quota");
    let leftovers: Vec<_> = std::fs::read_dir(v.plain.join("ws"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // the next pass finds the target (RENAME_NOREPLACE never replaces it) and still ensures its quota
    cli.quota_delete(&format!("/ws/{wid}")).await.unwrap();
    let again = legacy::import_jarvis(
        &hp,
        &op,
        Some(src),
        &v.squashed,
        ImportOptions {
            delta: true,
            ..ImportOptions::default()
        },
    )
    .await
    .unwrap()
    .shares
    .unwrap();
    assert_eq!((again.cloned, again.skipped), (0, 1), "{again:?}");
    assert!(cli.quota_get(&format!("/ws/{wid}")).await.unwrap().is_some());

    // A6's second jfs-owner run over the plain mount: every entry under ws/ to 1000:1000, the link itself and not its
    // target
    std::os::unix::fs::lchown(clone.join("out/deep/report.md"), Some(0), Some(0)).unwrap();
    std::os::unix::fs::lchown(clone.join("link"), Some(0), Some(0)).unwrap();
    let before = std::fs::metadata("/etc/hostname").unwrap();
    let o = legacy::fix_owner(&v.plain, 1000, 1000, 4).unwrap();
    assert_eq!(o.error_count, 0, "{o:?}");
    assert!(o.changed >= 2, "{o:?}");
    for p in ["", "out", "out/deep/report.md", "link"] {
        let m = std::fs::symlink_metadata(clone.join(p)).unwrap();
        assert_eq!((m.uid(), m.gid()), (1000, 1000), "{p}");
    }
    let after = std::fs::metadata("/etc/hostname").unwrap();
    assert_eq!(
        (after.uid(), after.gid()),
        (before.uid(), before.gid()),
        "the link's target"
    );

    // legacy-cleanup through the squashed mount: blocked while a share folder has no committed row
    let magpie = Magpie(Mutex::new(vec![wid]));
    let blocked = legacy::legacy_cleanup(&v.squashed, &hp, &magpie, cli.as_ref(), true, &[])
        .await
        .unwrap();
    assert!(matches!(blocked, Cleanup::Blocked { .. }), "{blocked:?}");
    assert!(v.plain.join("users").exists());
    // the half folder gone (the old UI's failed make removes it), the clone old enough: users/ and the empty one go
    std::fs::remove_dir_all(v.plain.join("users").join(key).join("shares/noRowYet00000000000000")).unwrap();
    hp.get()
        .await
        .unwrap()
        .execute("update workspaces set created_at = now() - interval '3 days'", &[])
        .await
        .unwrap();
    let Cleanup::Done { plan, done } = legacy::legacy_cleanup(&v.squashed, &hp, &magpie, cli.as_ref(), true, &[])
        .await
        .unwrap()
    else {
        panic!("not done")
    };
    assert!(done.complete(&plan), "{plan:?} {done:?}");
    assert_eq!(done.trees_removed, ["users", "sent"]);
    assert_eq!(plan.empty_workspaces, [empty]);
    // the other mount's entry cache (1 s by default) may still show them for a moment
    let gone = |p: PathBuf| async move {
        for _ in 0..50 {
            if !p.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    };
    for p in [v.plain.join("users"), v.plain.join("sent"), v.squashed.join("users")] {
        assert!(gone(p.clone()).await, "{} is still there", p.display());
    }
    assert!(!v.squashed.join("ws").join(empty.to_string()).exists());
    assert!(
        clone.join("out/deep/report.md").exists(),
        "the clone magpie names is kept"
    );
}
