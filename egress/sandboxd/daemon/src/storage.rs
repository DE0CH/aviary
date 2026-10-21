//! A sandbox's files on the host (docs/CLAUDE-CODE.md "Storage in the sandbox"):
//! - scratch: a sparse 5 GiB ext4 file per sandbox, loop-mounted at /var/lib/sbx/scratch/<name>; it holds the root
//!   overlay's upper layer (installs) and the sandbox's /tmp and Docker directory, so writes past 5 GiB fail with
//!   ENOSPC and one sandbox can't fill the host's disk. Plain files: a migration copies them file by file with rsync
//!   (`sync_scratch` on the source, `rsync --daemon` behind the destination's migrate-relay; docs/SANDBOX-VM.md
//!   "Scratch on migration"): rounds while the sandbox runs, then a final delta while it is stopped.
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
use core::fmt::Write as _;

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
    premake_upper(spec.image.as_str(), paths::join(sd.as_str(), "/upper").as_str())?;
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
    // the top folders belong to the sandbox user (uid 1000) too, so the UI (CloudCLI, uid 1000, the same volume
    // at /jfs) can remove a deleted conversation's ws/<id> and not only empty it; no sandbox sees them (each gets
    // its own folder bind-mounted)
    let persistent = !spec.workspace.is_empty() || !spec.user.is_empty();
    for top in ["/var/lib/sbx/jfs/ws", "/var/lib/sbx/jfs/users"].iter().filter(|_| persistent) {
        let t = path!("{top}");
        sh(&["mkdir", "-p", t.as_str()])?;
        sh(&["chown", UID, t.as_str()])?;
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

/// Directories on the way to the sandbox's bind mounts. They are made in the upper layer before the overlay is
/// mounted (as the image has them: mode and owner), on every host the same way. Why: runsc looks these paths up
/// while it creates a sandbox, and overlayfs caches what it found; a migration's destination creates the sandbox
/// before the source's upper layer is copied in under the mounted overlay, and a directory cached as lower-only
/// (pinned by the bind mount on it) would hide everything copied into its upper twin. With the upper directory
/// there from the start, the overlay looks into it on every lookup.
const MOUNT_PATHS: [&str; 8] =
    ["/tmp", "/var", "/var/lib", "/var/lib/docker", "/workspace", "/home", "/home/sandbox", "/home/sandbox/.claude"];

fn premake_upper(image: &str, upper: &str) -> Res<()> {
    use std::os::unix::fs::MetadataExt;
    for rel in MOUNT_PATHS {
        let up = paths::join(upper, rel);
        if std::fs::symlink_metadata(up.as_str()).is_ok() {
            continue;
        }
        let lower = path!("/var/lib/sbx/images/{image}/rootfs{rel}");
        let (mode, uid, gid) = match std::fs::symlink_metadata(lower.as_str()) {
            Ok(m) if m.is_dir() => (m.mode() & 0o7777, m.uid(), m.gid()),
            Ok(_) => continue, // not a directory in the image: leave it to the overlay
            Err(_) => (0o755, 0, 0),
        };
        let mut b = [0u8; 512];
        let c = crate::fixed::cstr(up.as_str(), &mut b).ok_or_else(|| Error::new(500, "path too long"))?;
        unsafe {
            if libc::mkdir(c.as_ptr(), 0o700) != 0 {
                return Err(crate::util::at(up.as_str(), std::io::Error::last_os_error()));
            }
            libc::chown(c.as_ptr(), uid, gid);
            libc::chmod(c.as_ptr(), mode);
        }
    }
    Ok(())
}

/// The destination's rsync daemon configuration (module `scratch` = the sandbox's scratch filesystem), next to its
/// bundle. Root on both ends, numeric ids, chroot: owners, modes, times, hard links, ACLs and xattrs (overlayfs'
/// trusted.overlay.* on opaque directories) and devices (overlayfs' whiteouts are 0/0 character devices) arrive as
/// they are on the source. Only overlay addresses may connect.
pub fn rsyncd_conf(name: &str, host_cidr: &str, buf: &mut [u8]) -> Res<P> {
    let conf = path!("/var/lib/sbx/bundles/{name}/rsyncd.conf");
    let mut b = crate::fixed::Buf::new(buf);
    let sd = scratch_dir(name);
    let log = path!("/var/log/sbx/{name}.rsync.log");
    let _ = write!(
        b,
        "use chroot = yes\nnumeric ids = yes\nuid = 0\ngid = 0\ntimeout = 300\nreverse lookup = no\n\
         hosts allow = {host_cidr} 127.0.0.0/8\nlog file = {log}\n[scratch]\npath = {sd}\nread only = no\n\
         list = no\nopen noatime = yes\n"
    );
    if b.full {
        return Err(Error::new(500, "rsyncd.conf too long"));
    }
    let len = b.len;
    crate::util::write_file_atomic(conf.as_str(), &buf[..len], 0o600)?;
    Ok(conf)
}

/// One rsync round's figures (from `rsync --stats`).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct SyncStats {
    pub files: u64,
    pub bytes: u64,
    pub deleted: u64,
    pub ms: u64,
}

