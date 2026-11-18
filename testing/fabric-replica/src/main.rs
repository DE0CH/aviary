//! fabric-replica: one fabric host on this machine, built the way the deployment builds its hosts, for checks that a
//! fake cannot make (DE0CH/jarvis4 docs/PLAN-fabric.md). It does as little of its own as it can:
//!
//! - the host is the deployment's recipe, unchanged: `fabric/host/fetch-sandboxd.sh` with the release `hermit.json`
//!   pins (`fabric.sandboxd`) and `fabric/host/setup.sh burst` with its gVisor release (`fabric.runsc`) and its sandbox
//!   image (`sandbox.image`), as the AMI bake does (`fabric/aws/infra.sh bake`);
//! - its first boot is the deployment's too: `sbx-userdata` reads the user data hermit's own
//!   `scaler::user_data` makes from that `hermit.json`, from a stand-in of the cloud's metadata service on
//!   169.254.169.254, then `sbx-jfs` and `sandboxd` start as on an instance;
//! - what only the cloud has is stood in for: `JuiceFS`'s metadata and data local (the deployment's are a Postgres and
//!   an object store), an S3 for frozen images (versitygw, signatures checked), the gateway (nothing answers its
//!   `WireGuard` endpoint), the PKI (`fabric/host/make-certs.sh`).
//!
//! Then the checks go through sandboxd's API with hermit's client certificate, on the deployment's image, with
//! `runsc exec` inside the sandbox:
//!   `egress`         the sandbox reaches the internet by name (a session's snapshot upload: 2026-11-18, it did not)
//!   `pause`          paused, a process stops; resumed, it goes on
//!   `freeze`         a paused sandbox evicted to the S3, gone from the host, thawed from it: its process goes on
//!   `delete-paused`  a paused sandbox deleted
//!
//! Usage, as root on a throwaway Ubuntu 24.04 machine (it becomes a fabric host: /var/lib/sbx, its nftables, its NAT):
//!   `fabric-replica up --hermit-json PATH [--aviary DIR]`,  `fabric-replica check [NAME…]`,  `fabric-replica down`

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use sandboxd_api::{Client, Env, Frozen, SandboxState, Spec, ThawRequest, Tls};
use uuid::Uuid;

/// the replica's own state (PKI, the stand-ins' data, the user data)
const DIR: &str = "/var/lib/fabric-replica";
const VERSITYGW: &str = "1.9.0";
const S3_PORT: u16 = 19000;
const BUCKET: &str = "frozen";
const IMDS: &str = "169.254.169.254";
/// runsc's state root as sandboxd runs it (crates/sandboxd `paths`)
const RUNSC_ROOT: &str = "/run/sbx/runsc";

#[derive(Parser)]
#[command(about = "A fabric host on this machine, set up as the deployment sets one up, and checks through sandboxd")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set the machine up as a host of the deployment in --hermit-json and boot it
    Up {
        #[arg(long)]
        hermit_json: PathBuf,
        /// an aviary checkout (the host recipe in fabric/)
        #[arg(long, default_value = ".")]
        aviary: PathBuf,
    },
    /// Run the checks (default: all)
    Check { names: Vec<String> },
    /// Stop the host, delete every sandbox, unmount `JuiceFS`
    Down,
    /// (internal) the metadata service stand-in, serving `--user-data`
    Imds {
        #[arg(long)]
        user_data: PathBuf,
    },
}

