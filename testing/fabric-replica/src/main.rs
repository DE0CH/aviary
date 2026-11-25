//! fabric-replica: one fabric host on this machine, built the way the deployment builds its hosts, for checks that a
//! fake cannot make (DE0CH/jarvis4 docs/PLAN-fabric.md). It does as little of its own as it can:
//!
//! - the host is the deployment's recipe, unchanged: `fabric/host/fetch-sandboxd.sh` with the release `hermit.json`
//!   pins (`fabric.sandboxd`) and `fabric/host/setup.sh burst` with its gVisor release (`fabric.runsc`) and its sandbox
//!   image (`sandbox.image`), as the AMI bake does (`fabric-aws ami`);
//! - its first boot is the deployment's too: `sbx-userdata` reads the user data hermit's own
//!   `scaler::user_data` makes from that `hermit.json`, from a stand-in of the cloud's metadata service on
//!   169.254.169.254, then `sbx-jfs` and `sandboxd` start as on an instance;
//! - what only the cloud has is stood in for: `JuiceFS`'s metadata and data local (the deployment's are a Postgres and
//!   an object store), an S3 for frozen images with STS session credentials (moto's server: signatures and tokens
//!   checked; its `AssumeRole` stands in for hermit's `GetFederationToken`, both an opaque token to sandboxd), the
//!   gateway (nothing answers its
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
/// moto, the S3 and STS stand-in (its server, pinned)
const MOTO: &str = "5.2.5";
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

/// the S3 frozen images go to: moto's server on loopback, which checks every signature and session token after the
/// three unsigned calls that make its one user (`INITIAL_NO_AUTH_ACTION_COUNT`); that user then makes the role whose
/// sessions `creds` hands out, and the bucket. Its state is in memory: every `up` makes it anew.
fn s3(dir: &Path) -> Result<()> {
    let venv = dir.join("moto");
    if !venv.join("bin/moto_server").exists() {
        // venv makes its pip with ensurepip, which Ubuntu ships apart (python3-venv)
        if Command::new("python3")
            .args(["-c", "import ensurepip"])
            .output()
            .map_or(true, |o| !o.status.success())
        {
            sh("DEBIAN_FRONTEND=noninteractive apt-get install -y -q python3-venv")?;
        }
        let _ = fs::remove_dir_all(&venv);
        run(Command::new("python3").args(["-m", "venv"]).arg(&venv))?;
        run(Command::new(venv.join("bin/pip")).args(["install", "-q", &format!("moto[server]=={MOTO}")]))?;
    }
    quiet("systemctl stop fabric-replica-s3; systemctl reset-failed fabric-replica-s3");
    run(Command::new("systemd-run")
        .args(["--unit", "fabric-replica-s3", "--setenv=INITIAL_NO_AUTH_ACTION_COUNT=3"])
        .arg(venv.join("bin/moto_server"))
        .args(["-H", "127.0.0.1", "-p", &S3_PORT.to_string()]))?;
    let t = Instant::now();
    while sh(&format!(
        "curl -s -o /dev/null -w %{{http_code}} http://127.0.0.1:{S3_PORT}/moto-api/"
    ))
    .map_or(true, |c| c.trim() != "200")
    {
        if t.elapsed() > Duration::from_secs(60) {
            bail!("moto did not answer in 60 s (journalctl -u fabric-replica-s3)");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let anyone = ("bootstrap", "unsigned");
    aws(
        "iam",
        anyone,
        None,
        &[("Action", "CreateUser"), ("UserName", "replica")],
    )?;
    aws(
        "iam",
        anyone,
        None,
        &[
            ("Action", "PutUserPolicy"),
            ("UserName", "replica"),
            ("PolicyName", "all"),
            ("PolicyDocument", ALLOW_ALL),
        ],
    )?;
    let k = aws(
        "iam",
        anyone,
        None,
        &[("Action", "CreateAccessKey"), ("UserName", "replica")],
    )?;
    let (id, secret) = (tag(&k, "AccessKeyId")?, tag(&k, "SecretAccessKey")?);
    fs::write(dir.join("s3-user"), format!("{id}\n{secret}\n"))?;
    let user = (id.as_str(), secret.as_str());
    let trust = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"*"},"Action":"sts:AssumeRole"}]}"#;
    aws(
        "iam",
        user,
        None,
        &[
            ("Action", "CreateRole"),
            ("RoleName", "freeze"),
            ("AssumeRolePolicyDocument", trust),
        ],
    )?;
    aws(
        "iam",
        user,
        None,
        &[
            ("Action", "PutRolePolicy"),
            ("RoleName", "freeze"),
            ("PolicyName", "all"),
            ("PolicyDocument", ALLOW_ALL),
        ],
    )?;
    let code = s3_call(user, None, "PUT", BUCKET)?;
    if code != "200" {
        bail!("the bucket: HTTP {code}");
    }
    Ok(())
}

