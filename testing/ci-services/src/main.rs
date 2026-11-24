//! The services CI's tests run against, each in a container on the runner (ci.yml's test and magpie-perf jobs).
//!
//! - `postgres IMAGE PORT`: the production Postgres image (`CloudNativePG`'s, which has no entrypoint of its own, so it
//!   cannot be a GitHub service container), TLS on with a throwaway self-signed certificate (aviary-pg connects with
//!   sslmode=require), scram passwords, fsync off. Its data is on the runner's disk: the suite makes a database per
//!   test. Prints the URL for `AVIARY_TEST_DATABASE_URL`.
//! - `postgres-sync IMAGE PORT`: the same image as a primary and one streaming standby named in
//!   `synchronous_standby_names` (magpie 6.10's write acceptance: a commit returns only after a synchronous standby has
//!   the WAL, as magpie-pg's CNPG cluster does), fsync on, at magpie-pg's WAL sizes (`min_wal_size` 1GB, `max_wal_size`
//!   2GB: a required parameter of the production cluster, crates/magpie/README.md). Both data directories are in
//!   memory: on GitHub's slower runners the disk stalled single writes for 150-900 ms (magpie-perf, 2026-11), which
//!   measured the runner, not magpie. Prints the primary's URL.
//! - `s3 IMAGE PORT`: `versitygw` (an S3 gateway over a directory, here in memory). The `MinIO` platform 6.1 names
//!   no longer publishes anything to pull: docker.io/minio/minio is gone, quay.io/minio/minio refuses anonymous pulls
//!   and dl.min.io answers 410 (checked 2026-11-02). Credentials are generated per run. Prints the
//!   `AVIARY_TEST_S3_*` lines for `$GITHUB_ENV`.
//!
//! Every container idles on `sleep infinity` and its server is started inside it with `docker exec`, so no shell
//! script runs anywhere.

use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Start a service CI's tests run against and print how to reach it")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// One Postgres server, its data on disk.
    Postgres { image: String, port: u16 },
    /// A Postgres primary and a synchronous standby, their data in memory.
    PostgresSync { image: String, port: u16 },
    /// versitygw, its data in memory.
    S3 { image: String, port: u16 },
}

/// Inside every Postgres container.
const PGDATA: &str = "/tmp/pgdata";
const PRIMARY: &str = "aviary-ci-pg-primary";
const STANDBY: &str = "aviary-ci-pg-standby";
const NETWORK: &str = "aviary-ci-pgsync";
/// A data directory in memory: the container's /tmp as tmpfs, writable by the image's postgres user.
const TMPFS: &str = "/tmp:rw,mode=1777";

fn main() -> Result<()> {
    let out = match Cli::parse().cmd {
        Cmd::Postgres { image, port } => postgres(&image, port)?,
        Cmd::PostgresSync { image, port } => postgres_sync(&image, port)?,
        Cmd::S3 { image, port } => s3(&image, port)?,
    };
    std::io::stdout().lock().write_all(out.as_bytes())?;
    Ok(())
}

fn postgres(image: &str, port: u16) -> Result<String> {
    let name = "aviary-ci-postgres";
    let (dir, password) = secrets(name, "hostssl all all all scram-sha-256\n")?;
    idle(name, image, &dir, &[&format!("127.0.0.1:{port}:5432")], None, false)?;
    let bin = pg_bin(name)?;
    initdb(name, &bin)?;
    pg_start(
        name,
        &bin,
        "-c listen_addresses='*' -c ssl=on -c fsync=off -c max_connections=200",
    )?;
    Ok(format!(
        "postgres://postgres:{password}@127.0.0.1:{port}/postgres?sslmode=require\n"
    ))
}

