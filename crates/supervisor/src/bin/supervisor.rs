//! `supervisor`: the in-sandbox supervisor (supervisor spec). The binary dispatches on how it is called:
//!
//! - `supervisor run`: the supervisor, started by the keeper (exit 0 after a release, 70 on an internal error);
//! - `sandbox-secret <KEY>` (a symlink to this binary): a secret from hermit, through the supervisor;
//! - `supervisor hook send-user-file`: Claude Code's `SendUserFile` hook (supervisor 13);
//! - as `send-file` (a link to this binary): the same send from a shell command, for Qwen Code;
//! - `supervisor --version`;
//! - `supervisor --speaks`: the surface numbers this build speaks, as JSON (`hermit-sandbox` and `magpie-sandbox` as a
//!   client, `supervisor-control` as the server), for the sandbox image's `dev.de0ch.aviary.speaks` label (platform
//!   5.3).

use std::sync::Arc;

use aviary_core::log;
use aviary_core::version::Build;
use supervisor::boot::BootEnv;
use supervisor::cc::run::{Driver, DriverConfig};
use supervisor::fuse::{Config as FuseConfig, FuseFs, Hooks};
use supervisor::hermit::Hermit;
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
    if argv0 == supervisor::harness::SEND_FILE {
        std::process::exit(supervisor::sent_files::send_file_cli(&args[1..]));
    }
    if argv0 == "sandbox-secret" {
        std::process::exit(supervisor::local::secret_cli(&args[1..]));
    }
    let code = match args.get(1).map(String::as_str) {
        Some("run") => run(),
        Some("hook") if args.get(2).map(String::as_str) == Some("send-user-file") => supervisor::sent_files::hook_cli(),
        Some("--speaks") => {
            use std::io::Write as _;
            let speaks = serde_json::json!({
                supervisor_api::HERMIT_SANDBOX.0: supervisor_api::HERMIT_SANDBOX.1,
                magpie_api::SURFACE_SANDBOX: magpie_api::PROTOCOL_SANDBOX,
                supervisor_api::CONTROL.0: supervisor_api::CONTROL.1,
            });
            let _ = writeln!(std::io::stdout(), "{speaks}");
            0
        }
        Some("--version") => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stdout(), "{}", build().version_line());
            0
        }
        _ => {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "usage: supervisor run | supervisor hook send-user-file | supervisor --version | supervisor --speaks"
            );
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
        let hermit = Hermit::new(http.clone());
        let magpie = supervisor::magpie::client(http.clone());
        let fs = FuseFs::new(FuseConfig::sandbox(), magpie.clone());
        let agent = Driver::new(
            DriverConfig::sandbox(),
            magpie.clone(),
            supervisor::model::Model::new(http),
            hermit.clone(),
            fs.clone(),
        );
        let start = Start {
            paths,
            boot,
            build,
            hermit,
            magpie,
            keeper: supervisor::keeper::Client::new(std::path::Path::new(supervisor::keeper::RUN_DIR)),
            agent: agent.clone(),
            fs: fs.clone(),
            control,
        };
        let (running, done) = start.spawn();
        let (lost, regained) = (running.lifecycle.clone(), running.lifecycle.clone());
        let (a1, a2, a3) = (agent.clone(), agent.clone(), agent.clone());
        // the fault-injection build (never a release image): the point in /run/fault-injection, read once
        #[cfg(feature = "fault-injection")]
        let fault = supervisor::fuse::fault_from_file(std::path::Path::new(supervisor::fuse::FAULT_FILE));
        #[cfg(not(feature = "fault-injection"))]
        let fault = None;
        fs.set_hooks(Hooks {
            // 9.5 item 7: unready while unleased (the generation killed, no more reports for its turn), ready again
            // with the new lease (no exit, no restart)
            lease_lost: Arc::new(move || {
                lost.set_ready(|r| r.lease = false);
                a1.lease_lost();
            }),
            lease_regained: Arc::new(move |l| {
                regained.set_lease_epoch(l.epoch);
                regained.set_ready(|r| r.lease = true);
            }),
            write_stuck: Arc::new(move |s| a2.write_stuck(&s.what, &s.message)),
            forked: Arc::new(move |m| a3.forked(&m)),
            fault,
        });
        done.await.unwrap_or(EXIT_INTERNAL)
    })
}