const ALLOW_ALL: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#;

/// an IAM or STS call to the stand-in, signed (`SigV4` by curl); the body form-encoded as the AWS SDKs encode it (moto
/// checks the signature over its own re-encoding of the form)
fn aws(service: &str, key: (&str, &str), token: Option<&str>, params: &[(&str, &str)]) -> Result<String> {
    let version = if service == "sts" { "2011-06-15" } else { "2010-05-08" };
    let body = params
        .iter()
        .chain(&[("Version", version)])
        .map(|(k, v)| format!("{}={}", form(k), form(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sS",
        "--aws-sigv4",
        &format!("aws:amz:us-east-1:{service}"),
        "--user",
        &format!("{}:{}", key.0, key.1),
    ]);
    if let Some(t) = token {
        cmd.args(["-H", &format!("x-amz-security-token: {t}")]);
    }
    let out = run(cmd.args(["--data", &body, &format!("http://127.0.0.1:{S3_PORT}/")]))?;
    if out.contains("<ErrorResponse") {
        bail!("{service} {}: {out}", params.first().map_or("", |p| p.1));
    }
    Ok(out)
}

/// an S3 request to the stand-in, signed; its HTTP status
fn s3_call(key: (&str, &str), token: Option<&str>, method: &str, path: &str) -> Result<String> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-s",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "--aws-sigv4",
        "aws:amz:us-east-1:s3",
    ])
    .args(["--user", &format!("{}:{}", key.0, key.1)]);
    if method == "HEAD" {
        cmd.arg("-I");
    } else {
        cmd.args(["-X", method]);
    }
    if let Some(t) = token {
        cmd.args(["-H", &format!("x-amz-security-token: {t}")]);
    }
    run(cmd.arg(format!("http://127.0.0.1:{S3_PORT}/{path}")))
}

/// session credentials for the frozen images (hermit's are an hour's federation token): (id, secret, token)
fn creds() -> Result<(String, String, String)> {
    let u = fs::read_to_string(Path::new(DIR).join("s3-user")).context("the stand-in's user (fabric-replica up)")?;
    let mut l = u.lines();
    let user = (l.next().unwrap_or_default(), l.next().unwrap_or_default());
    let r = aws(
        "sts",
        user,
        None,
        &[
            ("Action", "AssumeRole"),
            ("RoleArn", "arn:aws:iam::123456789012:role/freeze"),
            ("RoleSessionName", "replica"),
            ("DurationSeconds", "3600"),
        ],
    )?;
    Ok((
        tag(&r, "AccessKeyId")?,
        tag(&r, "SecretAccessKey")?,
        tag(&r, "SessionToken")?,
    ))
}

