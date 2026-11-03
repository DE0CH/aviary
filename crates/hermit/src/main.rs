//! `hermit`: `serve` (the replica), `catalogue-check <file>` (the catalogue rules alone, for YYC's CI),
//! `--version` (the build and its protocol numbers, which the image's labels are made from, platform 5.3), and the
//! switch's one-shot commands `import-jarvis [--delta | --final]`, `jfs-owner`, `legacy-cleanup [--confirm]`
//! (`hermit::legacy`).

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
use hermit::fabric::gvisor::{Deps, Gvisor, HostSecrets, Port7070};
use hermit::fabric::network::{EgressSource, Fixed, Watched};
use hermit::fabric::{Providers, ec2, sandboxd};
use hermit::tokens::Keys;
use hermit::workspaces::{JuiceCli, JuiceFs, MOUNT};

#[derive(Parser, Debug)]
#[command(about = "hermit: sandbox lifecycle", disable_version_flag = true)]
struct Args {
    /// Print `hermit <sha12> protocols=<json>` and exit.
    #[arg(long)]
    version: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

/// hermit's build facts: the surfaces it serves (hermit-api's numbers).
fn build() -> version::Build {
    version::Build::new("hermit")
        .surface(hermit_api::CORE.0, hermit_api::CORE.1)
        .surface(hermit_api::SANDBOX.0, hermit_api::SANDBOX.1)
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Serve the API on :8080 and the ops endpoints on :9090.
    Serve,
    /// Check a catalogue file against the rules hermit applies (hermit 3.4.1).
    CatalogueCheck { file: PathBuf },
    /// The switch's copy of the old stack's sandbox history and share workspaces (hermit 22.3 step 3, cutover 8.1;
    /// `CronJob` `hermit-import`). Env: `HERMIT_DATABASE_URL`, `HERMIT_IMPORT_JARVIS_URL` and
    /// `HERMIT_IMPORT_CLOUDCLI_URL` (read-only), the `JuiceFS` pair.
    ImportJarvis {
        /// Only what can be new since the last run.
        #[arg(long, conflicts_with = "final_pass")]
        delta: bool,
        /// A6's last run, after the old stack is gone: a full pass that also imports what the old fabric left
        /// unfinished.
        #[arg(long = "final")]
        final_pass: bool,
    },
    /// Everything under `/jfs/ws` to uid/gid 1000 (hermit 15.4; Job `hermit-jfs-owner`, as root, unsquashed mount).
    JfsOwner {
        #[arg(long, default_value = MOUNT)]
        root: PathBuf,
    },
    /// Removes `users/`, `sent/`, the empty `ws/` folders and the old shares' clones magpie no longer references
    /// (hermit 22.3 step 6), once every old share folder is carried over. Without `--confirm` it prints the plan and
    /// removes nothing. Env: `HERMIT_DATABASE_URL`, `HERMIT_TOKEN_MAGPIE`, the
    /// `JuiceFS` pair.
    LegacyCleanup {
        #[arg(long)]
        confirm: bool,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    if args.version {
        let _ = std::io::Write::write_all(
            &mut std::io::stdout(),
            format!("{}\n", build().version_line()).as_bytes(),
        );
        return ExitCode::SUCCESS;
    }
    let Some(command) = args.command else {
        let _ = std::io::Write::write_all(
            &mut std::io::stderr(),
            b"usage: hermit serve | catalogue-check <file> | import-jarvis [--delta | --final] | jfs-owner | legacy-cleanup [--confirm] | --version\n",
        );
        return ExitCode::from(2);
    };
    match command {
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
        Command::ImportJarvis { delta, final_pass } => one_shot("import-jarvis", import_jarvis(delta, final_pass)),
        Command::JfsOwner { root } => one_shot("jfs-owner", jfs_owner(root)),
        Command::LegacyCleanup { confirm } => one_shot("legacy-cleanup", legacy_cleanup(confirm)),
    }
}

/// Runs a one-shot command: logs to stdout like `serve`, exits with what it returns.
fn one_shot(name: &str, f: impl std::future::Future<Output = ExitCode>) -> ExitCode {
    log::init(&build(), Some(name.to_owned()), log::Output::Stdout);
    let Ok(rt) = tokio::runtime::Runtime::new() else {
        return ExitCode::FAILURE;
    };
    rt.block_on(f)
}

/// The workspace store of a one-shot command: hermit.json's quota, the `JuiceFS` pair from the env.
fn store() -> Result<JuiceFs, ExitCode> {
    let config = Config::load(std::path::Path::new(CONFIG_PATH)).map_err(|e| {
        tracing::error!(error = %e, "hermit.json refused");
        ExitCode::from(2)
    })?;
    let cli = Arc::new(JuiceCli::new(
        env::required("HERMIT_JUICEFS_META_URL"),
        env::required("HERMIT_JUICEFS_META_PASSWORD"),
    ));
    let ws = &config.file.sandbox.workspace;
    Ok(JuiceFs::new(MOUNT, cli, ws.quota_gib, ws.quota_inodes))
}

fn pool(var: &str, name: &str) -> Result<aviary_pg::Pool, ExitCode> {
    aviary_pg::connect(&env::required(var), name, 2, Duration::from_secs(120)).map_err(|e| {
        tracing::error!(name = var, error = %e, "invalid database url");
        ExitCode::from(2)
    })
}

/// How long a one-shot command waits for its store's background work (an unfinished clone's removal, a late
/// rename's quota) before it exits.
const SETTLE: Duration = Duration::from_secs(60);

async fn import_jarvis(delta: bool, final_pass: bool) -> ExitCode {
    let (hermit, source, cloudcli) = match (
        pool("HERMIT_DATABASE_URL", "hermit:import-jarvis"),
        pool("HERMIT_IMPORT_JARVIS_URL", "hermit:import-jarvis"),
        pool("HERMIT_IMPORT_CLOUDCLI_URL", "hermit:import-jarvis"),
    ) {
        (Ok(h), Ok(s), Ok(c)) => (h, s, c),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
    };
    let fs = match store() {
        Ok(s) => s,
        Err(e) => return e,
    };
    let opts = hermit::legacy::ImportOptions {
        delta,
        final_pass,
        ..hermit::legacy::ImportOptions::default()
    };
    let shares = hermit::legacy::ShareSource {
        cloudcli: &cloudcli,
        store: &fs,
    };
    let res = hermit::legacy::import_jarvis(&hermit, &source, Some(shares), std::path::Path::new(MOUNT), opts).await;
    let left = fs.settle(SETTLE).await;
    if left > 0 {
        tracing::warn!(
            tasks = left,
            "import-jarvis exits with clean-ups still running (hermit's sweep finishes them)"
        );
    }
    match res {
        Ok(r) => {
            tracing::info!(report = %serde_json::to_string(&r).unwrap_or_default(), "import-jarvis done");
            if r.complete() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "import-jarvis failed (the next run retries)");
            ExitCode::FAILURE
        }
    }
}

async fn jfs_owner(root: PathBuf) -> ExitCode {
    let res = tokio::task::spawn_blocking(move || {
        hermit::legacy::fix_owner(&root, hermit::legacy::SANDBOX_UID, hermit::legacy::SANDBOX_UID, 16)
    })
    .await;
    match res {
        Ok(Ok(r)) => {
            tracing::info!(report = %serde_json::to_string(&r).unwrap_or_default(), "jfs-owner done");
            if r.error_count == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "jfs-owner: ws/ not read");
            ExitCode::FAILURE
        }
        Err(e) => {
            tracing::error!(error = %e, "jfs-owner");
            ExitCode::FAILURE
        }
    }
}

