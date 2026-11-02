//! `magpie serve`: one replica (magpie 18). `magpie --version` prints the build. `magpie check-image <file>` decodes
//! an image as an upload would and prints its type and upright size (the image build's check that libheif and its
//! decoder plugins are there).

use std::process::ExitCode;
use std::time::Duration;

use aviary_core::{drain, env, log, ops, version};
use magpie::app::{Config, Service};
use magpie::{auth, limits};

/// The API port (platform 3.1).
const API_ADDR: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 8080);

/// hermit at its fixed endpoint name (magpie 1).
const HERMIT_URL: &str = "http://hermit";

fn build() -> version::Build {
    version::Build::new("magpie")
        .surface(magpie_api::SURFACE_PERSON, magpie_api::PROTOCOL_PERSON)
        .surface(magpie_api::SURFACE_SANDBOX, magpie_api::PROTOCOL_SANDBOX)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            let mut out = std::io::stdout().lock();
            let _ = std::io::Write::write_all(&mut out, format!("{}\n", build().version_line()).as_bytes());
            ExitCode::SUCCESS
        }
        Some("check-image") => check_image(args.get(1).map(String::as_str)),
        Some("serve") => {
            let Ok(rt) = tokio::runtime::Runtime::new() else {
                return ExitCode::FAILURE;
            };
            rt.block_on(serve())
        }
        _ => {
            let mut err = std::io::stderr().lock();
            let _ = std::io::Write::write_all(
                &mut err,
                b"usage: magpie serve | magpie --version | magpie check-image <file>\n",
            );
            ExitCode::from(2)
        }
    }
}

fn check_image(path: Option<&str>) -> ExitCode {
    use magpie::blobs::images;
    let mut err = std::io::stderr().lock();
    let Some(bytes) = path.and_then(|p| std::fs::read(p).ok()) else {
        let _ = std::io::Write::write_all(&mut err, b"check-image: no readable file\n");
        return ExitCode::from(2);
    };
    let Some(mime) = images::sniff(&bytes).filter(|m| images::is_image(m)) else {
        let _ = std::io::Write::write_all(&mut err, b"check-image: not an image type magpie reads\n");
        return ExitCode::FAILURE;
    };
    match images::decode(&bytes, mime) {
        Ok(img) => {
            let line = format!("{mime} {}x{}\n", img.width(), img.height());
            let _ = std::io::Write::write_all(&mut std::io::stdout().lock(), line.as_bytes());
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = std::io::Write::write_all(&mut err, format!("check-image: {e}\n").as_bytes());
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> ExitCode {
    let build = build();
    log::init_in_pod(&build, log::Output::Stdout);
    let cfg = Config {
        database_url: env::required("MAGPIE_DATABASE_URL"),
        callers: auth::callers_from_env(),
        hermit_url: HERMIT_URL.to_owned(),
        hermit_token: aviary_core::auth::outbound_token("MAGPIE", "HERMIT"),
        pod: env::field("POD_NAME"),
        bucket: magpie::blobs::store::BucketConfig {
            endpoint: env::required("MAGPIE_S3_ENDPOINT"),
            region: env::required("MAGPIE_S3_REGION"),
            bucket: env::required("MAGPIE_S3_BUCKET"),
            access_key_id: env::required("MAGPIE_S3_ACCESS_KEY_ID"),
            secret_access_key: env::required("MAGPIE_S3_SECRET_ACCESS_KEY"),
        },
    };
    let registry = ops::registry(&build);
    let ops = ops::Ops::new(build, registry);
    if let Err(e) = ops.spawn(ops::ADDR).await {
        tracing::error!(error = %e, "cannot listen on the ops port");
        return ExitCode::FAILURE;
    }
    let mut signals = match drain::Signals::os() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot install signal handlers");
            return ExitCode::FAILURE;
        }
    };
    let mut drain = drain::Drain::new(ops.readiness().clone());
    // the start retries while Postgres cannot be reached: a signal meanwhile ends the process (nothing to drain yet)
    let start = Service::start(cfg, ops.readiness(), drain.draining());
    let started = tokio::select! {
        r = start => Some(r),
        s = signals.recv() => {
            tracing::info!(signal = ?s, "signal during start");
            None
        }
    };
    let Some(started) = started else {
        drain::exit(drain.drain(&mut signals).await);
    };
    let service = match started {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "start failed");
            return ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(API_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen on the API port");
            return ExitCode::FAILURE;
        }
    };
    let mut draining = drain.draining();
    let router = service.router();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { draining.wait().await })
            .await
    });
    ops.readiness().started();
    tracing::info!("magpie serving");
    // held polls answer at once (they watch `draining`), the listener stops taking new connections, in-flight
    // requests finish; then the claims, the leader lock and the pool
    drain.hook("in-flight requests", limits::DRAIN_IN_FLIGHT, || async move {
        let _ = server.await;
    });
    drain.hook("claims, jobs, pool", Duration::from_secs(10), || async move {
        service.shutdown().await;
    });
    let outcome = drain.run(&mut signals).await;
    drain::exit(outcome)
}
