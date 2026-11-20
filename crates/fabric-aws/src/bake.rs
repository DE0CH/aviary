//! The two subcommands that run programs beside the AWS CLI: `ami` (a builder over SSH) and `publish-image` (crane,
//! tar, zstd). They are checked live (the deployment's AMI workflow), not offline.

use crate::aws::{Aws, call};
use crate::cmds::{self, wait_for};
use crate::config::Config;
use crate::plan::{self, SSH_RULE};
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A registry login for crane and the builder, when the image is private (a public one needs none).
#[derive(Debug, Clone)]
pub struct Ghcr {
    pub user: String,
    pub token: String,
}

impl Ghcr {
    fn auth_json(&self) -> String {
        format!(
            r#"{{"auths":{{"ghcr.io":{{"auth":"{}"}}}}}}"#,
            b64(format!("{}:{}", self.user, self.token).as_bytes())
        )
    }
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(T[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Result<TempDir> {
        let p = std::env::temp_dir().join(format!("fabric-aws-{tag}-{}-{}", std::process::id(), now_secs()));
        std::fs::create_dir_all(&p)?;
        Ok(TempDir(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `crane` with the registry login, if any, in a configuration directory of its own.
fn crane(ghcr: Option<&Ghcr>) -> Result<(Command, Option<TempDir>)> {
    let mut c = Command::new("crane");
    let dir = if let Some(g) = ghcr {
        let d = TempDir::new("crane")?;
        std::fs::write(d.0.join("config.json"), g.auth_json())?;
        c.env("DOCKER_CONFIG", &d.0);
        Some(d)
    } else {
        None
    };
    Ok((c, dir))
}

/// An image reference's manifest digest (64 hex): from the reference when pinned by digest, else the registry's.
///
/// # Errors
/// crane failed or answered no digest.
pub fn digest_of(image: &str, ghcr: Option<&Ghcr>) -> Result<String> {
    if let Some(d) = plan::pinned_digest(image) {
        return Ok(d.to_owned());
    }
    let (mut c, _dir) = crane(ghcr)?;
    let out = c.args(["digest", image]).output().context("running crane")?;
    if !out.status.success() {
        bail!("crane digest {image}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let d = String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_start_matches("sha256:")
        .to_owned();
    if d.len() != 64 || !d.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("no digest for {image}");
    }
    Ok(d)
}

fn run(c: &mut Command, what: &str) -> Result<String> {
    let out = c
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running {what}"))?;
    if !out.status.success() {
        bail!("{what} failed ({})", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a pipeline `a | b`, both checked.
fn pipe(mut a: Command, mut b: Command, what: &str) -> Result<()> {
    let mut first = a
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {what}"))?;
    let stdout = first.stdout.take().context("no pipe")?;
    let second = b.stdin(stdout).status().with_context(|| format!("running {what}"))?;
    let first = first.wait()?;
    if !first.success() || !second.success() {
        bail!("{what} failed ({first}, {second})");
    }
    Ok(())
}

fn as_root(args: &[&str]) -> Command {
    // SAFETY-free check of the effective uid without libc: /proc/self/status's Uid line
    let root = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .map(|l| l.split_whitespace().nth(2) == Some("0"))
        })
        .unwrap_or(false);
    let mut c = if root {
        Command::new(args[0])
    } else {
        Command::new("sudo")
    };
    if root {
        c.args(&args[1..]);
    } else {
        c.args(args);
    }
    c
}

/// `publish-image [image]`: puts the sandbox image (`sandbox.image` unless given) in the bucket as
/// `images/<digest>.tar.zst`, unpacked as sandboxd keeps it (`rootfs/`, `config.json`, `.done`); nothing when it is
/// already there. Needs crane, zstd and root (sudo) for the files' owners.
///
/// # Errors
/// A program or AWS call failed.
pub fn publish_image(aws: &dyn Aws, cfg: &Config, image: &str, ghcr: Option<&Ghcr>, out: &mut dyn Write) -> Result<()> {
    let bucket = cfg.bucket()?;
    let d = digest_of(image, ghcr)?;
    let key = format!("images/{d}.tar.zst");
    if call(aws, ["s3api", "head-object", "--bucket", bucket, "--key", &key]).is_ok() {
        writeln!(out, "s3://{bucket}/{key} is there")?;
        return Ok(());
    }
    let tmp = TempDir::new("publish")?;
    let rootfs = tmp.0.join("rootfs");
    std::fs::create_dir(&rootfs)?;
    let src = format!("{}@sha256:{d}", plan::repository(image));
    // as sandboxd unpacks an image (crates/sandboxd/src/images.rs) and as its hub serves one (api.rs): rootfs/ with
    // the files' own owners and modes, config.json, .done
    let rootfs_s = rootfs.to_string_lossy().into_owned();
    // a registry stream that breaks mid-way (ghcr's HTTP/2 PROTOCOL_ERROR, 2026-11-20: the image never reached the
    // bucket and every new fabric session failed) is downloaded again into an emptied rootfs
    with_retries(
        DOWNLOAD_ATTEMPTS,
        &|s| std::thread::sleep(Duration::from_secs(s)),
        &mut |attempt| {
            if attempt > 1 {
                let _ = as_root(&["rm", "-rf", &rootfs_s]).status();
                std::fs::create_dir_all(&rootfs)?;
            }
            let (mut export, _d1) = crane(ghcr)?;
            export.args(["export", &src, "-"]);
            pipe(
                export,
                as_root(&["tar", "-x", "-C", &rootfs_s]),
                "crane export | tar -x",
            )
        },
    )?;
    let (mut config, _d2) = crane(ghcr)?;
    std::fs::write(
        tmp.0.join("config.json"),
        run(config.args(["config", &src]), "crane config")?,
    )?;
    std::fs::write(tmp.0.join(".done"), "")?;
    // S3 shows an object only once its upload is whole: a host never reads half an image
    let tmp_s = tmp.0.to_string_lossy().into_owned();
    let mut cp = Command::new("aws");
    cp.args([
        "--region",
        &cfg.region,
        "s3",
        "cp",
        "-",
        &format!("s3://{bucket}/{key}"),
        "--no-progress",
    ]);
    pipe(
        as_root(&["tar", "--zstd", "-C", &tmp_s, "-cf", "-", "."]),
        cp,
        "tar --zstd | aws s3 cp",
    )?;
    // the unpacked files are root's: remove them as root before the TempDir goes
    let _ = as_root(&["rm", "-rf", &tmp_s]).status();
    let v = call(aws, ["s3api", "head-object", "--bucket", bucket, "--key", &key])?;
    writeln!(out, "s3://{bucket}/{key} published ({} bytes)", v["ContentLength"])?;
    Ok(())
}

/// How many times a broken image download is tried.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// Runs `step` until it succeeds, at most `attempts` times, sleeping 10 s, then 20 s, … between tries; the last error
/// if none did.
///
/// # Errors
/// Every attempt failed.
pub fn with_retries(attempts: u32, sleep: &dyn Fn(u64), step: &mut dyn FnMut(u32) -> Result<()>) -> Result<()> {
    let mut last = None;
    for attempt in 1..=attempts {
        match step(attempt) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "attempt {attempt} of {attempts}: {e:#}");
                last = Some(e);
                if attempt < attempts {
                    sleep(10 * u64::from(attempt));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no attempt made")))
}

/// What `ami` bakes from.
#[derive(Debug)]
pub struct Bake<'a> {
    /// the sandbox image (`sandbox.image` unless given)
    pub image: String,
    /// the sandboxd release (`fabric.sandboxd` unless given)
    pub sandboxd_release: String,
    /// a local sandboxd build instead of the release (its AMI is tagged `sandboxd=local`)
    pub sandboxd_binary: Option<PathBuf>,
    /// the gVisor fork's release (`fabric.runsc` unless given)
    pub runsc: String,
    /// aviary's `fabric/host` (setup.sh, fetch-sandboxd.sh), copied to the builder
    pub host_dir: PathBuf,
    pub ghcr: Option<&'a Ghcr>,
}

/// The builder and what it holds open, removed when dropped (also on an error or a panic; a killed process leaves
/// them to the next bake's [`cmds::clear_leftovers`]).
struct Builder<'a> {
    aws: &'a dyn Aws,
    sg: String,
    key_pair: String,
    cidr: String,
    instance: Option<String>,
}

impl Drop for Builder<'_> {
    fn drop(&mut self) {
        if let Some(i) = &self.instance {
            let _ = call(self.aws, ["ec2", "terminate-instances", "--instance-ids", i]);
        }
        let _ = cmds::revoke_ssh(self.aws, &self.sg, &self.cidr);
        let _ = call(self.aws, ["ec2", "delete-key-pair", "--key-name", &self.key_pair]);
    }
}

/// `ssh` to the builder with keepalives (a dead connection fails in two minutes, not at the job's timeout) and a
/// time limit on the whole command.
fn ssh(key: &Path, ip: &str, cmd: &str, limit: Duration, stdin: Option<Stdio>) -> Result<String> {
    let mut c = Command::new("timeout");
    c.args(["-k", "10", &limit.as_secs().to_string(), "ssh", "-i"])
        .arg(key)
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=8",
            &format!("ubuntu@{ip}"),
            cmd,
        ]);
    if let Some(i) = stdin {
        c.stdin(i);
    }
    let what = format!("ssh {}", cmd.split_whitespace().take(3).collect::<Vec<_>>().join(" "));
    run(&mut c, &what)
}

/// `ami`: bakes an AMI: Ubuntu 24.04 + `fabric/host/setup.sh burst` (the gVisor fork's release, sandboxd, the
/// sandbox image unpacked) on a temporary t3.large builder (its key pair and SSH rule removed after), tagged with what
/// it holds: `image=<manifest digest>`, `sandboxd=<release>`, `runsc=<release>`, plus the config's tags. Prints the
/// AMI's id last.
///
/// # Errors
/// A step failed or ran past its limit.
#[allow(clippy::too_many_lines)] // one bake, step after step, as the log reads
pub fn ami(aws: &dyn Aws, cfg: &Config, bake: &Bake<'_>, out: &mut dyn Write, log: &mut dyn Write) -> Result<String> {
    let sleep = |s: u64| std::thread::sleep(Duration::from_secs(s));
    let sg = cmds::sg_id(aws, cfg)?.context("no security group: run `sg` first")?;
    cmds::clear_leftovers(aws, cfg, &sg, log)?;
    let t = TempDir::new("ami")?;
    let key = t.0.join("key");
    run(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", ""])
            .arg("-f")
            .arg(&key),
        "ssh-keygen",
    )?;
    let kp = format!("{}-ami-{}", cfg.name, now_secs());
    call(
        aws,
        [
            "ec2",
            "import-key-pair",
            "--key-name",
            &kp,
            "--public-key-material",
            &format!("fileb://{}.pub", key.display()),
        ],
    )?;
    let myip = run(
        Command::new("curl").args(["-fsS", "https://checkip.amazonaws.com"]),
        "curl checkip",
    )?
    .trim()
    .to_owned();
    let cidr = format!("{myip}/32");
    let mut b = Builder {
        aws,
        sg: sg.clone(),
        key_pair: kp.clone(),
        cidr: cidr.clone(),
        instance: None,
    };
    call(
        aws,
        [
            "ec2",
            "authorize-security-group-ingress",
            "--group-id",
            &sg,
            "--ip-permissions",
            &format!("IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{{CidrIp={cidr},Description={SSH_RULE}}}]"),
        ],
    )?;
    let ubuntu = call(
        aws,
        [
            "ssm",
            "get-parameter",
            "--name",
            "/aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id",
        ],
    )?["Parameter"]["Value"]
        .as_str()
        .context("no Ubuntu AMI from SSM")?
        .to_owned();
    let v = call(
        aws,
        [
            "ec2",
            "run-instances",
            "--image-id",
            &ubuntu,
            "--instance-type",
            "t3.large",
            "--key-name",
            &kp,
            "--subnet-id",
            &cfg.subnet,
            "--security-group-ids",
            &sg,
            "--associate-public-ip-address",
            "--metadata-options",
            "HttpTokens=required,HttpPutResponseHopLimit=1",
            "--block-device-mappings",
            "DeviceName=/dev/sda1,Ebs={VolumeSize=16,VolumeType=gp3,DeleteOnTermination=true}",
            "--tag-specifications",
            &plan::tagspec(
                cfg,
                "instance",
                &[
                    ("app", &format!("{}-build", cfg.app)),
                    ("Name", &format!("{}-ami-build", cfg.name)),
                ],
            )
            .to_string(),
        ],
    )?;
    let iid = v["Instances"][0]["InstanceId"]
        .as_str()
        .context("no instance id")?
        .to_owned();
    b.instance = Some(iid.clone());
    writeln!(log, "builder {iid}")?;
    let mut ip = String::new();
    wait_for("builder running", 600, 5, &sleep, log, || {
        let v = call(aws, ["ec2", "describe-instances", "--instance-ids", &iid])?;
        let i = &v["Reservations"][0]["Instances"][0];
        i["PublicIpAddress"].as_str().unwrap_or_default().clone_into(&mut ip);
        let state = i["State"]["Name"].as_str().unwrap_or("?").to_owned();
        Ok((state == "running" && !ip.is_empty(), state))
    })?;
    let started = Instant::now();
    while ssh(&key, &ip, "true", Duration::from_secs(20), None).is_err() {
        if started.elapsed() > Duration::from_secs(600) {
            bail!("the builder {iid} took no SSH in 10 minutes");
        }
        sleep(3);
    }
    ssh(
        &key,
        &ip,
        "cloud-init status --wait >/dev/null 2>&1 || true; mkdir -p sbx",
        Duration::from_mins(15),
        None,
    )?;
    // sandboxd from the pinned aviary release (or a local build), checked against its SHA256SUMS
    let bin = TempDir::new("sandboxd")?;
    let mut fetch = Command::new("bash");
    fetch
        .arg(bake.host_dir.join("fetch-sandboxd.sh"))
        .arg(&bin.0)
        .arg(&bake.sandboxd_release);
    if let Some(p) = &bake.sandboxd_binary {
        fetch.env("SANDBOXD_BINARY", p);
    }
    run(fetch.stdout(Stdio::inherit()), "fetch-sandboxd.sh")?;
    let rel = std::fs::read_to_string(bin.0.join("bin/RELEASE"))?.trim().to_owned();
    let fabric_dir = bake.host_dir.parent().context("host dir has no parent")?;
    let mut tar = Command::new("tar");
    tar.arg("-cf")
        .arg("-")
        .arg("-C")
        .arg(fabric_dir)
        .arg("host")
        .arg("-C")
        .arg(&bin.0)
        .arg("bin");
    let mut tar = tar.stdout(Stdio::piped()).spawn().context("starting tar")?;
    let piped = tar.stdout.take().context("no pipe")?;
    ssh(
        &key,
        &ip,
        "tar -xf - -C sbx",
        Duration::from_secs(300),
        Some(Stdio::from(piped)),
    )?;
    if !tar.wait()?.success() {
        bail!("tar of the host files failed");
    }
    let mut auth = String::new();
    if let Some(g) = bake.ghcr {
        // the registry login exists only for the pull, in a file the setup deletes; it never reaches the AMI
        let mut c = Command::new("timeout");
        c.args(["60", "ssh", "-i"]).arg(&key).args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "LogLevel=ERROR",
            &format!("ubuntu@{ip}"),
            "umask 077; cat > /tmp/ghcr.json",
        ]);
        let mut child = c.stdin(Stdio::piped()).spawn()?;
        child
            .stdin
            .take()
            .context("no stdin")?
            .write_all(g.auth_json().as_bytes())?;
        if !child.wait()?.success() {
            bail!("writing the registry login on the builder failed");
        }
        "REGISTRY_AUTH_FILE=/tmp/ghcr.json".clone_into(&mut auth);
    }
    ssh(
        &key,
        &ip,
        &format!(
            "sudo GV={} {auth} bash sbx/host/setup.sh burst {}; rc=$?; rm -f /tmp/ghcr.json; exit $rc",
            bake.runsc, bake.image
        ),
        Duration::from_mins(45),
        None,
    )?;
    let digest = ssh(
        &key,
        &ip,
        "sudo ls /var/lib/sbx/images | grep -E '^[0-9a-f]{64}$' | head -1",
        Duration::from_secs(60),
        None,
    )?
    .trim()
    .to_owned();
    if digest.len() != 64 {
        bail!("no image on the builder");
    }
    // no way in afterwards: the build key goes, and instances from this AMI get no key pair
    ssh(
        &key,
        &ip,
        "sudo rm -rf /home/ubuntu/sbx /home/ubuntu/.ssh/authorized_keys /root/.ssh/authorized_keys; sudo sync",
        Duration::from_secs(120),
        None,
    )?;
    call(aws, ["ec2", "stop-instances", "--instance-ids", &iid])?;
    wait_for("builder stopped", 900, 10, &sleep, log, || {
        let v = call(aws, ["ec2", "describe-instances", "--instance-ids", &iid])?;
        let state = v["Reservations"][0]["Instances"][0]["State"]["Name"]
            .as_str()
            .unwrap_or("?")
            .to_owned();
        Ok((state == "stopped", state))
    })?;
    let name = format!("{}-{}", cfg.name, utc_stamp());
    let v = call(
        aws,
        [
            "ec2",
            "create-image",
            "--instance-id",
            &iid,
            "--name",
            &name,
            "--description",
            &format!(
                "hermit sandbox host: gVisor fork {}, sandboxd {rel}, sandbox image {}",
                bake.runsc,
                &digest[..12]
            ),
            "--tag-specifications",
            &plan::tagspec(
                cfg,
                "image",
                &[("image", &digest), ("sandboxd", &rel), ("runsc", &bake.runsc)],
            )
            .to_string(),
            &plan::tagspec(cfg, "snapshot", &[]).to_string(),
        ],
    )?;
    let new = v["ImageId"].as_str().context("no image id")?.to_owned();
    // the snapshot of the whole disk: about 15 minutes for 220 GiB; the CLI's own waiter gives up at 10
    wait_for(&format!("AMI {new}"), 90 * 60, 20, &sleep, log, || {
        let v = call(aws, ["ec2", "describe-images", "--image-ids", &new])?;
        let state = v["Images"][0]["State"].as_str().unwrap_or("?").to_owned();
        if state == "failed" {
            bail!("AMI {new} failed: {}", v["Images"][0]["StateReason"]);
        }
        Ok((state == "available", state))
    })?;
    drop(b);
    writeln!(out, "{new}")?;
    Ok(new)
}

/// `YYYYmmdd-HHMM` in UTC, for the AMI's name.
fn utc_stamp() -> String {
    let secs = now_secs();
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (hour, minute) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    // civil from days (Howard Hinnant's algorithm)
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64() {
        assert_eq!(b64(b"u:t"), "dTp0");
        assert_eq!(b64(b"ab"), "YWI=");
        assert_eq!(b64(b"a"), "YQ==");
    }

    #[test]
    fn a_broken_download_is_tried_again_and_the_last_error_kept() {
        let slept = std::cell::RefCell::new(Vec::new());
        let sleep = |s: u64| slept.borrow_mut().push(s);
        let mut seen = Vec::new();
        with_retries(3, &sleep, &mut |n| {
            seen.push(n);
            if n < 3 {
                anyhow::bail!("stream error {n}")
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, vec![1, 2, 3]);
        assert_eq!(*slept.borrow(), vec![10, 20]);
        let e = with_retries(2, &|_| {}, &mut |n| anyhow::bail!("broken {n}")).unwrap_err();
        assert_eq!(e.to_string(), "broken 2");
    }

    #[test]
    fn stamp_shape() {
        let s = utc_stamp();
        assert_eq!(s.len(), 13);
        assert!(s.starts_with("20"));
    }
}