async fn legacy_cleanup(confirm: bool) -> ExitCode {
    let hermit = match pool("HERMIT_DATABASE_URL", "hermit:legacy-cleanup") {
        Ok(h) => h,
        Err(e) => return e,
    };
    let cli = JuiceCli::new(
        env::required("HERMIT_JUICEFS_META_URL"),
        env::required("HERMIT_JUICEFS_META_PASSWORD"),
    );
    let magpie = magpie_api::client::Client::new(
        hermit::ops::http(),
        "http://magpie",
        aviary_core::auth::outbound_token("HERMIT", "MAGPIE"),
    );
    match hermit::legacy::legacy_cleanup(std::path::Path::new(MOUNT), &hermit, &magpie, &cli, confirm).await {
        Ok(hermit::legacy::Cleanup::Refused { plan }) => {
            let shown = serde_json::to_string(&plan).unwrap_or_default();
            tracing::warn!(plan = %shown, "legacy-cleanup refused without --confirm: nothing removed");
            ExitCode::from(2)
        }
        Ok(hermit::legacy::Cleanup::Blocked { plan }) => {
            let shown = serde_json::to_string(&plan).unwrap_or_default();
            tracing::error!(plan = %shown, "legacy-cleanup refused: old share folders are not carried over (shares_not_carried); nothing removed");
            ExitCode::FAILURE
        }
        Ok(hermit::legacy::Cleanup::Done { plan, done }) => {
            let (p, d) = (
                serde_json::to_string(&plan).unwrap_or_default(),
                serde_json::to_string(&done).unwrap_or_default(),
            );
            tracing::info!(plan = %p, done = %d, "legacy-cleanup done");
            if done.complete(&plan) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "legacy-cleanup failed (nothing removed when the plan failed)");
            ExitCode::FAILURE
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
    let build = build();
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
    // the fabric's credentials (8.4): mTLS to sandboxd, the AWS hosts' certificate, EC2, JuiceFS
    let fabric_tls = sandboxd_api::Tls {
        ca_pem: env::required("HERMIT_FABRIC_CA"),
        cert_pem: env::required("HERMIT_FABRIC_CLIENT_CERT"),
        key_pem: env::required("HERMIT_FABRIC_CLIENT_KEY"),
    };
    let host_secrets = HostSecrets {
        ca: fabric_tls.ca_pem.clone(),
        cert: env::required("HERMIT_FABRIC_HOST_CERT"),
        key: env::required("HERMIT_FABRIC_HOST_KEY"),
        juicefs_meta_url: env::required("HERMIT_JUICEFS_META_URL"),
        juicefs_meta_password: env::required("HERMIT_JUICEFS_META_PASSWORD"),
    };
    let aws_key = (
        env::required("HERMIT_AWS_ACCESS_KEY_ID"),
        env::required("HERMIT_AWS_SECRET_ACCESS_KEY"),
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
    // the Kubernetes client builds its TLS from the process's provider: ring, the workspace's only one
    let _ = rustls::crypto::ring::default_provider().install_default();
    let egress: Arc<dyn EgressSource> = match Watched::start(&namespace, &config.file.fabric.egress_service).await {
        Ok(w) => Arc::new(w),
        Err(e) => {
            // the gateway keeps its last targets; the ops page shows that none were read
            tracing::error!(error = %e, "egress endpoint watch not started");
            Arc::new(Fixed::default())
        }
    };
    let fabric = Arc::new(Gvisor::new(Deps {
        connect: Arc::new(sandboxd::Mtls::new(fabric_tls)),
        ec2: Some(Arc::new(ec2::Aws::new(
            &config.file.fabric.aws.region,
            &aws_key.0,
            &aws_key.1,
        ))),
        egress,
        host_secrets: Some(host_secrets),
        control: Arc::new(Port7070),
    }));

    let app = loop {
        let options = StartOptions {
            config: config.clone(),
            catalogue: staff.clone(),
            reload_catalogue: true,
            keys: keys.clone(),
            database_url: database_url.clone(),
            pod: pod.clone(),
            pool_size: 10,
            providers: Providers {
                fabric: fabric.clone(),
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
