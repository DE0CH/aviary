//! pigeon: the app-facing front end (Paseo protocol) of aviary. `pigeon serve` runs it (spec 3); `pigeon
//! --version` prints the build.

use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::time::Duration;

use aviary_core::{drain, log, ops, version};
use pigeon::{App, Config, PORT};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("serve") => {}
        Some("--version") => {
            let _ = writeln!(std::io::stdout(), "{}", version::Build::new("pigeon").version_line());
            return ExitCode::SUCCESS;
        }
        _ => {
            let _ = writeln!(std::io::stderr(), "usage: pigeon serve | pigeon --version");
            return ExitCode::from(2);
        }
    }
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "pigeon: no runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(run())
}

async fn run() -> ExitCode {
    let build = version::Build::new("pigeon");
    log::init_in_pod(&build, log::Output::Stdout);
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut registry = ops::registry(&build);
    let config = Config::from_env(build.clone());
    let bound = config.tuning.transfer_bound;
    let app = match App::new(config, &mut registry) {
        Ok(app) => app,
        Err(e) => {
            tracing::error!(error = %e, "app bundle not loaded");
            return ExitCode::FAILURE;
        }
    };
    let ops = ops::Ops::new(build, registry);
    if let Err(e) = ops.spawn(ops::ADDR).await {
        tracing::error!(error = %e, "ops port not bound");
        return ExitCode::FAILURE;
    }
    app.start(ops.readiness());
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "port 8080 not bound");
            return ExitCode::FAILURE;
        }
    };
    let server = pigeon::server::serve(listener, app.router());
    ops.readiness().started();
    tracing::info!("serving");
    let mut d = drain::Drain::new(ops.readiness().clone());
    // spec 11.2 steps 2 and 4: the listener closes, in-flight requests and transfers finish (up to the bound)
    d.hook(
        "listener and requests",
        bound + Duration::from_secs(5),
        move || async move {
            server.drain(bound).await;
        },
    );
    let waiting = app.clone();
    d.hook("transfers", bound, move || async move {
        let started = std::time::Instant::now();
        waiting.transfers().idle().await;
        waiting.observe_drain(started.elapsed());
    });
    let mut signals = match drain::Signals::os() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "signal handlers not installed");
            return ExitCode::FAILURE;
        }
    };
    drain::exit(d.run(&mut signals).await)
}
