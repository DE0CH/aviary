//! A sandbox's files on the host (docs/CLAUDE-CODE.md "Storage in the sandbox"):
//! - scratch: a sparse 5 GiB ext4 file per sandbox, loop-mounted at /var/lib/sbx/scratch/<name>; it holds the root
//!   overlay's upper layer (installs) and the sandbox's /tmp and Docker directory, so writes past 5 GiB fail with
//!   ENOSPC and one sandbox can't fill the host's disk. Plain files: a migration copies them file by file.
//! - root: Linux overlayfs, the image's rootfs (read-only, shared) under scratch/upper, at /var/lib/sbx/root/<name>;
//!   runsc runs with --overlay2=none, so nothing of the root lives in gVisor's memory file.
//! - persistent, on the host's JuiceFS mount (/var/lib/sbx/jfs, mounted by the host setup): `/workspace` is folder
//!   ws/<workspace> (one per conversation; quota 1 GiB and 1,000 files), `~/.claude` is users/<user>/claude (one per
//!   user: Claude Code's transcripts of all the user's conversations, which the UI reads).
use crate::config::Spec;
use crate::path;
use crate::paths::{self, P};
use crate::proc::{quiet, run_env, sh};
use crate::util::{Error, Res};

pub const SCRATCH_BYTES: &str = "5G";
const WORKSPACE_GIB: &str = "1";
const WORKSPACE_FILES: &str = "1000";
/// the sandbox user (uid 1000 inside gVisor; a uid inside gVisor is not a host uid, but the files are owned by it)
const UID: &str = "1000:1000";

pub fn scratch_img(name: &str) -> P {
    path!("/var/lib/sbx/scratch/{name}.img")
}
pub fn scratch_dir(name: &str) -> P {
    path!("/var/lib/sbx/scratch/{name}")
}
pub fn root_dir(name: &str) -> P {
    path!("/var/lib/sbx/root/{name}")
}
pub fn jfs_root() -> P {
    path!("/var/lib/sbx/jfs")
}
pub fn workspace_dir(ws: &str) -> P {
    path!("/var/lib/sbx/jfs/ws/{ws}")
}
pub fn claude_dir(user: &str) -> P {
    path!("/var/lib/sbx/jfs/users/{user}/claude")
}

fn is_mount(dir: &str) -> bool {
    quiet(&["mountpoint", "-q", dir]) == 0
}

/// Scratch filesystem + root overlay for a new sandbox (idempotent: a half-made one from a failed create is reused).
/// Returns the root path for the OCI bundle.
pub fn setup(spec: &Spec) -> Res<P> {
    let name = spec.name.as_str();
    let img = scratch_img(name);
    let sd = scratch_dir(name);
    crate::util::mkdir(path!("/var/lib/sbx/scratch").as_str(), 0o700)?;
    if !is_mount(sd.as_str()) {
        if std::fs::metadata(img.as_str()).is_err() {
            sh(&["truncate", "-s", SCRATCH_BYTES, img.as_str()])?;
            // no reserved blocks (all 5 GiB usable), no lazy init thread writing behind our back
            sh(&["mkfs.ext4", "-q", "-F", "-m", "0", "-E", "lazy_itable_init=0,lazy_journal_init=0", img.as_str()])?;
        }
        crate::util::mkdir(sd.as_str(), 0o700)?;
        sh(&["mount", "-o", "loop,noatime", img.as_str(), sd.as_str()])?;
    }
    for d in ["/upper", "/work"] {
        crate::util::mkdir(paths::join(sd.as_str(), d).as_str(), 0o755)?;
    }
    for d in ["/tmp", "/docker"] {
        let p = paths::join(sd.as_str(), d);
        crate::util::mkdir(p.as_str(), 0o755)?;
    }
    sh(&["chmod", "1777", paths::join(sd.as_str(), "/tmp").as_str()])?;
    let root = root_dir(name);
    if !is_mount(root.as_str()) {
        crate::util::mkdir(path!("/var/lib/sbx/root").as_str(), 0o700)?;
        crate::util::mkdir(root.as_str(), 0o755)?;
        let mut opts: crate::fixed::Str<1024> = crate::fixed::Str::new();
        let ok = opts.push("lowerdir=")
            && opts.push(paths::images().as_str())
            && opts.push("/")
            && opts.push(spec.image.as_str())
            && opts.push("/rootfs,upperdir=")
            && opts.push(sd.as_str())
            && opts.push("/upper,workdir=")
            && opts.push(sd.as_str())
            && opts.push("/work");
        if !ok {
            return Err(Error::new(500, "overlay options too long"));
        }
        sh(&["mount", "-t", "overlay", "overlay", "-o", opts.as_str(), root.as_str()])?;
    }
    if (!spec.workspace.is_empty() || !spec.user.is_empty()) && !is_mount(jfs_root().as_str()) {
        return Err(Error::new(503, "the workspace store (JuiceFS) is not mounted on this host"));
    }
    if !spec.user.is_empty() {
        let cd = claude_dir(spec.user.as_str());
        sh(&["mkdir", "-p", cd.as_str()])?;
        sh(&["chown", UID, cd.as_str()])?;
    }
    if !spec.workspace.is_empty() {
        workspace(spec.workspace.as_str())?;
    }
    Ok(root)
}

