//! A sandbox's files on the host (docs/HOST.md "Storage in the sandbox"):
//! - scratch: a sparse 5 GiB ext4 file per sandbox, loop-mounted at /var/lib/sbx/scratch/<name>; it holds the root
//!   overlay's upper layer (installs) and the sandbox's /tmp and Docker directory, so writes past 5 GiB fail with
//!   ENOSPC and one sandbox can't fill the host's disk. Plain files: a migration copies them file by file with rsync
//!   (`sync_scratch` on the source, `rsync --daemon` behind the destination's migrate-relay; docs/HOST.md
//!   "Scratch on migration"): rounds while the sandbox runs, then a final delta while it is stopped.
//! - root: Linux overlayfs, the image's rootfs (read-only, shared) under scratch/upper, at /var/lib/sbx/root/<name>;
//!   runsc runs with --overlay2=none, so nothing of the root lives in gVisor's memory file.
//! - persistent, on the host's JuiceFS mount (/var/lib/sbx/jfs, mounted by the host setup): `/workspace` is folder
//!   ws/<workspace> (one per conversation). hermit makes the folder and sets its quota before the create (hermit spec
//!   15.5); sandboxd only binds it, and refuses a create whose folder is missing (409), so a sandbox never makes an
//!   unquota'd one. `~/.claude` is no bind: the supervisor inside the sandbox mounts its FUSE filesystem there
//!   (hermit spec 13.2 items 2, 3).
use crate::config::Spec;
use crate::path;
use crate::paths::{self, P};
use crate::proc::{quiet, sh};
use crate::util::{Error, Res};
use core::fmt::Write as _;

pub const SCRATCH_BYTES: &str = "5G";

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

fn is_mount(dir: &str) -> bool {
    quiet(&["mountpoint", "-q", dir]) == 0
}

/// Is a JuiceFS mount at /var/lib/sbx/jfs, from /proc/self/mountinfo: reads nothing on the FUSE mount itself, so a
/// wedged juicefs can't hang the caller (GET /v1/host, which the scaler times out after 5 s and then loses the host).
pub fn jfs_mount_listed() -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open("/proc/self/mountinfo") else { return false };
    let mut buf = [0u8; 4096];
    let mut carry = 0usize;
    while let Ok(k) = file.read(&mut buf[carry..]) {
        if k == 0 {
            break;
        }
        let end = carry + k;
        let mut start = 0;
        while let Some(nl) = buf[start..end].iter().position(|c| *c == b'\n') {
            if is_jfs_line(core::str::from_utf8(&buf[start..start + nl]).unwrap_or("")) {
                return true;
            }
            start += nl + 1;
        }
        buf.copy_within(start..end, 0);
        carry = end - start;
        if carry == buf.len() {
            carry = 0;
        }
    }
    false
}

/// A mountinfo line for the JuiceFS mount: "id parent maj:min root MOUNTPOINT opts [optional...] - FSTYPE src opts".
fn is_jfs_line(l: &str) -> bool {
    l.split(' ').nth(4) == Some(jfs_root().as_str())
        && l.split(" - ").nth(1).is_some_and(|t| t.starts_with("fuse.juicefs "))
}

/// Is the host's JuiceFS mount there, waiting up to 30 s for it: sbx-jfs.service remounts within seconds after a
/// juicefs crash (Restart=always), and a sandbox started in that gap should wait rather than fail.
fn jfs_mounted() -> bool {
    for i in 0..60 {
        if jfs_mount_listed() {
            return true;
        }
        if i < 59 {
            crate::util::sleep_ms(500);
        }
    }
    false
}

/// Scratch filesystem + root overlay for a new sandbox (idempotent: a half-made one from a failed create is reused).
/// Returns the root path for the OCI bundle.
pub fn setup(spec: &Spec) -> Res<P> {
    let name = spec.name.as_str();
    if !spec.workspace.is_empty() {
        workspace(spec.workspace.as_str())?;
    }
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
        // SAFETY: `c` is a NUL-terminated path in `b`; mkdir/chmod/chown take no other memory.
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

/// The rsync argv of one scratch round. `last` (the final delta, while the sandbox is stopped) compares file
/// contents (`--checksum`): a write through a `MAP_SHARED` mapping to a page that is already dirty does not change
/// the file's mtime (the kernel updates it only when a clean page is first written), so the quick check (size and
/// mtime) skips such a file and the destination keeps an older copy of it: the supervisor's FUSE journal slots,
/// SQLite's `-shm` (crates/supervisor/GATE.md "WP-57", the short read). The live rounds keep the quick check (a
/// file changed after them is in the final delta either way).
pub fn scratch_argv<'a>(src: &'a str, url: &'a str, last: bool) -> ([&'a str; 12], usize) {
    let mut argv = [
        "rsync",
        "-aHAX",
        "--numeric-ids",
        "--delete",
        "--exclude=/work/**",
        "--exclude=/lost+found",
        "--stats",
        "--timeout=120",
        "--contimeout=10",
        "",
        "",
        "",
    ];
    let mut n = 9;
    if last {
        argv[n] = "--checksum";
        n += 1;
    }
    argv[n] = src;
    argv[n + 1] = url;
    (argv, n + 2)
}