fn postgres_sync(image: &str, port: u16) -> Result<String> {
    let wal = "-c min_wal_size=1GB -c max_wal_size=2GB";
    let (dir, password) = secrets(
        PRIMARY,
        "hostssl all all all scram-sha-256\nhost replication postgres all scram-sha-256\n",
    )?;
    docker(&["network", "create", NETWORK])?;
    idle(
        PRIMARY,
        image,
        &dir,
        &[&format!("127.0.0.1:{port}:5432")],
        Some(NETWORK),
        true,
    )?;
    let bin = pg_bin(PRIMARY)?;
    initdb(PRIMARY, &bin)?;
    pg_start(
        PRIMARY,
        &bin,
        &format!(
            "-c listen_addresses='*' -c ssl=on -c max_connections=200 -c wal_level=replica -c max_wal_senders=4 \
             -c synchronous_standby_names=standby1 -c synchronous_commit=on {wal}"
        ),
    )?;
    // the standby allows at least what the primary does (max_connections, max_wal_senders), or recovery stops
    idle(STANDBY, image, &dir, &[], Some(NETWORK), true)?;
    docker(&[
        "exec",
        "-e",
        &format!("PGPASSWORD={password}"),
        STANDBY,
        &format!("{bin}/pg_basebackup"),
        "-h",
        PRIMARY,
        "-U",
        "postgres",
        "-D",
        PGDATA,
        "-X",
        "stream",
        "-R",
        "-d",
        "application_name=standby1 sslmode=disable",
    ])?;
    docker(&["exec", STANDBY, "chmod", "0700", PGDATA])?;
    pg_start(
        STANDBY,
        &bin,
        &format!("-c hot_standby=on -c ssl=on -c max_connections=200 -c max_wal_senders=4 {wal}"),
    )?;
    for _ in 0..60 {
        let state = docker(&[
            "exec",
            PRIMARY,
            "psql",
            "-U",
            "postgres",
            "-tAc",
            "select sync_state from pg_stat_replication where application_name = 'standby1'",
        ])
        .unwrap_or_default();
        if state == "sync" {
            return Ok(format!(
                "postgres://postgres:{password}@127.0.0.1:{port}/postgres?sslmode=require\n"
            ));
        }
        sleep(Duration::from_secs(1));
    }
    logs(PRIMARY);
    logs(STANDBY);
    bail!("the standby never became synchronous")
}

fn s3(image: &str, port: u16) -> Result<String> {
    let name = "aviary-ci-s3";
    let (access, secret) = (hex(10)?, hex(20)?);
    docker(&[
        "run",
        "-d",
        "--name",
        name,
        "-p",
        &format!("127.0.0.1:{port}:7070"),
        "--tmpfs",
        "/data:rw,mode=0777",
        image,
        "--access",
        &access,
        "--secret",
        &secret,
        "--region",
        "us-east-1",
        "--port",
        ":7070",
        "posix",
        "/data",
    ])?;
    for _ in 0..60 {
        if answers_http(port) {
            return Ok(format!(
                "AVIARY_TEST_S3_ENDPOINT=http://127.0.0.1:{port}\nAVIARY_TEST_S3_REGION=us-east-1\n\
                 AVIARY_TEST_S3_ACCESS_KEY_ID={access}\nAVIARY_TEST_S3_SECRET_ACCESS_KEY={secret}\n"
            ));
        }
        sleep(Duration::from_secs(1));
    }
    if let Ok(l) = docker(&["logs", name]) {
        eprintln!("{l}");
    }
    bail!("versitygw did not answer on port {port}")
}

