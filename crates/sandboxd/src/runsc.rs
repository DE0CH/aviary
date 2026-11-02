//! gVisor (the DE0CH/gvisor fork's runsc with pre-copy live migration): flags and the OCI bundle of a sandbox.
//! Flags are identical for every sandbox on every host (a migration restores with the source's configuration).
//! `--overlay2=none`: the root's writable layer is a Linux overlayfs on the host (storage.rs), plain files outside
//! gVisor's memory file, so installs don't count as sandbox memory and a migration copies them as files.
//! `--allow-suid`: set-user-ID binaries work inside the sandbox (sudo for package installs, docs/HOST.md
//! "Installing packages"); a uid inside gVisor is never a host uid, and the setting is saved in the sandbox's state.
//! FUSE needs no flag: the fork's runsc registers `/dev/fuse` and the FUSE filesystem unconditionally
//! (`runsc/boot/vfs.go`, `fuse.Register`; the FUSE gate ran with exactly these flags, crates/supervisor/GATE.md), so
//! the flags stay as they were on every host (hermit spec 13.2 item 7); the bundle lists the device (`bundle_spec`).
use crate::config::Spec;
use crate::fixed::Buf;
use crate::json::W;
use crate::paths::RUNSC_ROOT;
use crate::proc::runsc_bin;
use crate::util::{Error, Res};

pub const FLAGS: [&str; 10] = [
    "--root",
    RUNSC_ROOT,
    "--platform=systrap",
    "--network=sandbox",
    "--overlay2=none",
    "--host-uds=none",
    "--host-fifo=none",
    "--net-raw",
    "--allow-packet-socket-write",
    "--allow-suid",
];

/// runsc argv: [runsc, FLAGS..., args...] into `out` (returns the used part).
pub fn argv<'a>(out: &'a mut [&'a str; 40], args: &[&'a str]) -> Res<&'a [&'a str]> {
    let n = 1 + FLAGS.len() + args.len();
    if n > out.len() {
        return Err(Error::new(500, "too many runsc arguments"));
    }
    out[0] = runsc_bin();
    out[1..=FLAGS.len()].copy_from_slice(&FLAGS);
    out[1 + FLAGS.len()..n].copy_from_slice(args);
    Ok(&out[..n])
}

/// The capabilities a sandbox's root has INSIDE gVisor (the image's first process: the keeper and the supervisor,
/// dockerd, sudo for package installs); Claude Code runs as uid 1000 with none of them.
const DOCKER_CAPS: [&str; 20] = [
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
    "CAP_NET_BIND_SERVICE",
    "CAP_NET_RAW",
    "CAP_NET_ADMIN",
    "CAP_SYS_CHROOT",
    "CAP_MKNOD",
    "CAP_AUDIT_WRITE",
    "CAP_SETFCAP",
    "CAP_SYS_ADMIN",
    "CAP_SYS_PTRACE",
    "CAP_SYS_RESOURCE",
    "CAP_IPC_LOCK",
    "CAP_SYS_NICE",
];

/// The FUSE device inside the sandbox (the supervisor's `~/.claude` mount, hermit spec 13.2 item 7).
pub const FUSE_MAJOR: u64 = 10;
pub const FUSE_MINOR: u64 = 229;

