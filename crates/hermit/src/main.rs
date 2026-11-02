//! `hermit`: `serve` (the replica) and `catalogue-check <file>` (the catalogue rules alone, for YYC's CI).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use aviary_core::auth::{Caller, Callers};
use aviary_core::{drain, env, log, ops, version};
use clap::{Parser, Subcommand};
use hermit::app::{App, StartOptions};
use hermit::catalogue::{self, Catalogue};
use hermit::config::{CONFIG_PATH, Config};
use hermit::fabric::{Providers, Unbuilt};
use hermit::tokens::Keys;
use hermit::workspaces::{JuiceCli, JuiceFs, MOUNT};

#[derive(Parser, Debug)]
#[command(about = "hermit: sandbox lifecycle")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Serve the API on :8080 and the ops endpoints on :9090.
    Serve,
    /// Check a catalogue file against the rules hermit applies (hermit 3.4.1).
    CatalogueCheck { file: PathBuf },
}

fn main() -> ExitCode {
    let args = Args::parse();
    match args.command {
        Command::CatalogueCheck { file } => {
            let mut err = std::io::stderr();
            match catalogue::check_file(&file) {
                Ok(c) => {
                    let _ = std::io::Write::write_fmt(
                        &mut err,
                        format_args!(
                            "ok: {} users, {} domains, {} admins\n",
                            c.users.len(),
                            c.domains.len(),
                            c.admins.len()
                        ),
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    let _ = std::io::Write::write_fmt(&mut err, format_args!("refused: {e}\n"));
                    ExitCode::FAILURE
                }
            }
        }
        Command::Serve => {
            let Ok(rt) = tokio::runtime::Runtime::new() else {
                return ExitCode::FAILURE;
            };
            rt.block_on(serve())
        }
    }
}

const CALLERS: [Caller; 6] = [
    Caller::Pigeon,
    Caller::Magpie,
    Caller::Gateway,
    Caller::Tools,
    Caller::Monitor,
    Caller::Admin,
];

#[allow(clippy::too_many_lines)] // the start sequence in order: env, config, start, listener, drain
async fn serve() -> ExitCode {
    let build = version::Build::new("hermit")
        .surface(hermit_api::CORE.0, hermit_api::CORE.1)
        .surface(hermit_api::SANDBOX.0, hermit_api::SANDBOX.1);
    let pod = env::field("HERMIT_POD");
    log::init(&build, Some(pod.clone()), log::Output::Stdout);
    let mut registry = ops::registry(&build);
    let config_path = std::path::Path::new(CONFIG_PATH);
    // hermit.json is read before the registry is handed over, so the catalogue's counter can be registered
    let config = Config::load(config_path);
    let staff = config
        .as_ref()
        .ok()
        .map(|c| Arc::new(Catalogue::new(c.file.catalogue.clone())));
    if let Some(s) = &staff {
        registry.register(
            "catalogue_refused",
            "Catalogue re-reads refused (the last good catalogue kept)",
            s.refusals_counter(),
        );
    }
    registry.register(
        "held_requests",
        "Long-polls this replica holds now",
        hermit::feed::held_gauge(),
    );
    let ops = ops::Ops::new(build, registry);
    if let Err(e) = ops.spawn(ops::ADDR).await {
        tracing::error!(error = %e, "ops listener");
        return ExitCode::FAILURE;
    }
    let readiness = ops.readiness().clone();
    let config_gate = readiness.gate("config");
    let migrations_gate = readiness.gate("migrations");
    let catalogue_gate = readiness.gate("catalogue");

    // secrets and pod facts only (8.4); everything else is hermit.json
    let database_url = env::required("HERMIT_DATABASE_URL");
    let keys = match Keys::parse(&env::required("HERMIT_TOKEN_KEYS")) {
        Ok(k) => Arc::new(k),
        Err(e) => {
            tracing::error!(name = "HERMIT_TOKEN_KEYS", error = %e, "invalid env");
            std::process::exit(env::EXIT_CONFIG);
        }
    };
    let callers = Callers::from_env("HERMIT", &CALLERS);
    let magpie_token = aviary_core::auth::outbound_token("HERMIT", "MAGPIE");
    let egress_token = aviary_core::auth::outbound_token("HERMIT", "EGRESS");
    let namespace = env::field("HERMIT_NAMESPACE");
    let juicefs = JuiceCli::new(
        env::required("HERMIT_JUICEFS_META_URL"),
        env::required("HERMIT_JUICEFS_META_PASSWORD"),
    );

    let config = match config {
        Ok(c) => Arc::new(c),
        Err(e) => {
            // never ready: the old pods keep serving (hermit 9)
            tracing::error!(error = %e, "hermit.json refused");
            std::future::pending::<()>().await;
            return ExitCode::FAILURE;
        }
    };
    config_gate.open();
    let Some(staff) = staff else {
        return ExitCode::FAILURE;
    };
    let ws = &config.file.sandbox.workspace;
    let workspaces = Arc::new(JuiceFs::new(MOUNT, Arc::new(juicefs), ws.quota_gib, ws.quota_inodes));

    let app = loop {
        let options = StartOptions {
            config: config.clone(),
            catalogue: staff.clone(),
            reload_catalogue: true,
            keys: keys.clone(),
            database_url: database_url.clone(),
            pod: pod.clone(),
            pool_size: 10,
            // the gVisor fabric (WP-19) replaces `Unbuilt`
            providers: Providers {
                fabric: Arc::new(Unbuilt),
                workspaces: workspaces.clone(),
            },
            timings: hermit::Timings::default(),
            callers: callers.clone(),
            upstreams: hermit::ops::Upstreams::new(
                "http://magpie",
                magpie_token.clone(),
                &format!("http://{}", config.file.fabric.egress_service),
                egress_token.clone(),
                Arc::new(hermit::ops::InCluster::new(namespace.clone())),
            ),
        };
        match App::start(options).await {
            Ok(app) => break Arc::new(app),
            Err(hermit::app::StartError::MissingKid(k)) => {
                tracing::error!(kids = ?k, "a live sandbox's token key is missing from HERMIT_TOKEN_KEYS");
                std::future::pending::<()>().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "start failed (retrying)");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
    migrations_gate.open();
    {
        let app = app.clone();
        tokio::spawn(async move {
            while !app.ready() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            catalogue_gate.open();
        });
    }

    let listener = match tokio::net::TcpListener::bind("0.0.0.0:8080").await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "api listener");
            return ExitCode::FAILURE;
        }
    };
    let router = app
        .router()
        .layer(axum::middleware::from_fn(aviary_core::http::defaults))
        .layer(axum::middleware::from_fn(aviary_core::http::request_id));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        let mut rx = stop_rx;
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.wait_for(|s| *s).await;
            })
            .await;
    });
    readiness.started();

    let mut d = drain::Drain::new(readiness);
    hermit::app::drain_hooks(&mut d, app, move || async move {
        let _ = stop_tx.send(true);
        let _ = server.await;
    });
    let Ok(mut signals) = drain::Signals::os() else {
        return ExitCode::FAILURE;
    };
    drain::exit(d.run(&mut signals).await)
}
