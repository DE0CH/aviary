//! The tx rule (platform 4.3), compile-fail style.
//!
//! - CI's second lint pass (`CLIPPY_CONF_DIR=target/tx-rule`, `-A clippy::disallowed_methods -D warnings`)
//!   rejects a network call inside the closure of `aviary_pg::tx!` (`reqwest`, every typed client of the contract
//!   crates, `aviary_core::http::send`, the AWS SDKs) and accepts the same calls outside it.
//! - The repository's `clippy.toml` (the first pass) rejects every other way into a transaction: `tokio-postgres`' and
//!   deadpool's `transaction()`/`build_transaction()` and calling `aviary_pg::__run_tx` directly, and accepts `tx!`.
//! - The second pass's configuration is generated (`aviary_source_rules::tx_rule`, written for CI by the
//!   `tx-rule-conf` binary): the repository's `clippy.toml` plus the network calls, found in the contract crates'
//!   `client.rs` files and the AWS SDK sources. These tests generate it the same way.
//!
//! Each case is a scratch crate depending on this workspace's crates and on its `reqwest`, `tokio` and AWS SDKs,
//! checked offline with the workspace's lockfile and lint levels.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn metadata() -> &'static serde_json::Value {
    static META: OnceLock<serde_json::Value> = OnceLock::new();
    META.get_or_init(|| aviary_source_rules::cargo_metadata(env!("CARGO"), &repo_root()).unwrap())
}

/// The tx rule's configuration, generated as CI's lint job does, in a directory of its own.
fn tx_conf_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = scratch_base().join("conf");
        fs::create_dir_all(&dir).unwrap();
        let conf = aviary_source_rules::tx_rule::tx_rule_conf(&repo_root(), metadata()).unwrap();
        fs::write(dir.join("clippy.toml"), conf).unwrap();
        dir
    })
    .clone()
}

fn sdk_names() -> Vec<&'static str> {
    aviary_source_rules::tx_rule::sdks(metadata())
        .iter()
        .map(|s| s.name)
        .collect()
}

fn scratch_base() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("tx-rule")
}

/// The `[workspace.dependencies]` line for `name`, verbatim.
fn workspace_dep(root: &Path, name: &str) -> String {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    manifest
        .lines()
        .find(|l| l.starts_with(&format!("{name} = ")))
        .unwrap_or_else(|| panic!("no workspace dependency {name}"))
        .to_owned()
}

/// The workspace crates a case may call.
const PATH_DEPS: &[&str] = &[
    "aviary-pg",
    "aviary-core",
    "hermit-api",
    "magpie-api",
    "sandboxd-api",
    "supervisor-api",
];
const REGISTRY_DEPS: &[&str] = &["reqwest", "tokio", "uuid", "aws-config"];

