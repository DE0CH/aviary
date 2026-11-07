//! The switch's one-off commands on a real `JuiceFS` volume (hermit 15.4, 22.3): the `juicefs` 1.4.1 CLI of hermit's
//! image and two FUSE mounts of one volume (SQLite metadata, a local bucket), one squashed to 1000:1000 as hermit's own
//! mount is and one not, as Job `hermit-jfs-owner`'s is. A committed share's frozen folder is cloned into
//! `ws/<uuidv5>` as `import-jarvis` left it (that command went in A6); `jfs-owner` `lchown`s every entry over FUSE
//! without following a symlink; `legacy-cleanup` removes `users/` with `juicefs rmr` and an old empty folder, and keeps
//! the clone magpie names.
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
use hermit::legacy::{self, Cleanup, Referenced, share_workspace};
use hermit::workspaces::JuiceCli;
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

/// magpie's `workspaces/exists` as a set; its import recorded complete.
struct Magpie(Mutex<Vec<Uuid>>);

impl Referenced for Magpie {
    fn exists<'a>(&'a self, ids: &'a [Uuid]) -> Fut<'a, Result<Vec<Uuid>, String>> {
        Box::pin(async move {
            let known = self.0.lock().unwrap();
            Ok(ids.iter().filter(|i| known.contains(i)).copied().collect())
        })
    }
    fn import_incomplete(&self) -> Fut<'_, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
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

    let h = fresh_db("jfs_h").await;
    let hp = pool(&h.url);
    hermit::db::migrate(&hp).await.unwrap();
    // what import-jarvis left (until A6): the committed live share cloned into its uuidv5, the revoked one recorded
    let wid = share_workspace("liveShare0000000000000");
    let clone = v.plain.join("ws").join(wid.to_string());
    let ok = Command::new("cp")
        .arg("-a")
        .arg(share("liveShare0000000000000"))
        .arg(&clone)
        .status()
        .unwrap();
    assert!(ok.success());
    hp.get()
        .await
        .unwrap()
        .execute(
            "insert into legacy_shares (id, workspace, revoked, cloned_at) values
               ('liveShare0000000000000', $1, false, now()), ('revoked000000000000000', $2, true, null)",
            &[&wid, &share_workspace("revoked000000000000000")],
        )
        .await
        .unwrap();
    hermit::db::workspace_created(&hp, wid, hermit_api::Timestamp::now(), true).await;
    let cli = Arc::new(JuiceCli::new(v.meta.clone(), String::new()));

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
