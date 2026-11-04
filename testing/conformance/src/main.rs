//! `aviary-conformance`: the hermit, gateway and model conformance suites (platform 8). Exit code 0 = pass; one line
//! per check on stdout; `JUnit` XML to `--junit <file>`.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use aviary_conformance::report::Report;
use aviary_conformance::{fakes, gateway, hermit, model, proxy};
use clap::{Parser, Subcommand};
use hermit_api::token::MasterKey;

#[derive(Parser, Debug)]
#[command(about = "Conformance suites for implementations of the aviary contracts")]
struct Args {
    /// Write the results as `JUnit` XML here.
    #[arg(long, global = true)]
    junit: Option<PathBuf>,
    /// Run only the checks whose names start with this (repeatable).
    #[arg(long, global = true)]
    only: Vec<String>,
    #[command(subcommand)]
    suite: Suite,
}

#[derive(Subcommand, Debug)]
enum Suite {
    /// hermit spec 24.1 against a live hermit with a fake fabric.
    Hermit {
        #[arg(long)]
        target: String,
        /// JSON: {"pigeon", "magpie", "gateway", "tools", "monitor", "admin"}, the pair token each caller presents.
        #[arg(long)]
        tokens: PathBuf,
        /// What the implementation claims besides the core: `pause`, `snapshot`.
        #[arg(long, default_value = "")]
        capabilities: String,
        /// A command printing the sandbox token of a sandbox; `{sandboxId}` and `{conversationId}` are replaced.
        #[arg(long)]
        token_command: Option<String>,
        /// How long a sandbox may take to become ready, in seconds.
        #[arg(long, default_value_t = 120)]
        ready_within_secs: u64,
    },
    /// Platform 8.2 against a sandbox gateway.
    Gateway {
        /// The proxy, `http://host:3128`.
        #[arg(long)]
        proxy: String,
        /// The in-cluster API, `http://host:8082`.
        #[arg(long)]
        api: String,
        /// The address the gateway sees this suite's connections come from.
        #[arg(long)]
        sandbox_ip: Ipv4Addr,
        /// Where the fakes listen (`:80`); the gateway's upstreams must reach them.
        #[arg(long, default_value = ":80")]
        serve_fakes: String,
        /// Serve hermit alone here too (`:8081`) and point the gateway's hermit upstream at it: the outage's
        /// refuse phase then closes this listener, so the gateway's connects are refused (ECONNREFUSED). Without
        /// it, hermit answers on --serve-fakes by `Host` and that phase resets each connection after accept.
        #[arg(long)]
        serve_hermit_fakes: Option<String>,
        /// JSON: {"gatewayToMagpie", "gatewayToHermit", "gatewayToTools", "gatewayToModel", "pigeonToGateway",
        /// "hermitToGateway"} and, with --route, "gatewayToRoute".
        #[arg(long)]
        tokens: PathBuf,
        /// hermit's signing key, `kid:base64url(32 bytes)` (or `@file`).
        #[arg(long)]
        signing_key: String,
        /// A shell command that sends SIGTERM to the replica under test (the drain check runs last).
        #[arg(long)]
        sigterm: Option<String>,
        /// One of the gateway's own addresses (its VIP, a Service name); repeatable.
        #[arg(long = "self-host")]
        self_hosts: Vec<String>,
        /// A configured route's name (platform 3.11.8): the gateway routes it to --serve-fakes with the prefix
        /// `/api/v1/` and `Authorization: Bearer <gatewayToRoute>` (the route.configured checks; skipped without).
        #[arg(long)]
        route: Option<String>,
        /// Short durations, for the suite's own tests only (a pass then proves nothing).
        #[arg(long, hide = true)]
        quick: bool,
    },
    /// Platform 8.3 against a model route.
    Model {
        #[arg(long)]
        target: String,
        /// pigeon's pair token.
        #[arg(long)]
        token: String,
        /// Send the Messages API calls through this proxy (with --sandbox-token).
        #[arg(long, requires = "sandbox_token")]
        proxy: Option<String>,
        #[arg(long)]
        sandbox_token: Option<String>,
    },
}

fn read_json<T: serde::de::DeserializeOwned>(p: &PathBuf) -> anyhow::Result<T> {
    let text = std::fs::read_to_string(p).with_context(|| p.display().to_string())?;
    serde_json::from_str(&text).with_context(|| p.display().to_string())
}

