//! The end-to-end harness (platform 7): the real pigeon, magpie and hermit images, the real sandbox image as Pods
//! made by fake-sandboxd, the reference gateway, mock-model, fake-tools and fake-access, in one k3d cluster on this
//! machine, with Postgres, S3 and a `JuiceFS` mount beside it. The same on a GitHub runner (`ci.yml` `e2e`) and a laptop.
//!
//! ```text
//! e2e-harness up <pigeon image> <magpie image> <hermit image> <aviary-testing image> <hermit-sandbox image>
//! e2e-harness test [scenario…]     the driver Job (testing/e2e/src: every scenario of 7.8 it has, or the named ones)
//! e2e-harness artifacts <dir>      every Pod's logs, events, admin dumps, mock-model's requests, the driver's frames
//! e2e-harness down                 the cluster, the containers and the mount
//! e2e-harness gate <sha12> <sandbox image> <out dir>    nightly's Claude Code release gate (one side)
//! ```
//!
//! Run from the repository's root. Needs docker, k3d, kubectl, openssl and juicefs (1.4.1) on PATH, and sudo for the
//! FUSE mount. Every command has a bound and its exit status is read; a download or a pull is tried again. The work
//! directory is `E2E_WORK` (default /tmp/aviary-e2e). `E2E_PRELOAD=1` copies the cluster's own images (Calico, k3s's)
//! from the local Docker into the node, for a machine whose containers cannot pull.

mod gate;
mod secrets;
mod sh;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use sh::{cmd, hex, kubectl, log, network, wait_for, write_private};

pub const NS: &str = "aviary-e2e";
const CLUSTER: &str = "aviary-e2e";
const NET: &str = "aviary-e2e";
const SUBNET: &str = "172.30.0.0/24";
const PG_IP: &str = "172.30.0.10";
const S3_IP: &str = "172.30.0.11";
/// Both `JuiceFS` mounts live under one folder the node shares (rshared), as in YYC: the hosts' mount (sandboxes'
/// /workspace, root-squash=1000:1000) and hermit's own (all-squash=1000:1000: everything hermit makes is the sandbox
/// user's, hermit spec 15.4).
const MNT: &str = "/mnt/aviary";
const JFS: &str = "/mnt/aviary/jfs";
const JFS_HERMIT: &str = "/mnt/aviary/jfs-hermit";
const K3S_IMAGE: &str = "rancher/k3s:v1.36.5-k3s1";
const PG_IMAGE: &str = "ghcr.io/cloudnative-pg/postgresql:17.11-standard-trixie@sha256:ed6923f9ba2399cf9070379a353c710c29ced8a294be73986a2c3dae7e599ce3";
const S3_IMAGE: &str =
    "versity/versitygw:v1.9.0@sha256:eb7f786fcd91e72194e48fcf1ac16302d686bf8e3a71434bdbc067a6efd3fdc7";
const CALICO_VERSION: &str = "v3.30.3";
const CALICO_SHA256: &str = "9382d2b27a76f40c170454b408653e6d71e2205ef0aef069e942bb690e7381d0";
/// The deployments `up` waits for.
const DEPLOYMENTS: [&str; 9] = [
    "magpie",
    "hermit",
    "pigeon",
    "test-gateway",
    "mock-model",
    "fake-tools",
    "fake-access",
    "fake-ec2",
    "fabric-gateway",
];

#[derive(Parser)]
#[command(about = "The end-to-end harness: a k3d cluster with the images under test")]
struct Cli {
    #[command(subcommand)]
    cmd: Op,
}

#[derive(Subcommand)]
enum Op {
    /// The infrastructure, the cluster, the images and the deployments.
    Up {
        pigeon: String,
        magpie: String,
        hermit: String,
        testing: String,
        sandbox: String,
    },
    /// The driver Job with the named scenarios (all when none); `E2E_DRIVER_SECONDS` bounds it (default 2400).
    Test { scenarios: Vec<String> },
    /// Every Pod's logs, events and the side containers' logs into `dir`.
    Artifacts { dir: PathBuf },
    /// The cluster, the containers, the mounts and the work directory.
    Down,
    /// One side of nightly's Claude Code release gate.
    Gate { sha12: String, image: String, out: PathBuf },
}