/// A directory the containers mount at /ci: a self-signed certificate, the password file and `pg_hba.conf`. Returns it
/// with the password.
fn secrets(name: &str, hba: &str) -> Result<(PathBuf, String)> {
    let dir = std::env::temp_dir().join(format!("{name}-{}", hex(6)?));
    fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    run(Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2"])
        .args([
            "-subj",
            "/CN=aviary-ci-postgres",
            "-keyout",
            &dir.join("server.key").to_string_lossy(),
            "-out",
            &dir.join("server.crt").to_string_lossy(),
        ]))?;
    let password = hex(16)?;
    fs::write(dir.join("pw"), format!("{password}\n"))?;
    fs::write(dir.join("pg_hba.conf"), format!("local all all trust\n{hba}"))?;
    for f in ["server.key", "server.crt", "pw", "pg_hba.conf"] {
        fs::set_permissions(dir.join(f), fs::Permissions::from_mode(0o644))?;
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
    Ok((dir, password))
}

/// A container of `image` idling, /ci mounted from `dir`, its /tmp in memory when `memory`.
fn idle(name: &str, image: &str, dir: &Path, publish: &[&str], net: Option<&str>, memory: bool) -> Result<()> {
    let ci = format!("{}:/ci:ro", dir.display());
    let mut args = vec!["run", "-d", "--name", name, "-v", &ci];
    for p in publish {
        args.extend(["-p", *p]);
    }
    if let Some(n) = net {
        args.extend(["--network", n]);
    }
    if memory {
        args.extend(["--tmpfs", TMPFS]);
    }
    args.extend(["--entrypoint", "sleep", image, "infinity"]);
    docker(&args).map(drop)
}

/// The image's one Postgres version's bin directory.
fn pg_bin(name: &str) -> Result<String> {
    let versions = docker(&["exec", name, "ls", "/usr/lib/postgresql"])?;
    let v = versions.lines().next().context("no Postgres in the image")?;
    Ok(format!("/usr/lib/postgresql/{v}/bin"))
}

fn initdb(name: &str, bin: &str) -> Result<()> {
    docker(&[
        "exec",
        name,
        &format!("{bin}/initdb"),
        "-D",
        PGDATA,
        "-U",
        "postgres",
        "--pwfile=/ci/pw",
        "-A",
        "scram-sha-256",
    ])?;
    for f in ["server.crt", "server.key", "pg_hba.conf"] {
        docker(&["exec", name, "cp", &format!("/ci/{f}"), &format!("{PGDATA}/{f}")])?;
    }
    docker(&["exec", name, "chmod", "0600", &format!("{PGDATA}/server.key")]).map(drop)
}

/// Starts the server and waits until it accepts connections (`pg_ctl -w`); its log is printed when it does not.
fn pg_start(name: &str, bin: &str, options: &str) -> Result<()> {
    let log = "/tmp/postgres.log";
    let started = docker(&[
        "exec",
        name,
        &format!("{bin}/pg_ctl"),
        "-D",
        PGDATA,
        "-l",
        log,
        "-w",
        "-t",
        "60",
        "-o",
        options,
        "start",
    ]);
    if started.is_err()
        && let Ok(l) = docker(&["exec", name, "cat", log])
    {
        eprintln!("{name}'s log:\n{l}");
    }
    started.map(drop)
}

fn logs(name: &str) {
    if let Ok(l) = docker(&["exec", name, "cat", "/tmp/postgres.log"]) {
        eprintln!("{name}'s log:\n{l}");
    }
}

fn docker(args: &[&str]) -> Result<String> {
    run(Command::new("docker").args(args))
}

/// Runs `cmd`; its stdout, trimmed. A failure carries the command and its stderr.
fn run(cmd: &mut Command) -> Result<String> {
    let o = cmd.output().with_context(|| format!("running {cmd:?}"))?;
    if !o.status.success() {
        bail!("{cmd:?}: {}: {}", o.status, String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

/// `n` random bytes as hex.
fn hex(n: usize) -> Result<String> {
    let mut b = vec![0u8; n];
    fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().fold(String::with_capacity(2 * n), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    }))
}

/// Whether anything answers HTTP on the local port.
fn answers_http(port: u16) -> bool {
    let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut b = [0u8; 5];
    s.write_all(b"GET / HTTP/1.0\r\n\r\n").is_ok() && s.read_exact(&mut b).is_ok() && &b == b"HTTP/"
}