/// The OCI runtime spec of a sandbox (pure: tested). `image_env` is the image config's Env ("K=V" each), `args` its
/// Entrypoint + Cmd (the first process, never empty: `images::image_config` refuses an image without an
/// entrypoint); `root` is the sandbox's overlay root and `scratch` its scratch filesystem (storage.rs).
pub fn bundle_spec(b: &mut Buf, spec: &Spec, image_env: &[&str], args: &[&str], root: &str, scratch: &str) {
    let mut w = W::new(b);
    w.obj().kstr("ociVersion", "1.0.0").key("annotations").obj();
    if !spec.cpu_features.is_empty() {
        w.kstr("dev.gvisor.internal.cpufeatures", spec.cpu_features.as_str());
    }
    w.end();
    w.key("process").obj();
    w.key("user").obj().ku64("uid", 0).ku64("gid", 0).end();
    w.kstr("cwd", "/work");
    w.key("args").arr();
    for a in args {
        w.str(a);
    }
    w.end_arr();
    // env: the image's, then the spec's, then HOME; a later value replaces an earlier one in place (Map semantics)
    w.key("env").arr();
    for kv in image_env {
        let Some(i) = kv.find('=') else { continue };
        if i == 0 {
            continue;
        }
        let k = &kv[..i];
        if k == "HOME" {
            w.str("HOME=/home/sandbox");
            continue;
        }
        if let Some((_, v)) = spec.env_pairs().find(|(sk, _)| *sk == k) {
            w.pre_str_kv(k, v);
        } else {
            w.str(kv);
        }
    }
    let image_has = |k: &str| image_env.iter().any(|kv| kv.find('=').is_some_and(|i| i > 0 && &kv[..i] == k));
    for (k, v) in spec.env_pairs() {
        if !image_has(k) && k != "HOME" {
            w.pre_str_kv(k, v);
        }
    }
    if !image_has("HOME") {
        w.str("HOME=/home/sandbox");
    }
    w.end_arr();
    w.key("capabilities").obj();
    for set in ["bounding", "effective", "permitted"] {
        w.key(set).arr();
        for c in DOCKER_CAPS {
            w.str(c);
        }
        w.end_arr();
    }
    w.key("inheritable").arr().end_arr().key("ambient").arr().end_arr().end();
    w.key("rlimits").arr().obj().kstr("type", "RLIMIT_NOFILE").ku64("hard", 65536).ku64("soft", 65536).end().end_arr();
    w.end(); // process
    w.key("root").obj().kstr("path", root).key("readonly").bool(false).end();
    w.kstr("hostname", "sandbox");
    w.key("mounts").arr();
    let mount = |w: &mut W, dst: &str, ty: &str, src: &str, opts: &[&str]| {
        w.obj().kstr("destination", dst).kstr("type", ty).kstr("source", src);
        if !opts.is_empty() {
            w.key("options").arr();
            for o in opts {
                w.str(o);
            }
            w.end_arr();
        }
        w.end();
    };
    mount(&mut w, "/proc", "proc", "proc", &[]);
    mount(&mut w, "/dev", "tmpfs", "tmpfs", &["nosuid", "strictatime", "mode=755", "size=65536k"]);
    mount(&mut w, "/dev/pts", "devpts", "devpts", &["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"]);
    mount(&mut w, "/dev/shm", "tmpfs", "shm", &["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]);
    mount(&mut w, "/sys", "sysfs", "sysfs", &["nosuid", "noexec", "nodev", "ro"]);
    mount(&mut w, "/sys/fs/cgroup", "cgroup", "cgroup", &["nosuid", "noexec", "nodev"]);
    mount(&mut w, "/run", "tmpfs", "tmpfs", &["nosuid", "nodev", "mode=755"]);
    // scratch (storage.rs): host files on the sandbox's own 5 GiB filesystem, not memory
    let tmp = crate::paths::join(scratch, "/tmp");
    let docker = crate::paths::join(scratch, "/docker");
    mount(&mut w, "/tmp", "bind", tmp.as_str(), &["rbind", "rw"]);
    mount(&mut w, "/var/lib/docker", "bind", docker.as_str(), &["rbind", "rw"]);
    // the persistent folder on JuiceFS (docs/HOST.md): the conversation's /workspace. No ~/.claude bind (the
    // supervisor's FUSE mount) and no /etc/hosts (the supervisor writes it; hermit spec 13.2 items 2, 4)
    if !spec.workspace.is_empty() {
        let ws = crate::storage::workspace_dir(spec.workspace.as_str());
        mount(&mut w, "/workspace", "bind", ws.as_str(), &["rbind", "rw"]);
    }
    w.end_arr();
    w.key("linux").obj().key("namespaces").arr();
    for t in ["pid", "ipc", "uts", "mount"] {
        w.obj().kstr("type", t).end();
    }
    w.obj().kstr("type", "network").key("path");
    w.str_parts(&["/var/run/netns/", spec.name.as_str()]);
    w.end();
    w.end_arr();
    w.key("devices").arr();
    w.obj()
        .kstr("path", "/dev/fuse")
        .kstr("type", "c")
        .ku64("major", FUSE_MAJOR)
        .ku64("minor", FUSE_MINOR)
        .ku64("fileMode", 0o666)
        .ku64("uid", 0)
        .ku64("gid", 0)
        .end();
    w.end_arr();
    w.key("cgroupsPath");
    w.str_parts(&["/sbx/", spec.name.as_str()]);
    w.key("resources").obj();
    w.key("memory").obj().ku64("limit", spec.memory).end();
    w.key("cpu").obj().key("quota").f64((spec.cpu * 100_000.0).round()).ku64("period", 100_000).end();
    w.key("pids").obj().ku64("limit", spec.pids_limit()).end();
    // the device allow-list: only additions (runsc applies no device cgroup; a runtime that does keeps its defaults)
    w.key("devices").arr();
    w.obj()
        .key("allow")
        .bool(true)
        .kstr("type", "c")
        .ku64("major", FUSE_MAJOR)
        .ku64("minor", FUSE_MINOR)
        .kstr("access", "rwm")
        .end();
    w.end_arr();
    w.end(); // resources
    w.end(); // linux
    w.end();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{self, Val};

    #[test]
    fn pids_from_the_spec() {
        let body = br#"{"name":"sbx-abc","ip":"10.200.0.10","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":1073741824,"pids":512}"#;
        let spec = Spec::parse(body, "10.200.0.0/16").ok().unwrap();
        let mut mem = [0u8; 16384];
        let mut b = Buf::new(&mut mem);
        bundle_spec(&mut b, &spec, &[], &["/init"], "/r", "/s");
        let n = b.len;
        let s = core::str::from_utf8(&mem[..n]).unwrap();
        assert!(s.contains(r#""pids":{"limit":512}"#), "{s}");
        let bad = br#"{"name":"sbx-abc","ip":"10.200.0.10","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","cpu":1,"memory":1073741824,"pids":3}"#;
        assert!(Spec::parse(bad, "10.200.0.0/16").is_err());
    }

    #[test]
    fn flags() {
        // package installs (docs/HOST.md "Installing packages") with the host isolation flags unchanged
        for f in ["--allow-suid", "--host-uds=none", "--host-fifo=none", "--network=sandbox", "--overlay2=none"] {
            assert!(FLAGS.contains(&f), "{f}");
        }
    }

    #[test]
    fn bundle() {
        let body = br#"{"name":"sbx-abc","ip":"10.200.0.10","image":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","env":{"HTTP_PROXY":"http://sbx:t@10.201.1.1:3128","EXEC_TOKEN":"e","PATH":"/x"},"cpu":1.5,"memory":2147483648,"cpuFeatures":"fpu,sse2","workspace":"c0ffee-1"}"#;
        let spec = Spec::parse(body, "10.200.0.0/16").ok().unwrap();
        let mut mem = [0u8; 16384];
        let mut b = Buf::new(&mut mem);
        bundle_spec(
            &mut b,
            &spec,
            &["PATH=/usr/bin", "HOME=/root", "LANG=C"],
            &["/usr/bin/tini", "--", "/opt/keeper", "--serve"],
            "/var/lib/sbx/root/sbx-abc",
            "/var/lib/sbx/scratch/sbx-abc",
        );
        assert!(!b.full);
        let s = b.as_str();
        assert!(json::parse(s.as_bytes()).is_ok(), "{s}");
        assert!(s.contains(r#""dev.gvisor.internal.cpufeatures":"fpu,sse2""#));
        assert!(s.contains(r#""args":["/usr/bin/tini","--","/opt/keeper","--serve"]"#), "{s}");
        assert!(s.contains(r#""user":{"uid":0,"gid":0},"cwd":"/work""#), "{s}");
        assert!(s.contains(r#""env":["PATH=/x","HOME=/home/sandbox","LANG=C","HTTP_PROXY=http://sbx:t@10.201.1.1:3128","EXEC_TOKEN=e"]"#), "{s}");
        assert!(s.contains(r#""limit":2147483648"#));
        assert!(s.contains(r#""quota":150000"#));
        assert!(s.contains(r#""pids":{"limit":4096}"#), "no pids in the spec: the default");
        assert!(s.contains(r#""path":"/var/run/netns/sbx-abc""#));
        assert!(s.contains(r#""cgroupsPath":"/sbx/sbx-abc""#));
        assert!(s.contains(r#""root":{"path":"/var/lib/sbx/root/sbx-abc","readonly":false}"#), "{s}");
        assert!(
            s.contains(r#"{"destination":"/tmp","type":"bind","source":"/var/lib/sbx/scratch/sbx-abc/tmp""#),
            "{s}"
        );
        assert!(
            s.contains(r#"{"destination":"/workspace","type":"bind","source":"/var/lib/sbx/jfs/ws/c0ffee-1""#),
            "{s}"
        );
        // hermit spec 13.2 items 2, 4: no ~/.claude bind, no /etc/hosts
        assert!(!s.contains(".claude") && !s.contains("/etc/hosts") && !s.contains("users/"), "{s}");
        // item 7: /dev/fuse in the sandbox and on the device allow-list
        assert!(
            s.contains(
                r#""devices":[{"path":"/dev/fuse","type":"c","major":10,"minor":229,"fileMode":438,"uid":0,"gid":0}]"#
            ),
            "{s}"
        );
        assert!(s.contains(r#""devices":[{"allow":true,"type":"c","major":10,"minor":229,"access":"rwm"}]"#), "{s}");
        assert!(!s.contains("tmpfs\",\"source\":\"tmpfs\",\"options\":[\"mode=711"));
        let Val::Obj(_) = json::parse(s.as_bytes()).unwrap() else { panic!() };
    }
}