fn form(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// the text of the first `<name>` element of an XML answer
fn tag(xml: &str, name: &str) -> Result<String> {
    let open = format!("<{name}>");
    let i = xml.find(&open).with_context(|| format!("no <{name}> in {xml}"))? + open.len();
    let j = xml[i..].find('<').map_or(xml.len(), |j| i + j);
    Ok(xml[i..j].to_owned())
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
    // running once it has written its first count
    let t = Instant::now();
    while counter(name).is_err() {
        if t.elapsed() > Duration::from_secs(60) {
            bail!("the counter in {name} never wrote");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
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
    // it goes on: a later count (one a second), within a bound it would never need
    let since = Instant::now();
    let mut b = a;
    while b <= a && since.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        b = counter("sbx-ba05e")?;
    }
    let _ = c.delete("sbx-ba05e").await;
    if b <= a {
        bail!("the process does not go on after the resume ({a} -> {b})");
    }
    Ok(format!("paused 5 s, went on after the resume ({a} -> {b})"))
}

/// The tries the sandbox's init has logged on its console (its stderr: /var/log/sbx/<name>.log), from byte `from`
fn init_tries(name: &str, from: usize) -> Result<Vec<u64>> {
    let log = fs::read(format!("/var/log/sbx/{name}.log"))?;
    Ok(String::from_utf8_lossy(log.get(from..).unwrap_or_default())
        .lines()
        .filter_map(|l| l.split("(try ").nth(1)?.split(')').next()?.parse().ok())
        .collect())
}

async fn wait_tries(name: &str, from: usize, secs: u64) -> Result<Vec<u64>> {
    let t = Instant::now();
    loop {
        let v = init_tries(name, from)?;
        if !v.is_empty() || t.elapsed() > Duration::from_secs(secs) {
            return Ok(v);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Evicted to the S3 while paused (as a frozen session is), thawed from it: the same processes go on. The process is
/// the sandbox's init, as a session's are its init's: gVisor does not bring back what `runsc exec` started (they come
/// back killed). The image's init (fabric-entrypoint) given a token but no router fetches its env every 5 s and logs
/// each try on its console: the tries after the thaw go on from the count before it.
async fn check_freeze(c: &Client, cfg: &hermit::config::Config) -> Result<String> {
    let name = "sbx-f4ee2e";
    let _ = c.delete(name).await;
    let _ = fs::remove_file(format!("/var/log/sbx/{name}.log"));
    let mut sp = spec(cfg, name, 12)?;
    // nothing listens on port 9: each try fails at once
    sp.env = Env(vec![(
        "HTTP_PROXY".into(),
        "http://sbx:hst1.replica@127.0.0.1:9".into(),
    )]);
    c.create(&sp).await.with_context(|| format!("create {name}"))?;
    if wait_tries(name, 0, 30).await?.is_empty() {
        bail!("the init logged no try in 30 s (/var/log/sbx/{name}.log)");
    }
    c.pause(name).await.context("pause")?;
    let before = init_tries(name, 0)?.into_iter().max().unwrap_or(0);
    let key = "0a".repeat(32); // a fixed key: the replica's frozen image is test data
    let (id, secret, token) = creds()?;
    let path = format!("{BUCKET}/frozen/{name}/{}", Uuid::new_v4());
    let frozen = Frozen {
        object: format!("s3://{path}"),
        region: "us-east-1".into(),
        access_key_id: id.clone(),
        secret_access_key: secret.clone(),
        session_token: token.clone(),
        key,
    };
    let st = c.freeze(name, &frozen).await.context("freeze (evict to the S3)")?;
    if c.list().await?.sandboxes.iter().any(|s| s.name == name) {
        bail!("evicted, still listed on the host");
    }
    let stored = s3_call((&id, &secret), Some(&token), "HEAD", &path)?;
    if stored != "200" {
        bail!("no object at s3://{path} (HEAD: {stored})");
    }
    let mark = fs::metadata(format!("/var/log/sbx/{name}.log")).map_or(0, |m| usize::try_from(m.len()).unwrap_or(0));
    c.thaw(&ThawRequest { spec: sp, frozen })
        .await
        .context("thaw (restore from the S3)")?;
    let after = wait_tries(name, mark, 15).await?;
    let _ = c.delete(name).await;
    match after.first() {
        Some(&n) if n > before => Ok(format!(
            "evicted to the S3 in {} ms, thawed from it: the init's tries went on {before} -> {n}",
            st.ms
        )),
        Some(&n) => bail!("a new init, not the frozen one: try {n} after the thaw, {before} before"),
        None => bail!("the init logged nothing in 15 s after the thaw ({before} tries before)"),
    }
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