/// The conversation's `/workspace`; a new one gets its quota (1 GiB, 1,000 files) at once.
fn workspace(ws: &str) -> Res<()> {
    let wd = workspace_dir(ws);
    let fresh = std::fs::metadata(wd.as_str()).is_err();
    sh(&["mkdir", "-p", wd.as_str()])?;
    sh(&["chown", UID, wd.as_str()])?;
    if fresh {
        set_quota(ws)?;
    }
    Ok(())
}

/// `juicefs quota set` on ws/<ws>. The metadata URL and its password come from /etc/sbx/juicefs.env
/// (META_URL without the password, META_PASSWORD; host setup writes it, mode 600).
fn set_quota(ws: &str) -> Res<()> {
    let mut buf = [0u8; 1024];
    let text = crate::util::read_file(path!("/etc/sbx/juicefs.env").as_str(), &mut buf)
        .map_err(|_| Error::new(503, "/etc/sbx/juicefs.env is missing"))?;
    let text = core::str::from_utf8(text).map_err(|_| Error::new(500, "juicefs.env is not UTF-8"))?;
    let (mut url, mut pw) = ("", "");
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("META_URL=") {
            url = v.trim();
        } else if let Some(v) = line.strip_prefix("META_PASSWORD=") {
            pw = v.trim();
        }
    }
    if url.is_empty() {
        return Err(Error::new(503, "juicefs.env has no META_URL"));
    }
    let mut env_pw: crate::fixed::Str<300> = crate::fixed::Str::new();
    if !(env_pw.push("META_PASSWORD=") && env_pw.push(pw)) {
        return Err(Error::new(500, "META_PASSWORD too long"));
    }
    let mut rel: crate::fixed::Str<100> = crate::fixed::Str::new();
    let _ = rel.push("/ws/") && rel.push(ws);
    let mut out = [0u8; 4096];
    let argv = [
        "juicefs",
        "quota",
        "set",
        url,
        "--path",
        rel.as_str(),
        "--capacity",
        WORKSPACE_GIB,
        "--inodes",
        WORKSPACE_FILES,
    ];
    let o = run_env(&argv, &[env_pw.as_str()], &mut out, 60_000)?;
    if o.code != 0 {
        return Err(Error::new(500, "juicefs quota set failed"));
    }
    Ok(())
}

/// Undo `setup` (every step may already be gone).
pub fn teardown(name: &str) {
    let root = root_dir(name);
    let sd = scratch_dir(name);
    quiet(&["umount", "-l", root.as_str()]);
    quiet(&["umount", "-l", sd.as_str()]);
    quiet(&["rmdir", root.as_str(), sd.as_str()]);
    crate::util::remove_file(scratch_img(name).as_str());
}