fn run(cmd: &mut Command) -> Result<String> {
    let o = cmd.output().with_context(|| format!("{cmd:?}"))?;
    if !o.status.success() {
        bail!(
            "{cmd:?}: {}\n{}\n{}",
            o.status,
            String::from_utf8_lossy(&o.stdout).trim(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn sh(c: &str) -> Result<String> {
    run(Command::new("bash").args(["-c", c]))
}

fn quiet(c: &str) {
    let _ = Command::new("bash").args(["-c", c]).output();
}

fn say(s: &str) {
    eprintln!("fabric-replica: {s}");
}

fn config(hermit_json: &Path) -> Result<hermit::config::Config> {
    let file: hermit::config::HermitJson = serde_json::from_slice(&fs::read(hermit_json)?).context("hermit.json")?;
    hermit::config::Config::from_parts(file, "replica", "replica", "replica").map_err(|e| anyhow::anyhow!("{e}"))
}

// ---- up ------------------------------------------------------------------------------------------------------------

fn up(hermit_json: &Path, aviary: &Path) -> Result<()> {
    if run(Command::new("id").arg("-u"))?.trim() != "0" {
        bail!("run as root: it makes this machine a fabric host");
    }
    let cfg = config(hermit_json)?;
    let f = &cfg.file.fabric;
    let dir = Path::new(DIR);
    fs::create_dir_all(dir)?;
    fs::copy(hermit_json, dir.join("hermit.json"))?;

    say(&format!(
        "the host recipe: sandboxd {}, gVisor {}, image {}",
        f.sandboxd, f.runsc, cfg.file.sandbox.image
    ));
    let sbx = dir.join("sbx");
    quiet(&format!("rm -rf {}", sbx.display()));
    run(Command::new("cp").arg("-r").arg(aviary.join("fabric")).arg(&sbx))?;
    run(Command::new("bash")
        .arg(sbx.join("host/fetch-sandboxd.sh"))
        .arg(&sbx)
        .arg(&f.sandboxd))?;
    quiet("systemctl stop sandboxd sbx-jfs; umount -l /var/lib/sbx/jfs; rm -f /etc/sbx/host.json");
    run(Command::new("bash")
        .arg(sbx.join("host/setup.sh"))
        .args(["burst", &cfg.file.sandbox.image])
        .env("GV", &f.runsc))?;

    say("stand-ins: PKI, JuiceFS metadata and data, the S3, the metadata service");
    let pki = dir.join("pki");
    if !pki.join("hermit.crt").exists() {
        run(Command::new("bash")
            .arg(aviary.join("fabric/host/make-certs.sh"))
            .arg(&pki)
            .args(["host", "hermit"]))?;
    }
    let meta = format!("sqlite3://{}/jfs.db", dir.display());
    if !dir.join("jfs.db").exists() {
        run(Command::new("juicefs")
            .args(["format", "--no-update", "--storage", "file", "--bucket"])
            .arg(dir.join("jfs-data/"))
            .args([meta.as_str(), "replica"]))?;
    }
    s3(dir)?;
    // the instance's user data, as hermit's scaler makes it (its WireGuard address: the host range's .10)
    let host_net: Vec<u8> = f
        .host_cidr
        .split('/')
        .next()
        .unwrap_or_default()
        .split('.')
        .filter_map(|x| x.parse().ok())
        .collect();
    if host_net.len() != 4 {
        bail!("hermit.json fabric.hostCidr {}", f.host_cidr);
    }
    let wg_ip: std::net::IpAddr = format!("{}.{}.{}.10", host_net[0], host_net[1], host_net[2]).parse()?;
    let private_key = sh("wg genkey")?.trim().to_owned();
    let gateway_key = sh("wg genkey | wg pubkey")?.trim().to_owned();
    let rd = |n: &str| fs::read_to_string(pki.join(n)).with_context(|| n.to_owned());
    let secrets = hermit::fabric::gvisor::HostSecrets {
        ca: rd("ca.crt")?,
        cert: rd("host.crt")?,
        key: rd("host.key")?,
        juicefs_meta_url: meta.clone(),
        juicefs_meta_password: String::new(),
    };
    let ud = hermit::fabric::scaler::user_data(&cfg, "replica", wg_ip, &private_key, &gateway_key, &secrets);
    let ud_path = dir.join("user-data.json");
    fs::write(&ud_path, serde_json::to_vec(&ud)?)?;
    fs::write(dir.join("wg-ip"), wg_ip.to_string())?;
    quiet(&format!("ip addr add {IMDS}/32 dev lo"));
    quiet("systemctl stop fabric-replica-imds; systemctl reset-failed fabric-replica-imds");
    run(Command::new("systemd-run")
        .args(["--unit", "fabric-replica-imds"])
        .arg(std::env::current_exe()?)
        .args(["imds", "--user-data"])
        .arg(&ud_path))?;
    // the S3 of frozen images is the stand-in: sandboxd's own test switch for another endpoint
    fs::create_dir_all("/etc/systemd/system/sandboxd.service.d")?;
    fs::write(
        "/etc/systemd/system/sandboxd.service.d/fabric-replica.conf",
        format!("[Service]\nEnvironment=SBX_S3_ENDPOINT=http://127.0.0.1:{S3_PORT}\n"),
    )?;
    sh("systemctl daemon-reload")?;

    ipv4_default()?;
    say("first boot: sbx-userdata, sbx-jfs, sandboxd");
    // as at an instance's first boot (restarted: a run before left the oneshot unit active)
    sh("systemctl restart sbx-userdata.service && systemctl restart sbx-jfs.service sandboxd.service")?;
    boot_wait()
}

/// A cloud host has an IPv4 default route in the main table (sandboxd takes its interface as the public one); a
/// machine that reaches IPv4 only by policy routing (Cloudflare WARP on an IPv6-only server) gets one through the
/// interface that carries it: the stand-in of the cloud's IPv4.
fn ipv4_default() -> Result<()> {
    if sh("ip -4 route show default")?.trim().is_empty() {
        let via = sh("ip -4 route get 1.1.1.1")?;
        let dev = via
            .split(" dev ")
            .nth(1)
            .and_then(|r| r.split_whitespace().next())
            .context("no IPv4 route to the internet")?;
        say(&format!("the IPv4 default route: dev {dev}"));
        sh(&format!("ip -4 route add default dev {dev}"))?;
    }
    Ok(())
}

/// `JuiceFS` mounted and sandboxd answering, as after an instance's boot
fn boot_wait() -> Result<()> {
    let t0 = Instant::now();
    while !sh("grep -c ' /var/lib/sbx/jfs ' /proc/self/mountinfo || true")?
        .trim()
        .starts_with('1')
    {
        if t0.elapsed() > Duration::from_secs(60) {
            bail!("JuiceFS did not mount (journalctl -u sbx-jfs)");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let c = client()?;
    let rt = tokio::runtime::Runtime::new()?;
    loop {
        match rt.block_on(c.host()) {
            Ok(h) => {
                say(&format!("sandboxd answers: {} ({:?}), up", h.name, h.role));
                return Ok(());
            }
            Err(_) if t0.elapsed() < Duration::from_secs(120) => std::thread::sleep(Duration::from_secs(2)),
            Err(e) => bail!("sandboxd does not answer: {e} (journalctl -u sandboxd)"),
        }
    }
}

/// the S3 frozen images go to: versitygw on loopback, signatures checked against the replica's own key
fn s3(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir.join("s3").join(BUCKET))?;
    if !Path::new("/usr/local/bin/versitygw").exists() {
        let t = dir.join("versitygw");
        fs::create_dir_all(&t)?;
        sh(&format!(
            "cd {t} && curl -fsSL -o v.tgz https://github.com/versity/versitygw/releases/download/v{VERSITYGW}/versitygw_v{VERSITYGW}_Linux_x86_64.tar.gz \
             && tar -xzf v.tgz && install -m755 $(find . -name versitygw -type f | head -1) /usr/local/bin/",
            t = t.display()
        ))?;
    }
    quiet("systemctl stop fabric-replica-s3; systemctl reset-failed fabric-replica-s3");
    run(Command::new("systemd-run")
        .args(["--unit", "fabric-replica-s3"])
        .args([
            "--setenv=ROOT_ACCESS_KEY=replica",
            "--setenv=ROOT_SECRET_KEY=replica-secret",
        ])
        .args([
            "/usr/local/bin/versitygw",
            "--port",
            &format!("127.0.0.1:{S3_PORT}"),
            "posix",
        ])
        .arg(dir.join("s3")))?;
    Ok(())
}

/// the cloud's metadata service, as much of it as `sbx-userdata` asks: a token, then the user data
fn imds(user_data: &Path) -> Result<()> {
    let body = fs::read(user_data)?;
    let l = std::net::TcpListener::bind((IMDS, 80))?;
    for c in l.incoming() {
        let Ok(mut c) = c else { continue };
        let mut req = [0u8; 4096];
        let n = c.read(&mut req).unwrap_or(0);
        let line = String::from_utf8_lossy(&req[..n])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let out: &[u8] = if line.contains("/latest/api/token") {
            b"replica-token"
        } else if line.contains("/latest/user-data") {
            &body
        } else {
            b""
        };
        let _ = write!(
            c,
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            out.len()
        );
        let _ = c.write_all(out);
    }
    Ok(())
}

// ---- checks --------------------------------------------------------------------------------------------------------

fn client() -> Result<Client> {
    let pki = Path::new(DIR).join("pki");
    let rd = |n: &str| fs::read_to_string(pki.join(n)).with_context(|| n.to_owned());
    let tls = Tls {
        ca_pem: rd("ca.crt")?,
        cert_pem: rd("hermit.crt")?,
        key_pem: rd("hermit.key")?,
    };
    let ip = fs::read_to_string(Path::new(DIR).join("wg-ip"))?;
    Ok(Client::new(ip.trim().parse()?, sandboxd_api::PORT, &tls)?)
}

/// a sandbox as hermit asks for one (the deployment's image, cpu, memory and mounts; a conversation's folders on
/// `JuiceFS`, made as hermit makes them), at the sandbox address `.1.n`
fn spec(cfg: &hermit::config::Config, name: &str, n: u8) -> Result<Spec> {
    let s = &cfg.file.sandbox;
    let cid = Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes());
    let o: Vec<&str> = cfg
        .file
        .fabric
        .sandbox_cidr
        .split('/')
        .next()
        .unwrap_or_default()
        .split('.')
        .collect();
    let mounts = hermit::fabric::gvisor::spec_mounts(&s.mounts, cid);
    for d in std::iter::once(format!("ws/{cid}")).chain(mounts.iter().map(|m| m.folder.clone())) {
        sh(&format!("mkdir -p /var/lib/sbx/jfs/{d}"))?;
    }
    let image = s
        .image
        .rsplit_once("@sha256:")
        .map(|x| x.1)
        .unwrap_or_default()
        .to_owned();
    Ok(Spec {
        name: name.into(),
        ip: format!("{}.{}.1.{n}", o[0], o[1]),
        image,
        image_ref: Some(s.image.clone()),
        env: Env(Vec::new()),
        cpu: f64::from(s.cpu),
        memory: cfg.memory_bytes,
        cpu_features: String::new(),
        workspace: Some(cid.to_string()),
        drive: None,
        pids: Some(s.pids),
        scratch_bytes: None,
        mounts,
    })
}

/// a command inside the sandbox (its first container), its output
fn exec(name: &str, cmd: &str) -> Result<String> {
    run(Command::new("runsc").args(["--root", RUNSC_ROOT, "exec", name, "sh", "-c", cmd]))
}

const COUNTER: &str =
    "i=$(cat /tmp/replica-n 2>/dev/null || echo 0); while :; do i=$((i+1)); echo $i > /tmp/replica-n; sleep 1; done";

fn counter(name: &str) -> Result<u64> {
    exec(name, "cat /tmp/replica-n")?.trim().parse().context("the counter")
}

async fn create(c: &Client, cfg: &hermit::config::Config, name: &str, n: u8) -> Result<()> {
    let _ = c.delete(name).await;
    c.create(&spec(cfg, name, n)?)
        .await
        .with_context(|| format!("create {name}"))?;
    Ok(())
}

fn start_counter(name: &str) -> Result<()> {
    // no pipes: the detached process would hold them open, and a read of its output would never end
    let st = Command::new("runsc")
        .args(["--root", RUNSC_ROOT, "exec", "-detach", name, "sh", "-c", COUNTER])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !st.success() {
        bail!("runsc exec -detach {name}: {st}");
    }
    std::thread::sleep(Duration::from_secs(3));
    counter(name).map(|_| ())
}

async fn check_egress(c: &Client, cfg: &hermit::config::Config) -> Result<String> {
    create(c, cfg, "sbx-e9e55", 10).await?;
    let r = exec(
        "sbx-e9e55",
        "curl -sS -m 20 -o /dev/null -w '%{http_code} %{remote_ip}' https://fsn1.your-objectstorage.com/ 2>&1",
    );
    let _ = c.delete("sbx-e9e55").await;
    let out = r?;
    if out.trim().starts_with('0') || out.contains("curl:") {
        bail!("the sandbox does not reach the internet: {}", out.trim());
    }
    Ok(format!("https://fsn1.your-objectstorage.com/ answered {}", out.trim()))
}

async fn check_pause(c: &Client, cfg: &hermit::config::Config) -> Result<String> {
    create(c, cfg, "sbx-ba05e", 11).await?;
    start_counter("sbx-ba05e")?;
    let l = c.pause("sbx-ba05e").await.context("pause")?;
    if l.state != SandboxState::Paused {
        bail!("paused, listed {:?}", l.state);
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    c.resume("sbx-ba05e").await.context("resume")?;
    let a = counter("sbx-ba05e")?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let b = counter("sbx-ba05e")?;
    let _ = c.delete("sbx-ba05e").await;
    if b <= a {
        bail!("the process does not go on after the resume ({a} -> {b})");
    }
    Ok(format!("paused 5 s, went on after the resume ({a} -> {b})"))
}

async fn check_freeze(c: &Client, cfg: &hermit::config::Config) -> Result<String> {
    create(c, cfg, "sbx-f4ee2e", 12).await?;
    start_counter("sbx-f4ee2e")?;
    let before = counter("sbx-f4ee2e")?;
    c.pause("sbx-f4ee2e").await.context("pause")?;
    // a fixed key: the replica's frozen image is test data
    let key = "0a".repeat(32);
    let frozen = Frozen {
        object: format!("s3://{BUCKET}/frozen/sbx-f4ee2e/{before}"),
        region: "us-east-1".into(),
        access_key_id: "replica".into(),
        secret_access_key: "replica-secret".into(),
        session_token: "replica-session".into(),
        key,
    };
    let st = c
        .freeze("sbx-f4ee2e", &frozen)
        .await
        .context("freeze (evict to the S3)")?;
    if c.list().await?.sandboxes.iter().any(|s| s.name == "sbx-f4ee2e") {
        bail!("evicted, still listed on the host");
    }
    let stored = Path::new(DIR)
        .join("s3")
        .join(BUCKET)
        .join(format!("frozen/sbx-f4ee2e/{before}"));
    let bytes = fs::metadata(&stored).map_or(0, |m| m.len());
    if bytes == 0 {
        bail!("no object at {}", stored.display());
    }
    c.thaw(&ThawRequest {
        spec: spec(cfg, "sbx-f4ee2e", 12)?,
        frozen,
    })
    .await
    .context("thaw (restore from the S3)")?;
    let a = counter("sbx-f4ee2e")?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let b = counter("sbx-f4ee2e")?;
    let _ = c.delete("sbx-f4ee2e").await;
    if a < before || b <= a {
        bail!("not the frozen process going on ({before} before, {a} then {b} after the thaw)");
    }
    Ok(format!(
        "evicted in {} ms ({bytes} bytes in the S3), thawed: {before} -> {a} -> {b}",
        st.ms
    ))
}

async fn check_delete_paused(c: &Client, cfg: &hermit::config::Config) -> Result<String> {
    create(c, cfg, "sbx-de1ba05e", 13).await?;
    c.pause("sbx-de1ba05e").await.context("pause")?;
    c.delete("sbx-de1ba05e").await.context("delete")?;
    if c.list().await?.sandboxes.iter().any(|s| s.name == "sbx-de1ba05e") {
        bail!("a paused sandbox survived its delete");
    }
    Ok("a paused sandbox deleted".into())
}

const CHECKS: [&str; 4] = ["egress", "pause", "freeze", "delete-paused"];

fn check(names: &[String]) -> Result<()> {
    let cfg = config(&Path::new(DIR).join("hermit.json"))?;
    let c = client()?;
    let rt = tokio::runtime::Runtime::new()?;
    let all: Vec<String> = CHECKS.iter().map(|s| (*s).to_owned()).collect();
    let names = if names.is_empty() { &all } else { names };
    let mut failed = 0;
    for n in names {
        let r = rt.block_on(async {
            match n.as_str() {
                "egress" => check_egress(&c, &cfg).await,
                "pause" => check_pause(&c, &cfg).await,
                "freeze" => check_freeze(&c, &cfg).await,
                "delete-paused" => check_delete_paused(&c, &cfg).await,
                other => bail!("no check {other} ({})", CHECKS.join(", ")),
            }
        });
        match r {
            Ok(s) => say(&format!("ok   {n}: {s}")),
            Err(e) => {
                failed += 1;
                say(&format!("FAIL {n}: {e:#}"));
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} checks failed", names.len());
    }
    Ok(())
}

fn down() -> Result<()> {
    if let Ok(c) = client() {
        let rt = tokio::runtime::Runtime::new()?;
        if let Ok(l) = rt.block_on(c.list()) {
            for s in l.sandboxes {
                let _ = rt.block_on(c.delete(&s.name));
            }
        }
    }
    quiet("systemctl stop sandboxd sbx-jfs fabric-replica-s3 fabric-replica-imds; umount -l /var/lib/sbx/jfs");
    quiet("systemctl reset-failed fabric-replica-s3 fabric-replica-imds");
    say("down (sandboxd, JuiceFS and the stand-ins stopped; /var/lib/fabric-replica kept)");
    Ok(())
}

fn main() -> std::process::ExitCode {
    let r = match Cli::parse().cmd {
        Cmd::Up { hermit_json, aviary } => up(&hermit_json, &aviary),
        Cmd::Check { names } => check(&names),
        Cmd::Down => down(),
        Cmd::Imds { user_data } => imds(&user_data),
    };
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fabric-replica: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