/// What `up` and `down` need of the machine.
#[derive(Debug)]
pub struct Env {
    /// testing/e2e (its YAML and files).
    pub here: PathBuf,
    pub work: PathBuf,
    pub kubeconfig: PathBuf,
    pub pg_port: String,
    pub s3_port: String,
    pub preload: bool,
}

/// The passwords `infra` made (the secrets are made from them).
pub struct Infra {
    pub pg_password: String,
    pub s3_access: String,
    pub s3_secret: String,
    pub jfs_password: String,
}

impl std::fmt::Debug for Infra {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Infra { .. }")
    }
}

/// A variable of the harness's own environment (a developer's or the workflow's knobs).
#[allow(clippy::disallowed_methods)] // the harness's command-line environment, not a service's configuration
pub(crate) fn knob(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl Env {
    fn new() -> Result<Self> {
        let here = PathBuf::from("testing/e2e");
        if !here.join("k8s/00-base.yaml").exists() {
            bail!("run e2e-harness from the repository's root (no testing/e2e/k8s here)");
        }
        let here = here.canonicalize()?;
        let work = PathBuf::from(knob("E2E_WORK").unwrap_or_else(|| "/tmp/aviary-e2e".into()));
        Ok(Self {
            here,
            kubeconfig: work.join("kubeconfig"),
            work,
            // the host ports of Postgres and S3 (the JuiceFS mounts use them); E2E_PG_PORT/E2E_S3_PORT on a host
            // whose own services hold the defaults (a shared build machine)
            pg_port: knob("E2E_PG_PORT").unwrap_or_else(|| "15432".into()),
            s3_port: knob("E2E_S3_PORT").unwrap_or_else(|| "19000".into()),
            preload: knob("E2E_PRELOAD").as_deref() == Some("1"),
        })
    }

    fn k(&self, args: &[&str]) -> sh::Cmd {
        kubectl(&self.kubeconfig, &["-n", NS]).args(args)
    }

    fn kube(&self, args: &[&str]) -> sh::Cmd {
        kubectl(&self.kubeconfig, args)
    }
}

fn main() -> Result<()> {
    let e = Env::new()?;
    match Cli::parse().cmd {
        Op::Up {
            pigeon,
            magpie,
            hermit,
            testing,
            sandbox,
        } => up(&e, [&pigeon, &magpie, &hermit, &testing, &sandbox]),
        Op::Test { scenarios } => driver(&e, &scenarios),
        Op::Artifacts { dir } => {
            artifacts(&e, &dir);
            Ok(())
        }
        Op::Down => {
            down(&e);
            Ok(())
        }
        Op::Gate { sha12, image, out } => gate::run(&e, &sha12, &image, &out),
    }
}

pub(crate) fn up(e: &Env, images: [&String; 5]) -> Result<()> {
    let i = infra(e)?;
    cluster(e)?;
    load_images(images)?;
    deploy(e, &i)?;
    // the driver's tokens as one file (the driver reads files, not its environment)
    let secret = e
        .k(&[
            "create",
            "secret",
            "generic",
            "driver-file",
            &format!("--from-file=env={}", e.work.join("driver.env").display()),
            "--dry-run=client",
            "-o",
            "yaml",
        ])
        .run(60)?;
    e.k(&["apply", "-f", "-"]).stdin(secret).run(60)?;
    Ok(())
}

fn infra(e: &Env) -> Result<Infra> {
    std::fs::create_dir_all(&e.work)?;
    if !cmd("docker", &["network", "inspect", NET]).ok(30) {
        cmd(
            "docker",
            &["network", "create", "--subnet", SUBNET, "--gateway", "172.30.0.1", NET],
        )
        .run(60)?;
    }
    let pg_password = hex(16)?;
    let i = Infra {
        jfs_password: pg_password.clone(),
        pg_password,
        s3_access: hex(10)?,
        s3_secret: hex(20)?,
    };
    // Postgres (the production image, TLS on, scram): databases and roles magpie, hermit, jfs
    let pgdir = e.work.join("pg");
    std::fs::create_dir_all(&pgdir)?;
    let p = |s: &str| pgdir.join(s).to_string_lossy().into_owned();
    cmd(
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "2",
            "-subj",
            "/CN=aviary-e2e-postgres",
            "-keyout",
            &p("server.key"),
            "-out",
            &p("server.crt"),
        ],
    )
    .run(120)?;
    std::fs::write(pgdir.join("pw"), format!("{}\n", i.pg_password))?;
    for f in ["server.key", "server.crt", "pw"] {
        set_mode(&pgdir.join(f), 0o644)?;
    }
    set_mode(&pgdir, 0o755)?;
    have_image(PG_IMAGE)?;
    cmd(
        "docker",
        &[
            "run",
            "-d",
            "--name",
            "aviary-e2e-pg",
            "--network",
            NET,
            "--ip",
            PG_IP,
            "-p",
            &format!("127.0.0.1:{}:5432", e.pg_port),
            "-v",
            &format!("{}:/ci:ro", pgdir.display()),
            PG_IMAGE,
            "bash",
            "-ec",
            r#"
    bin=$(ls -d /usr/lib/postgresql/*/bin)
    "$bin/initdb" -D /tmp/pgdata -U postgres --pwfile=/ci/pw -A scram-sha-256 >/dev/null
    cp /ci/server.crt /ci/server.key /tmp/pgdata/ && chmod 0600 /tmp/pgdata/server.key
    printf "local all all trust\nhostssl all all all scram-sha-256\n" > /tmp/pgdata/pg_hba.conf
    exec "$bin/postgres" -D /tmp/pgdata -c listen_addresses="*" -c ssl=on -c fsync=off -c max_connections=300
  "#,
        ],
    )
    .run(120)?;
    wait_for(60, "postgres", || {
        cmd(
            "docker",
            &["exec", "aviary-e2e-pg", "pg_isready", "-q", "-h", "127.0.0.1"],
        )
        .ok(10)
    })?;
    let psql = |db: &str, sql: &[String]| -> Result<()> {
        let mut c = cmd(
            "docker",
            &[
                "exec",
                "aviary-e2e-pg",
                "psql",
                "-q",
                "-h",
                "/var/run/postgresql",
                "-U",
                "postgres",
                "-d",
                db,
                "-v",
                "ON_ERROR_STOP=1",
            ],
        );
        for s in sql {
            c = c.args(&["-c", s]);
        }
        c.run(60).map(|_| ())
    };
    for db in ["magpie", "hermit", "jfs"] {
        psql(
            "postgres",
            &[
                format!("create role {db} login password '{}'", i.pg_password),
                format!("create database {db} owner {db}"),
            ],
        )?;
    }
    // pgvector in magpie's database, as a deployment's superuser creates it (magpie's role cannot; migration 0042)
    psql("magpie", &["create extension if not exists vector".into()])?;
    // S3 (versitygw over a directory; a bucket is a directory)
    let s3dir = e.work.join("s3");
    for b in ["magpie", "jfs"] {
        std::fs::create_dir_all(s3dir.join(b))?;
    }
    cmd("chmod", &["-R", "0777", &s3dir.to_string_lossy()]).run(30)?;
    have_image(S3_IMAGE)?;
    cmd(
        "docker",
        &[
            "run",
            "-d",
            "--name",
            "aviary-e2e-s3",
            "--network",
            NET,
            "--ip",
            S3_IP,
            "-p",
            &format!("127.0.0.1:{}:9000", e.s3_port),
            "-v",
            &format!("{}:/data", s3dir.display()),
            S3_IMAGE,
            "--access",
            &i.s3_access,
            "--secret",
            &i.s3_secret,
            "--region",
            "us-east-1",
            "--port",
            ":9000",
            "posix",
            "/data",
        ],
    )
    .run(120)?;
    let s3 = format!("http://127.0.0.1:{}/", e.s3_port);
    wait_for(60, "s3", || cmd("curl", &["-s", "-o", "/dev/null", &s3]).ok(10))?;
    // JuiceFS: meta in Postgres jfs, data in bucket jfs, mounted where the node and hermit see /workspace folders
    let meta = format!("postgres://jfs@127.0.0.1:{}/jfs?sslmode=require", e.pg_port);
    cmd(
        "juicefs",
        &[
            "format",
            "--storage",
            "s3",
            "--bucket",
            &format!("http://127.0.0.1:{}/jfs", e.s3_port),
            "--access-key",
            &i.s3_access,
            "--secret-key",
            &i.s3_secret,
            &meta,
            "aviary-e2e",
        ],
    )
    .env("META_PASSWORD", &i.jfs_password)
    .run(120)
    .context("juicefs format")?;
    cmd("sudo", &["mkdir", "-p", MNT]).run(30)?;
    if !cmd("mountpoint", &["-q", MNT]).ok(10) {
        cmd("sudo", &["mount", "--bind", MNT, MNT]).run(30)?;
    }
    cmd("sudo", &["mount", "--make-rshared", MNT]).run(30)?;
    cmd("sudo", &["mkdir", "-p", JFS, JFS_HERMIT]).run(30)?;
    for (dir, squash, log) in [
        (JFS, "--root-squash", "juicefs.log"),
        (JFS_HERMIT, "--all-squash", "juicefs-hermit.log"),
    ] {
        cmd(
            "sudo",
            &[
                "env",
                &format!("META_PASSWORD={}", i.jfs_password),
                "juicefs",
                "mount",
                "-d",
                "--log",
                &e.work.join(log).to_string_lossy(),
                squash,
                "1000:1000",
                &meta,
                dir,
            ],
        )
        .run(120)?;
        wait_for(30, &format!("the JuiceFS mount {dir}"), || {
            cmd("mountpoint", &["-q", dir]).ok(10)
        })?;
    }
    // the top folder is the sandbox user's (YYC's one-time jfs-owner Job, hermit spec 15.4)
    cmd("sudo", &["mkdir", "-p", &format!("{JFS}/ws")]).run(30)?;
    cmd("sudo", &["chown", "1000:1000", &format!("{JFS}/ws")]).run(30)?;
    write_private(
        &e.work.join("infra.env"),
        format!(
            "PG_PASSWORD={}\nS3_ACCESS_KEY={}\nS3_SECRET_KEY={}\nJFS_META_PASSWORD={}\n",
            i.pg_password, i.s3_access, i.s3_secret, i.jfs_password
        ),
    )?;
    Ok(i)
}

fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Calico's manifest with the harness's pool: CIDR 10.48.0.0/16, no IP-in-IP.
fn calico_manifest(orig: &str) -> String {
    let mut out = String::with_capacity(orig.len());
    let mut after_ipip = false;
    for line in orig.lines() {
        let mut l = line.replacen("# - name: CALICO_IPV4POOL_CIDR", "- name: CALICO_IPV4POOL_CIDR", 1);
        l = l.replacen("#   value: \"192.168.0.0/16\"", "  value: \"10.48.0.0/16\"", 1);
        if after_ipip {
            l = l.replacen("value: \"Always\"", "value: \"Never\"", 1);
            after_ipip = false;
        }
        if l.contains("name: CALICO_IPV4POOL_IPIP") {
            after_ipip = true;
        }
        out.push_str(&l);
        out.push('\n');
    }
    out
}

fn cluster(e: &Env) -> Result<()> {
    log("cluster");
    // kubelet's eviction at 1% free: its default (10-15%) tainted a hosted runner's node with disk-pressure once the
    // images were imported, leaving every Pod Pending (nightly e2e-chaos, 2026-11-03); a test node evicts nothing
    let volume = format!("{MNT}:{MNT}:rshared@server:0");
    have_image(K3S_IMAGE)?;
    cmd(
        "k3d",
        &[
            "cluster",
            "create",
            CLUSTER,
            "--image",
            K3S_IMAGE,
            "--network",
            NET,
            "--no-lb",
            "--wait",
            "--timeout",
            "300s",
            "--k3s-arg",
            "--flannel-backend=none@server:*",
            "--k3s-arg",
            "--disable-network-policy@server:*",
            "--k3s-arg",
            "--disable=traefik@server:*",
            "--k3s-arg",
            "--cluster-cidr=10.48.0.0/13@server:*",
            "--k3s-arg",
            "--service-cidr=10.43.0.0/16@server:*",
            "--k3s-arg",
            "--kubelet-arg=eviction-hard=memory.available<100Mi,nodefs.available<1%,imagefs.available<1%@server:*",
            "--k3s-arg",
            "--kubelet-arg=eviction-minimum-reclaim=nodefs.available=0,imagefs.available=0@server:*",
            "--volume",
            &volume,
            "--kubeconfig-update-default=false",
        ],
    )
    .run(600)?;
    let kc = cmd("k3d", &["kubeconfig", "get", CLUSTER]).run(60)?;
    write_private(&e.kubeconfig, kc)?;
    // Calico: NetworkPolicy, and Pods at the addresses hermit chose (cni.projectcalico.org/ipAddrs)
    let orig = e.work.join("calico.orig.yaml");
    let url = format!("https://raw.githubusercontent.com/projectcalico/calico/{CALICO_VERSION}/manifests/calico.yaml");
    network("Calico's manifest", || {
        cmd("curl", &["-sSfL", "-o", &orig.to_string_lossy(), &url]).run(120)?;
        let sum = cmd("sha256sum", &[&orig.to_string_lossy()]).run(30)?;
        if !sum.starts_with(CALICO_SHA256) {
            bail!("calico.yaml checksum: {sum}");
        }
        Ok(())
    })?;
    let manifest = calico_manifest(&std::fs::read_to_string(&orig)?);
    if e.preload {
        let mut images: Vec<String> = manifest
            .lines()
            .filter_map(|l| l.trim().strip_prefix("image: "))
            .map(|i| i.trim().to_owned())
            .collect();
        images.sort();
        images.dedup();
        for i in [
            "rancher/mirrored-pause:3.10.2",
            "busybox:1.37.0",
            "rancher/mirrored-coredns-coredns:1.14.7",
            "rancher/local-path-provisioner:v0.0.37",
            "rancher/mirrored-metrics-server:v0.9.0",
        ] {
            images.push(i.into());
        }
        node_import(&images)?;
    }
    e.kube(&["apply", "-f", "-"]).stdin(manifest).run(120)?;
    e.kube(&[
        "-n",
        "kube-system",
        "rollout",
        "status",
        "ds/calico-node",
        "--timeout=290s",
    ])
    .run(300)?;
    e.kube(&["apply", "-f", "-"])
        .stdin(
            "apiVersion: crd.projectcalico.org/v1
kind: IPPool
metadata: {name: fabric}
spec: {cidr: 10.52.0.0/16, ipipMode: Never, vxlanMode: Never, natOutgoing: false, nodeSelector: \"!all()\", blockSize: 26}
",
        )
        .run(60)?;
    Ok(())
}

/// Pulls `image` unless the local Docker has it (a pull is tried again).
fn have_image(image: &str) -> Result<()> {
    if cmd("docker", &["image", "inspect", image]).ok(30) {
        return Ok(());
    }
    network(&format!("pulling {image}"), || {
        cmd("docker", &["pull", "-q", "--platform", "linux/amd64", image])
            .run(900)
            .map(|_| ())
    })
}

/// The node's containerd name of a Docker image name.
fn full_name(image: &str) -> String {
    let parts = image.split('/').count();
    let named = if parts >= 3 {
        image.to_owned()
    } else if parts == 2 {
        format!("docker.io/{image}")
    } else {
        format!("docker.io/library/{image}")
    };
    if named
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains(':') || last.contains('@'))
    {
        named
    } else {
        format!("{named}:latest")
    }
}

/// Imports `images` from the local Docker into the node's containerd, and checks the node has each of them. Streamed
/// straight into the node (`docker save | ctr import`), not through k3d's tools node: k3d's import once lost its
/// tarball between writing and reading it ("open /k3d/images/…tar: no such file", e2e-chaos 2026-11-25) and the
/// deploy went on to `ErrImageNeverPull`.
fn node_import(images: &[String]) -> Result<()> {
    let node = format!("k3d-{CLUSTER}-server-0");
    for i in images {
        have_image(i)?;
        sh::pipe(
            cmd("docker", &["save", i]),
            cmd(
                "docker",
                &[
                    "exec",
                    "-i",
                    &node,
                    "ctr",
                    "-n",
                    "k8s.io",
                    "images",
                    "import",
                    "--platform",
                    "linux/amd64",
                    "-",
                ],
            ),
            900,
        )
        .with_context(|| format!("importing {i} into {node}"))?;
    }
    let listed = cmd("docker", &["exec", &node, "ctr", "-n", "k8s.io", "images", "ls", "-q"]).run(60)?;
    let have: Vec<&str> = listed.lines().collect();
    for i in images {
        let want = full_name(i);
        if !have.contains(&want.as_str()) {
            bail!("{node} does not have {want} after the import");
        }
    }
    Ok(())
}

fn load_images(images: [&String; 5]) -> Result<()> {
    log("images");
    let names = ["pigeon", "magpie", "hermit", "testing", "hermit-sandbox"];
    let mut tagged = Vec::new();
    for (img, name) in images.iter().zip(names) {
        let t = format!("aviary-e2e/{name}:e2e");
        cmd("docker", &["tag", img, &t]).run(60)?;
        tagged.push(t);
    }
    node_import(&tagged)
}

fn deploy(e: &Env, i: &Infra) -> Result<()> {
    log("deploy");
    let digest = cmd(
        "docker",
        &["image", "inspect", "-f", "{{.Id}}", "aviary-e2e/hermit-sandbox:e2e"],
    )
    .run(60)?;
    secrets::generate(&e.here, &e.work, i, digest.trim(), SUBNET, PG_IP, S3_IP)?;
    e.kube(&["apply", "-f", &e.here.join("k8s/00-base.yaml").to_string_lossy()])
        .run(120)?;
    e.kube(&["apply", "-f", &e.work.join("k8s").to_string_lossy()])
        .run(120)?;
    e.kube(&["-n", "kube-system", "rollout", "restart", "deploy/coredns"])
        .run(60)?;
    e.kube(&[
        "apply",
        "-f",
        &e.here.join("k8s/10-services.yaml").to_string_lossy(),
        "-f",
        &e.here.join("k8s/20-network.yaml").to_string_lossy(),
    ])
    .run(120)?;
    for d in DEPLOYMENTS {
        if let Err(err) = e
            .k(&["rollout", "status", &format!("deploy/{d}"), "--timeout=390s"])
            .run(400)
        {
            if let Ok(pods) = e.k(&["get", "pods", "-o", "wide"]).run(60) {
                eprintln!("{pods}");
            }
            return Err(err.context(format!("deployment {d} did not become ready")));
        }
    }
    log("up");
    Ok(())
}

/// The driver Job with the named scenarios (`E2E_DRIVER_SECONDS` bounds it, default 2400).
pub(crate) fn driver(e: &Env, scenarios: &[String]) -> Result<()> {
    e.k(&["delete", "job", "driver", "--ignore-not-found", "--wait=true"])
        .run(300)?;
    let deadline: u64 = knob("E2E_DRIVER_SECONDS")
        .map_or(Ok(2400), |s| s.parse())
        .context("E2E_DRIVER_SECONDS")?;
    let args = scenarios.iter().fold(String::new(), |mut a, s| {
        let _ = std::fmt::Write::write_fmt(&mut a, format_args!(", {}", serde_json::Value::String(s.clone())));
        a
    });
    let job = format!(
        "apiVersion: batch/v1
kind: Job
metadata: {{name: driver, namespace: {NS}}}
spec:
  backoffLimit: 0
  activeDeadlineSeconds: {deadline}
  template:
    metadata: {{labels: {{app: driver}}}}
    spec:
      serviceAccountName: driver
      restartPolicy: Never
      containers:
        - name: driver
          image: aviary-e2e/testing:e2e
          imagePullPolicy: Never
          command: [aviary-e2e, --env-file, /etc/aviary-e2e/env/env{args}]
          env:
            - {{name: SSL_CERT_FILE, value: /etc/aviary-e2e/ca/ca.crt}}
          volumeMounts:
            - {{name: env, mountPath: /etc/aviary-e2e/env, readOnly: true}}
            - {{name: ca, mountPath: /etc/aviary-e2e/ca, readOnly: true}}
            - {{name: jfs, mountPath: {JFS}, readOnly: true, mountPropagation: HostToContainer}}
      volumes:
        - {{name: env, secret: {{secretName: driver-file}}}}
        - {{name: ca, secret: {{secretName: driver-ca}}}}
        - {{name: jfs, hostPath: {{path: {JFS}, type: Directory}}}}
"
    );
    e.k(&["apply", "-f", "-"]).stdin(job).run(60)?;
    let until = Instant::now() + Duration::from_secs(deadline + 50);
    let mut phase = String::new();
    while Instant::now() < until {
        phase = e
            .k(&[
                "get",
                "job",
                "driver",
                "-o",
                "jsonpath={.status.conditions[?(@.status==\"True\")].type}",
            ])
            .run(60)
            .unwrap_or_default();
        if phase.contains("Complete") || phase.contains("Failed") {
            break;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    match e.k(&["logs", "job/driver", "--tail=-1"]).run(300) {
        Ok(l) => print_out(&l),
        Err(err) => eprintln!("e2e: the driver's log: {err:#}"),
    }
    if phase.contains("Complete") {
        log("driver passed");
        Ok(())
    } else {
        bail!("driver: {}", if phase.is_empty() { "timed out" } else { &phase })
    }
}

/// The driver's output goes to standard output, as `kubectl logs` would print it.
fn print_out(s: &str) {
    use std::io::Write as _;
    let _ = std::io::stdout().lock().write_all(s.as_bytes());
}

/// Every Pod's logs, events and the side containers' logs into `dir` (as much as there is: nothing here fails).
pub(crate) fn artifacts(e: &Env, dir: &Path) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let save = |name: &str, c: sh::Cmd| {
        let text = match c.output_within(120) {
            Ok(o) => [o.stdout, o.stderr].concat(),
            Err(err) => format!("{err:#}").into_bytes(),
        };
        let _ = std::fs::write(dir.join(name), text);
    };
    save("pods.txt", e.k(&["get", "pods", "-o", "wide"]));
    save("events.txt", e.k(&["get", "events", "--sort-by=.lastTimestamp"]));
    save(
        "events-all.txt",
        e.kube(&["get", "events", "-A", "--sort-by=.lastTimestamp"]),
    );
    let pods = e.k(&["get", "pods", "-o", "name"]).run(60).unwrap_or_default();
    for p in pods.lines() {
        let base = p.rsplit('/').next().unwrap_or(p);
        save(
            &format!("{base}.log"),
            e.k(&["logs", p, "--all-containers", "--tail=-1"]),
        );
        if let Ok(o) = e
            .k(&["logs", p, "--all-containers", "--previous", "--tail=-1"])
            .output_within(120)
            && o.status.success()
        {
            let _ = std::fs::write(dir.join(format!("{base}.previous.log")), o.stdout);
        }
    }
    for f in ["juicefs.log", "juicefs-hermit.log"] {
        let _ = std::fs::copy(e.work.join(f), dir.join(f));
    }
    save("postgres.log", cmd("docker", &["logs", "aviary-e2e-pg"]));
    save("s3.log", cmd("docker", &["logs", "aviary-e2e-s3"]));
}

/// The cluster, the containers, the mounts and the work directory (as much as there is).
pub(crate) fn down(e: &Env) {
    let _ = cmd("k3d", &["cluster", "delete", CLUSTER]).output_within(300);
    let _ = cmd("docker", &["rm", "-f", "-v", "aviary-e2e-pg", "aviary-e2e-s3"]).output_within(120);
    for m in [JFS, JFS_HERMIT] {
        if cmd("mountpoint", &["-q", m]).ok(10) && !cmd("sudo", &["juicefs", "umount", "--force", m]).ok(60) {
            let _ = cmd("sudo", &["umount", "-l", m]).output_within(60);
        }
    }
    if cmd("mountpoint", &["-q", MNT]).ok(10) {
        let _ = cmd("sudo", &["umount", "-l", MNT]).output_within(60);
    }
    let _ = cmd("docker", &["network", "rm", NET]).output_within(60);
    let _ = cmd("sudo", &["rm", "-rf", &e.work.to_string_lossy()]).output_within(120);
}

#[cfg(test)]
mod tests {
    use super::{calico_manifest, full_name};

    #[test]
    fn calicos_pool_is_the_harness_one() {
        let orig = "            # - name: CALICO_IPV4POOL_CIDR\n            #   value: \"192.168.0.0/16\"\n\
                                - name: CALICO_IPV4POOL_IPIP\n              value: \"Always\"\n\
                                - name: OTHER\n              value: \"Always\"\n";
        let m = calico_manifest(orig);
        assert_eq!(
            m,
            "            - name: CALICO_IPV4POOL_CIDR\n              value: \"10.48.0.0/16\"\n\
                         - name: CALICO_IPV4POOL_IPIP\n              value: \"Never\"\n\
                         - name: OTHER\n              value: \"Always\"\n"
        );
    }

    #[test]
    fn image_names_as_containerd_has_them() {
        assert_eq!(full_name("busybox:1.37.0"), "docker.io/library/busybox:1.37.0");
        assert_eq!(full_name("aviary-e2e/pigeon:e2e"), "docker.io/aviary-e2e/pigeon:e2e");
        assert_eq!(full_name("ghcr.io/de0ch/magpie:abc"), "ghcr.io/de0ch/magpie:abc");
        assert_eq!(full_name("rancher/k3s"), "docker.io/rancher/k3s:latest");
    }
}