/// Runs clippy over a scratch crate whose `lib.rs` is `source`; `Some(stderr)` when it was rejected. `tx_conf`:
/// the second pass (the tx rule's configuration, the lint allowed outside `tx!`), else the first.
fn clippy(case: &str, source: &str, tx_conf: bool) -> Option<String> {
    let root = repo_root();
    let base = scratch_base();
    let dir = base.join(case);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    let mut deps = String::new();
    for d in PATH_DEPS {
        let _ = writeln!(
            deps,
            "{d} = {{ path = {:?} }}",
            root.join("crates").join(d).display().to_string()
        );
    }
    for d in REGISTRY_DEPS.iter().copied().chain(sdk_names()) {
        deps.push_str(&workspace_dep(&root, d));
        deps.push('\n');
    }
    // hyper's client half (the workspace's line names no features)
    let hyper = workspace_dep(&root, "hyper");
    let version = hyper.strip_prefix("hyper = ").unwrap();
    let _ = writeln!(
        deps,
        "hyper = {{ version = {version}, features = [\"client\", \"http1\"] }}"
    );
    let manifest = format!(
        "[package]\nname = \"scratch-{case}\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[dependencies]\n{deps}\n{}",
        aviary_source_rules::workspace_lints_as_package_lints(&root).unwrap(),
    );
    fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock")).unwrap();
    fs::copy(root.join("rust-toolchain.toml"), dir.join("rust-toolchain.toml")).unwrap();
    fs::write(dir.join("src/lib.rs"), source).unwrap();
    let conf = if tx_conf { tx_conf_dir() } else { root.clone() };
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["clippy", "--quiet", "--offline"])
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", base.join("target"))
        .env("CLIPPY_CONF_DIR", &conf)
        .env_remove("RUSTC_WRAPPER")
        .env("CARGO_TERM_COLOR", "never");
    if tx_conf {
        cmd.args(["--", "-A", "clippy::disallowed_methods", "-D", "warnings"]);
    } else {
        cmd.args(["--", "-D", "warnings"]);
    }
    let out = cmd.output().unwrap();
    if out.status.success() {
        None
    } else {
        Some(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// The disallowed methods clippy reported, sorted (clippy reports a chain's inner call first).
fn reported(stderr: &str) -> Vec<String> {
    let mut v: Vec<String> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("error: use of a disallowed method `"))
        .map(|l| l.trim_end_matches('`').to_owned())
        .collect();
    v.sort();
    v
}

fn sorted(v: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = v.iter().map(|s| (*s).to_owned()).collect();
    v.sort();
    v
}

/// Every kind of network call, as a service makes it. `$TX_OPEN` / `$TX_CLOSE` wrap the calls in a transaction or
/// not.
const CALLS: &str = r#"//! seeded: network calls

/// The clients a service holds.
#[allow(missing_debug_implementations)]
pub struct Clients {
    /// raw HTTP
    pub http: reqwest::Client,
    /// hermit
    pub hermit: hermit_api::Client,
    /// magpie
    pub magpie: magpie_api::client::Client,
    /// a host's sandboxd
    pub sandboxd: sandboxd_api::Client,
    /// a sandbox's supervisor
    pub supervisor: supervisor_api::client::ControlClient,
    /// the bucket
    pub s3: aws_sdk_s3::Client,
    $EC2_FIELD
}

/// Calls out.
///
/// # Errors
/// When the database fails.
pub async fn calls<Io>(
    pool: &aviary_pg::Pool,
    c: &Clients,
    h1: &mut hyper::client::conn::http1::SendRequest<String>,
    io: Io,
) -> Result<(), aviary_pg::Error>
where
    Io: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    $TX_OPEN
        let _ = c.http.post("http://hermit/v1/things").send().await;
        let _ = reqwest::get("http://hermit/v1/things").await;
        let _ = aviary_core::http::send(
            c.http.get("http://magpie/v1/x"),
            aviary_core::http::Backoff::default(),
            aviary_core::http::Idempotence::ByMethod,
        )
        .await;
        let _ = c.hermit.people().await;
        let _ = c.magpie.person("p").cancel_turn(uuid::Uuid::nil()).await;
        let _ = c.sandboxd.host().await;
        let _ = c.supervisor.health().await;
        let _ = c.s3.put_object().bucket("b").key("k").send().await;
        let _ = c.s3.list_objects_v2().bucket("b").into_paginator().send().next().await;
        $EC2_CALLS
        let _ = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let _ = hyper::client::conn::http1::handshake::<_, String>(io).await;
        let _ = h1.send_request(hyper::Request::new(String::new())).await;
        let _ = tokio::net::TcpStream::connect("hermit:80").await;
        Ok(())
    $TX_CLOSE
}
"#;

/// What the calls of [`CALLS`] are reported as.
const CALLED: &[&str] = &[
    "reqwest::Client::post",
    "reqwest::RequestBuilder::send",
    "reqwest::get",
    "reqwest::Client::get",
    "aviary_core::http::send",
    "hermit_api::client::Client::people",
    "magpie_api::client::PersonCalls::cancel_turn",
    "sandboxd_api::client::Client::host",
    "supervisor_api::client::ControlClient::health",
    "aws_sdk_s3::operation::put_object::builders::PutObjectFluentBuilder::send",
    "aws_smithy_async::future::pagination_stream::PaginationStream::next",
    "$EC2",
    "aws_config::load_defaults",
    "hyper::client::conn::http1::handshake",
    "hyper::client::conn::http1::SendRequest::send_request",
    "tokio::net::TcpStream::connect",
];