/// Copies the sandbox's scratch filesystem to the destination's relay (`to` = its overlay host:port). `last`: the
/// final delta while the sandbox is stopped (any error fails it; contents compared, `scratch_argv`); before that,
/// files changing or vanishing under rsync (exit 23/24) are expected and the next round picks them up. overlayfs'
/// work directory is not copied.
pub fn sync_scratch(name: &str, to: &str, last: bool, out: &mut [u8]) -> Res<SyncStats> {
    let t0 = crate::util::mono_ms();
    let sd = scratch_dir(name);
    let mut src: P = P::new();
    let _ = src.push(sd.as_str()) && src.push("/");
    let mut url: crate::fixed::Str<120> = crate::fixed::Str::new();
    if !(url.push("rsync://") && url.push(to) && url.push("/scratch/")) {
        return Err(Error::new(500, "rsync url too long"));
    }
    let (a, n) = scratch_argv(src.as_str(), url.as_str(), last);
    let argv = &a[..n];
    let o = crate::proc::run(argv, out, 15 * 60_000)?;
    let ok = o.code == 0 || (!last && (o.code == 23 || o.code == 24));
    if !ok {
        return Err(o.failure(argv));
    }
    let mut st = parse_stats(o.out_str());
    st.ms = crate::util::mono_ms() - t0;
    Ok(st)
}

fn parse_stats(text: &str) -> SyncStats {
    let num = |line: &str| -> u64 {
        let v = line.split_once(':').map_or("", |x| x.1);
        let mut n = 0u64;
        for c in v.trim_start().bytes() {
            match c {
                b'0'..=b'9' => n = n * 10 + u64::from(c - b'0'),
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

/// The conversation's `/workspace` folder must be there: the JuiceFS mount (503 without it, after waiting for a
/// remount) and ws/<ws> in it, made with its quota by hermit (409 `workspace missing` otherwise; nothing is created).
fn workspace(ws: &str) -> Res<()> {
    if !jfs_mounted() {
        return Err(Error::new(503, "the workspace store (JuiceFS) is not mounted on this host"));
    }
    match std::fs::metadata(workspace_dir(ws).as_str()) {
        Ok(m) if m.is_dir() => Ok(()),
        _ => Err(Error::new(409, "workspace missing")),
    }
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
    fn final_delta_compares_contents() {
        let (a, n) = scratch_argv("/s/", "rsync://10.0.0.2:7100/scratch/", true);
        assert_eq!(&a[n - 3..n], &["--checksum", "/s/", "rsync://10.0.0.2:7100/scratch/"]);
        assert!(a[..n].contains(&"--delete") && a[..n].contains(&"-aHAX"));
        let (a, n) = scratch_argv("/s/", "rsync://10.0.0.2:7100/scratch/", false);
        assert!(!a[..n].contains(&"--checksum"), "the live rounds keep the quick check");
        assert_eq!(&a[n - 2..n], &["/s/", "rsync://10.0.0.2:7100/scratch/"]);
    }

    #[test]
    fn rsync_stats() {
        let t = "Number of files: 1,203 (reg: 1,000, dir: 200, dev: 3)\nNumber of created files: 10\n\
                 Number of deleted files: 4 (reg: 4)\nNumber of regular files transferred: 1,042\n\
                 Total file size: 123,456,789 bytes\nTotal transferred file size: 12,345,678 bytes\n";
        assert_eq!(parse_stats(t), SyncStats { files: 1042, bytes: 12_345_678, deleted: 4, ms: 0 });
        assert_eq!(parse_stats(""), SyncStats::default());
    }

    #[test]
    fn jfs_mountinfo_line() {
        assert!(is_jfs_line(
            "512 29 0:52 / /var/lib/sbx/jfs rw,relatime shared:280 - fuse.juicefs JuiceFS:sbx rw,user_id=0,group_id=0"
        ));
        assert!(!is_jfs_line("29 1 259:1 / / rw,relatime shared:1 - ext4 /dev/root rw,discard"));
        assert!(!is_jfs_line("530 29 0:60 / /var/lib/sbx/jfs/x rw shared:9 - fuse.juicefs JuiceFS:sbx rw"));
        assert!(!is_jfs_line("531 29 7:3 / /var/lib/sbx/jfs rw shared:9 - ext4 /dev/loop3 rw"));
        assert!(!is_jfs_line(""));
    }
}
