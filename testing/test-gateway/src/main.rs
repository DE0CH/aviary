//! `test-gateway`: the reference sandbox gateway (platform 7.3). See the library docs.
//!
//! Upstreams and listeners are flags (the harness's Service names are the defaults); the credentials are the
//! environment: `GATEWAY_TOKEN_MAGPIE`, `GATEWAY_TOKEN_HERMIT`, `GATEWAY_TOKEN_TOOLS` (the pair tokens it presents),
//! `GATEWAY_MODEL_KEY` (sent to the model upstream as `x-api-key`), `GATEWAY_ACCEPT_PIGEON` and
//! `GATEWAY_ACCEPT_HERMIT` (the in-cluster API's callers).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use aviary_core::auth::{Caller, Callers, outbound_token};
use aviary_core::{drain, env, log, ops, version};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "The reference sandbox gateway (aviary platform 7.3)", version = None)]
struct Args {
    /// The proxy.
    #[arg(long, default_value = "0.0.0.0:3128")]
    proxy_listen: SocketAddr,
    /// The in-cluster API (models route, ops summary).
    #[arg(long, default_value = "0.0.0.0:8082")]
    api_listen: SocketAddr,
    /// healthz, readyz, version, metrics.
    #[arg(long, default_value = "0.0.0.0:9090")]
    ops_listen: SocketAddr,
    #[arg(long, default_value = "http://magpie")]
    magpie: String,
    #[arg(long, default_value = "http://hermit")]
    hermit: String,
    #[arg(long, default_value = "http://tools")]
    tools: String,
    /// The `model` route's upstream.
    #[arg(long, default_value = "http://mock-model")]
    model: String,
    /// A host that is the proxy itself (its VIP, a Service name); repeatable.
    #[arg(long = "self-host")]
    self_hosts: Vec<String>,
    /// `GET /v1/ops/summary`'s answer (gateway-api's `OpsSummary` JSON); default: gateway-api's fixture.
    #[arg(long)]
    ops_summary: Option<PathBuf>,
    /// Print the version line and exit.
    #[arg(long)]
    version: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let build = version::Build::new("test-gateway").surface("gateway", gateway_api::GATEWAY.1);
    if args.version {
        use std::io::Write as _;
        writeln!(std::io::stdout(), "{}", build.version_line())?;
        return Ok(());
    }
    log::init(&build, None, log::Output::Stdout);
    let ops_summary = match &args.ops_summary {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(p).with_context(|| p.display().to_string())?)
            .context("ops summary")?,
        None => test_gateway::default_ops_summary(),
    };
    let credentials = test_gateway::Credentials {
        magpie: outbound_token("GATEWAY", "MAGPIE"),
        hermit: outbound_token("GATEWAY", "HERMIT"),
        tools: outbound_token("GATEWAY", "TOOLS"),
        model: env::required("GATEWAY_MODEL_KEY"),
    };
    let callers = Callers::from_env("GATEWAY", &[Caller::Pigeon, Caller::Hermit]);
    let mut registry = ops::registry(&build);
    let cfg = test_gateway::Config {
        proxy_listen: args.proxy_listen,
        api_listen: args.api_listen,
        upstreams: test_gateway::Upstreams {
            magpie: args.magpie,
            hermit: args.hermit,
            tools: args.tools,
            model: args.model,
        },
        credentials,
        callers,
        self_hosts: args.self_hosts,
        ops_summary,
        drain: test_gateway::DrainBounds::default(),
        upload_cap: test_gateway::UPLOAD_CAP,
        faults: test_gateway::Faults::default(),
    };
    let running = std::sync::Arc::new(test_gateway::start(cfg, &mut registry).await?);
    let ops = ops::Ops::new(build, registry);
    ops.spawn(args.ops_listen).await?;
    // not ready (out of the VIP's targets) until hermit's key set is in: no token could verify before
    let keys = ops.readiness().gate("token-keys");
    let r = running.clone();
    tokio::spawn(async move {
        r.keys_loaded().await;
        keys.open();
    });
    ops.readiness().started();
    tracing::info!(proxy = %running.proxy_addr(), api = %running.api_addr(), "test-gateway listening");
    let mut d = drain::Drain::new(ops.readiness().clone());
    let r = running.clone();
    d.hook(
        "gateway",
        test_gateway::DrainBounds::default().model + Duration::from_secs(10),
        move || async move {
            let report = r.drain().await;
            tracing::info!(cut = ?report.cut, "gateway drained");
        },
    );
    let outcome = d.run(&mut drain::Signals::os()?).await;
    drain::exit(outcome)
}