/// The EC2 calls, once a crate (hermit) depends on the SDK.
const EC2_CALLS: &str = r#"let _ = c.ec2.describe_instances().customize().send().await;
        let _ = c.ec2.terminate_instances().instance_ids("i-1").send().await;
        let _ = aws_sdk_ec2::operation::describe_instances::builders::DescribeInstancesInputBuilder::default()
            .send_with(&c.ec2)
            .await;
        let _ = aws_sdk_ec2::client::Waiters::wait_until_instance_running(&c.ec2).wait(std::time::Duration::from_secs(1)).await;"#;
const EC2_CALLED: &[&str] = &[
    "aws_sdk_ec2::client::customize::CustomizableOperation::send",
    "aws_sdk_ec2::operation::terminate_instances::builders::TerminateInstancesFluentBuilder::send",
    "aws_sdk_ec2::operation::describe_instances::builders::DescribeInstancesInputBuilder::send_with",
    "aws_sdk_ec2::waiters::instance_running::InstanceRunningFluentBuilder::wait",
];

fn ec2_present() -> bool {
    sdk_names().contains(&"aws-sdk-ec2")
}

fn called() -> Vec<&'static str> {
    let ec2 = ec2_present();
    CALLED
        .iter()
        .flat_map(|c| {
            if *c == "$EC2" {
                if ec2 { EC2_CALLED } else { &[] }
            } else {
                std::slice::from_ref(c)
            }
        })
        .copied()
        .collect()
}

fn calls(tx: bool) -> String {
    let (open, close) = if tx {
        (
            "aviary_pg::tx!(pool, |t| {\n        t.execute(\"select 1\", &[]).await?;",
            "})\n    .await",
        )
    } else {
        (
            "aviary_pg::tx!(pool, |t| t.execute(\"select 1\", &[]).await).await?;",
            "",
        )
    };
    let ec2 = ec2_present();
    CALLS
        .replace("$TX_OPEN", open)
        .replace("$TX_CLOSE", close)
        .replace("$EC2_CALLS", if ec2 { EC2_CALLS } else { "" })
        .replace(
            "$EC2_FIELD",
            if ec2 {
                "/// EC2\n    pub ec2: aws_sdk_ec2::Client,"
            } else {
                ""
            },
        )
}

const INSIDE_TYPED: &str = r#"//! seeded: a network call inside a transaction with the caller's error type
/// The caller's error.
#[derive(Debug)]
pub struct Mine;
impl From<aviary_pg::Error> for Mine {
    fn from(_: aviary_pg::Error) -> Self {
        Self
    }
}
/// Reads a body inside a transaction.
///
/// # Errors
/// When anything fails.
pub async fn record(pool: &aviary_pg::Pool, http: &reqwest::Client) -> Result<String, Mine> {
    let resp = http.get("http://magpie/v1/x").send().await.map_err(|_| Mine)?;
    aviary_pg::tx!(pool, |t| -> Result<String, Mine> {
        t.execute("select 1", &[]).await?;
        resp.text().await.map_err(|_| Mine)
    })
    .await
}
"#;

#[test]
fn network_calls_inside_tx_are_rejected_and_outside_are_accepted() {
    let stderr = clippy("inside", &calls(true), true).expect("clippy accepted network calls inside aviary_pg::tx!");
    assert_eq!(reported(&stderr), sorted(&called()), "{stderr}");
    assert!(stderr.contains("no network call inside aviary_pg::tx!"), "{stderr}");

    let stderr = clippy("typed", INSIDE_TYPED, true).expect("clippy accepted a reqwest body read inside the typed tx!");
    // the call before the transaction is not reported
    assert_eq!(reported(&stderr), ["reqwest::Response::text"], "{stderr}");

    assert_eq!(
        clippy("outside", &calls(false), true),
        None,
        "the tx rule rejected calls outside a transaction"
    );
}