/// Copies the sandbox's scratch filesystem to the destination's relay (`to` = its overlay host:port). `last`: the
/// final delta while the sandbox is stopped (any error fails it); before that, files changing or vanishing under
/// rsync (exit 23/24) are expected and the next round picks them up. overlayfs' work directory is not copied.
pub fn sync_scratch(name: &str, to: &str, last: bool, out: &mut [u8]) -> Res<SyncStats> {
    let t0 = crate::util::mono_ms();
    let sd = scratch_dir(name);
    let mut src: P = P::new();
    let _ = src.push(sd.as_str()) && src.push("/");
    let mut url: crate::fixed::Str<120> = crate::fixed::Str::new();
    if !(url.push("rsync://") && url.push(to) && url.push("/scratch/")) {
        return Err(Error::new(500, "rsync url too long"));
    }
    let argv = [
        "rsync",
        "-aHAX",
        "--numeric-ids",
        "--delete",
        "--exclude=/work/**",
        "--exclude=/lost+found",
        "--stats",
        "--timeout=120",
        "--contimeout=10",
        src.as_str(),
        url.as_str(),
    ];
    let o = crate::proc::run(&argv, out, 15 * 60_000)?;
    let ok = o.code == 0 || (!last && (o.code == 23 || o.code == 24));
    if !ok {
        return Err(o.failure(&argv));
    }
    let mut st = parse_stats(o.out_str());
    st.ms = crate::util::mono_ms() - t0;
    Ok(st)
}

fn parse_stats(text: &str) -> SyncStats {
    let num = |line: &str| -> u64 {
        let v = line.split_once(':').map(|x| x.1).unwrap_or("");
        let mut n = 0u64;
        for c in v.trim_start().bytes() {
            match c {
                b'0'..=b'9' => n = n * 10 + (c - b'0') as u64,
                b',' | b'.' => {}
                _ => break,
            }
        }
        n
    };
    let mut st = SyncStats::default();
    for l in text.lines() {
        if l.starts_with("Number of regular files transferred:") {
            st.files = num(l);
        } else if l.starts_with("Total transferred file size:") {
            st.bytes = num(l);
        } else if l.starts_with("Number of deleted files:") {
            st.deleted = num(l);
        }
    }
    st
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsync_stats() {
        let t = "Number of files: 1,203 (reg: 1,000, dir: 200, dev: 3)\nNumber of created files: 10\n\
                 Number of deleted files: 4 (reg: 4)\nNumber of regular files transferred: 1,042\n\
                 Total file size: 123,456,789 bytes\nTotal transferred file size: 12,345,678 bytes\n";
        assert_eq!(parse_stats(t), SyncStats { files: 1042, bytes: 12_345_678, deleted: 4, ms: 0 });
        assert_eq!(parse_stats(""), SyncStats::default());
    }
}