async fn shell(cmd: &str) -> Result<String, String> {
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[tokio::main]
#[allow(clippy::too_many_lines)] // one arm per suite, each mapping its flags
async fn main() -> anyhow::Result<ExitCode> {
    let args = Args::parse();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (name, report) = match args.suite {
        Suite::Hermit {
            target,
            tokens,
            capabilities,
            token_command,
            ready_within_secs,
        } => {
            let report = Report::new("hermit", args.only, true);
            let mut hooks = hermit::Hooks::default();
            if let Some(cmd) = token_command {
                hooks.token = Some(Box::new(move |sandbox, conversation| {
                    let c = cmd
                        .replace("{sandboxId}", &sandbox.to_string())
                        .replace("{conversationId}", &conversation.to_string());
                    let out = std::process::Command::new("sh").arg("-c").arg(&c).output().ok()?;
                    out.status
                        .success()
                        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
                        .filter(|t| !t.is_empty())
                }));
            }
            let o = hermit::Options {
                target,
                tokens: read_json(&tokens)?,
                capabilities: hermit::parse_capabilities(&capabilities).map_err(anyhow::Error::msg)?,
                hooks,
                ready_within: Duration::from_secs(ready_within_secs),
            };
            hermit::run(o, &report).await;
            ("hermit", report)
        }
        Suite::Gateway {
            proxy: proxy_url,
            api,
            sandbox_ip,
            serve_fakes,
            serve_hermit_fakes,
            tokens,
            signing_key,
            sigterm,
            self_hosts,
            route,
            quick,
        } => {
            let report = Report::new("gateway", args.only, true);
            let key_text = match signing_key.strip_prefix('@') {
                Some(path) => std::fs::read_to_string(path).with_context(|| path.to_owned())?,
                None => signing_key,
            };
            let signing_key = MasterKey::parse(key_text.trim()).map_err(|e| anyhow::anyhow!("--signing-key: {e}"))?;
            let tokens: gateway::Tokens = read_json(&tokens)?;
            let fakes = fakes::Fakes::new(
                tokens.expected(route.as_deref()),
                vec![gateway::public_key(&signing_key).map_err(anyhow::Error::msg)?],
            );
            let listener = tokio::net::TcpListener::bind(fakes::listen_addr(&serve_fakes).map_err(anyhow::Error::msg)?)
                .await
                .context("--serve-fakes")?;
            let _served = fakes.serve(listener)?;
            if let Some(a) = &serve_hermit_fakes {
                fakes
                    .serve_hermit(fakes::listen_addr(a).map_err(anyhow::Error::msg)?)
                    .context("--serve-hermit-fakes")?;
            }
            let proxy = proxy::resolve(&proxy_url).await.map_err(anyhow::Error::msg)?;
            let o = gateway::Options {
                proxy,
                api,
                sandbox_ip,
                tokens,
                signing_key,
                self_hosts,
                route,
                sigterm: sigterm.map(|cmd| -> gateway::Sigterm {
                    Box::new(move || {
                        let cmd = cmd.clone();
                        Box::pin(async move { shell(&cmd).await.map(|_| ()) })
                    })
                }),
                durations: if quick {
                    gateway::Durations::quick()
                } else {
                    gateway::Durations::spec()
                },
            };
            gateway::run(o, fakes, &report).await;
            ("gateway", report)
        }
        Suite::Model {
            target,
            token,
            proxy: proxy_url,
            sandbox_token,
        } => {
            let report = Report::new("model", args.only, true);
            let proxy = match (proxy_url, sandbox_token) {
                (Some(p), Some(t)) => Some((proxy::resolve(&p).await.map_err(anyhow::Error::msg)?, t)),
                _ => None,
            };
            model::run(model::Options { target, token, proxy }, &report).await;
            ("model", report)
        }
    };
    if let Some(p) = &args.junit {
        std::fs::write(p, report.junit()).with_context(|| p.display().to_string())?;
    }
    let failures = report.failures();
    eprintln!(
        "aviary-conformance {name}: {} checks, {} failed",
        report.results().len(),
        failures.len()
    );
    Ok(if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