#[test]
fn the_first_pass_allows_network_calls_and_only_tx_into_a_transaction() {
    // the main pass alone does not see the tx rule: the second pass is needed
    assert_eq!(clippy("main-outside", &calls(false), false), None);
    assert_eq!(clippy("main-inside", &calls(true), false), None);

    let other_ways = r#"//! seeded: transactions without tx!
use aviary_pg::reexports::{deadpool_postgres, tokio_postgres};

/// tokio-postgres' own transactions.
///
/// # Errors
/// Never mind.
pub async fn raw(c: &mut tokio_postgres::Client) -> Result<(), tokio_postgres::Error> {
    c.transaction().await?.commit().await?;
    c.build_transaction().start().await?.commit().await
}

/// deadpool's.
///
/// # Errors
/// Never mind.
pub async fn pooled(c: &mut deadpool_postgres::Object) -> Result<(), tokio_postgres::Error> {
    c.transaction().await?.commit().await?;
    c.build_transaction().start().await?.commit().await
}

/// The macro's runner called by hand.
///
/// # Errors
/// Never mind.
pub async fn by_hand(pool: &aviary_pg::Pool) -> Result<(), aviary_pg::Error> {
    aviary_pg::__run_tx(pool, aviary_pg::__tx_fn(|t| Box::pin(async move { t.execute("select 1", &[]).await.map(drop) }))).await
}
"#;
    let stderr = clippy("other-ways", other_ways, false).expect("clippy accepted a transaction without tx!");
    assert_eq!(
        reported(&stderr),
        sorted(&[
            "tokio_postgres::Client::transaction",
            "tokio_postgres::Client::build_transaction",
            "deadpool_postgres::ClientWrapper::transaction",
            "deadpool_postgres::ClientWrapper::build_transaction",
            "aviary_pg::__run_tx",
        ]),
        "{stderr}"
    );
    assert!(stderr.contains("aviary_pg::tx!"), "{stderr}");
}

/// The generator finds the calls it should.
#[test]
fn the_generated_configuration_lists_the_clients_and_the_sdks() {
    let conf = std::fs::read_to_string(tx_conf_dir().join("clippy.toml")).unwrap();
    for must in [
        "reqwest::RequestBuilder::send",
        "aviary_core::http::send",
        "hermit_api::client::Client::put_sandbox",
        "magpie_api::client::PersonCalls::send_turn",
        "magpie_api::client::SandboxCalls::heartbeat",
        "sandboxd_api::client::Client::create",
        "supervisor_api::client::ControlClient::release",
        "aws_sdk_s3::operation::get_object::builders::GetObjectFluentBuilder::send",
        "aws_sdk_s3::operation::put_object::builders::PutObjectInputBuilder::send_with",
        "aws_sdk_s3::client::customize::CustomizableOperation::send",
        "reqwest::Response::upgrade",
        "kube::Api::patch",
        "kube::Api::list",
        "kube::Client::request",
        "hyper::client::conn::http2::SendRequest::send_request",
        "hyper_util::client::legacy::Client::request",
    ] {
        assert!(conf.contains(&format!("\"{must}\"")), "{must}");
    }
    if ec2_present() {
        assert!(conf.contains("\"aws_sdk_ec2::operation::run_instances::builders::RunInstancesFluentBuilder::send\""));
    }
    // sync builders are not calls
    assert!(!conf.contains("hermit_api::client::Client::bearer\""));
    // everything of clippy.toml is in it, so the second pass judges the rest exactly as the first
    let main = std::fs::read_to_string(repo_root().join("clippy.toml")).unwrap();
    assert!(conf.contains(main.trim_end().strip_suffix(']').unwrap()));
}
