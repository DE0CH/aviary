//! `supervisor`: the in-sandbox supervisor (supervisor spec). The binary dispatches on how it is called:
//!
//! - `supervisor run`: the supervisor, started by the keeper (exit 0 after a release, 70 on an internal error);
//! - `sandbox-secret <KEY>` (a symlink to this binary): a secret from hermit, through the supervisor;
//! - `supervisor --version`.

use std::sync::Arc;

use aviary_core::log;
use aviary_core::version::Build;
use supervisor::boot::BootEnv;
use supervisor::hermit::Hermit;
use supervisor::lifecycle::{IdleAgent, UnbuiltFilesystem};
use supervisor::start::{EXIT_INTERNAL, Paths, Start};

fn build() -> Build {
    Build::new("supervisor").surface(supervisor_api::CONTROL.0, supervisor_api::CONTROL.1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let argv0 = args
        .first()
        .map(|a| a.rsplit('/').next().unwrap_or(a).to_owned())
        .unwrap_or_default();
    if argv0 == "sandbox-secret" {
        std::process::exit(supervisor::local::secret_cli(&args[1..]));
    }
    let code = match args.get(1).map(String::as_str) {
        Some("run") => run(),
        Some("--version") => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stdout(), "{}", build().version_line());
            0
        }
        _ => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "usage: supervisor run | supervisor --version");
            2
        }
    };
    std::process::exit(code);
}

fn run() -> i32 {
    let build = build();
    // stderr is the keeper's stdout: the sandbox's console log (supervisor 21.1)
    log::init(&build, None, log::Output::Stderr);
    let boot = BootEnv::from_process();
    if let Err(e) = boot.token() {
        tracing::error!(error = %e, "no sandbox credential");
        return EXIT_INTERNAL;
    }
    let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return EXIT_INTERNAL;
    };
    rt.block_on(async move {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = match boot.fixed_names_client(&build) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "fixed-names client");
                return EXIT_INTERNAL;
            }
        };
        let paths = Paths::sandbox();
        let control = match tokio::net::TcpListener::bind(paths.control).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "bind :7070");
                return EXIT_INTERNAL;
            }
        };
        let start = Start {
            paths,
            boot,
            build,
            hermit: Hermit::new(http.clone()),
            magpie: supervisor::magpie::client(http),
            keeper: supervisor::keeper::Client::new(std::path::Path::new(supervisor::keeper::RUN_DIR)),
            // WP-23 and WP-22 replace these two
            agent: Arc::new(IdleAgent::default()),
            fs: Arc::new(UnbuiltFilesystem),
            control,
        };
        let (_running, done) = start.spawn();
        done.await.unwrap_or(EXIT_INTERNAL)
    })
}
