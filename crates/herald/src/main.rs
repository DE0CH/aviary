//! `herald serve`: one replica. `herald login <phone>`: signs the account in (the login code, then the 2FA password
//! when the account has one, each read as a line from stdin) and writes the session for `HERALD_TELEGRAM_SESSION` to
//! stdout. `herald --version` prints the build.

use std::io::{BufRead as _, Write as _};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use aviary_core::auth::{Caller, Callers};
use aviary_core::{drain, env, log, ops, version};
use grammers_client::session::SessionData;
use grammers_client::session::storages::MemorySession;
use grammers_client::{Client, SenderPool, SignInError};
use herald::app::{self, AppState};
use herald::telegram::{self, Live};
use herald::{handle, session};

/// The API port (platform 3.1).
const API_ADDR: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 8080);

fn build() -> version::Build {
    version::Build::new("herald").surface("herald", 1)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = || tokio::runtime::Runtime::new().ok();
    match args.first().map(String::as_str) {
        Some("--version") => {
            let _ = writeln!(std::io::stdout().lock(), "{}", build().version_line());
            ExitCode::SUCCESS
        }
        Some("serve") => rt().map_or(ExitCode::FAILURE, |rt| rt.block_on(serve())),
        Some("login") if args.len() == 2 => rt().map_or(ExitCode::FAILURE, |rt| rt.block_on(login(&args[1]))),
        _ => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "usage: herald serve | herald login <phone> | herald --version"
            );
            ExitCode::from(2)
        }
    }
}

fn api_id() -> i32 {
    let raw = env::required("HERALD_TELEGRAM_API_ID");
    raw.parse()
        .unwrap_or_else(|_| invalid("HERALD_TELEGRAM_API_ID", "not a number"))
}

/// A variable that is set but unusable: one error line naming it, never its value, and exit 78.
fn invalid(name: &str, why: &str) -> ! {
    let _ = writeln!(std::io::stderr().lock(), "herald: invalid env {name}: {why}");
    std::process::exit(env::EXIT_CONFIG)
}

async fn serve() -> ExitCode {
    let build = build();
    log::init_in_pod(&build, log::Output::Stdout);
    let registry = ops::registry(&build);
    let callers = Callers::from_env("HERALD", &[Caller::Gateway]);
    let key = env::required("HERALD_HANDLE_KEY");
    if key.len() < 32 {
        invalid("HERALD_HANDLE_KEY", "shorter than 32 characters");
    }
    let cfg = telegram::Config {
        api_id: api_id(),
        session: env::required("HERALD_TELEGRAM_SESSION"),
        bot_token: env::required("HERALD_BOT_TOKEN"),
        socks: env::required("HERALD_SOCKS"),
        version: build.version_line(),
    };
    // a prefix may end in a space, which `env::required` trims: the separator is herald's
    let title_prefix = format!("{} ", env::required("HERALD_TITLE_PREFIX"));
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
    let gate = ops.readiness().gate("telegram");
    // Telegram unreachable at start: retry; a signal meanwhile ends the process
    let live = loop {
        let attempt = tokio::select! {
            r = Live::start(&cfg) => r,
            s = signals.recv() => {
                tracing::info!(signal = ?s, "signal during start");
                return ExitCode::SUCCESS;
            }
        };
        match attempt {
            Ok(l) => break l,
            Err(e @ (telegram::StartError::Session(_) | telegram::StartError::NotSignedIn)) => {
                tracing::error!(error = %e, "start failed");
                return ExitCode::from(78); // EX_CONFIG
            }
            Err(e) => {
                tracing::warn!(error = %e, "telegram not reachable yet; retrying in 10 s");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    };
    gate.open();
    let state = AppState {
        telegram: Arc::new(live),
        key: handle::Key::new(&key),
        title_prefix,
    };
    let listener = match tokio::net::TcpListener::bind(API_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen on the API port");
            return ExitCode::FAILURE;
        }
    };
    let mut drain = drain::Drain::new(ops.readiness().clone());
    let mut draining = drain.draining();
    let router = app::router(state, &callers);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { draining.wait().await })
            .await
    });
    ops.readiness().started();
    tracing::info!("herald serving");
    drain.hook("in-flight requests", Duration::from_secs(40), || async move {
        let _ = server.await;
    });
    drain::exit(drain.run(&mut signals).await)
}

fn read_line(prompt: &str) -> Option<String> {
    let _ = write!(std::io::stderr().lock(), "{prompt}");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let line = line.trim().to_owned();
    (!line.is_empty()).then_some(line)
}

async fn login(phone: &str) -> ExitCode {
    let mut err = std::io::stderr().lock();
    let hash = env::required("HERALD_TELEGRAM_API_HASH");
    let socks = env::required("HERALD_SOCKS");
    let session = Arc::new(MemorySession::from(SessionData::default()));
    let pool = SenderPool::with_configuration(
        Arc::clone(&session),
        api_id(),
        telegram::connection_params(&socks, &build().version_line()),
    );
    let client = Client::new(pool.handle);
    tokio::spawn(pool.runner.run());
    let token = match client.request_login_code(phone, &hash).await {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(err, "login: requesting the code failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _ = writeln!(err, "login: code requested");
    let Some(code) = read_line("code: ") else {
        return ExitCode::FAILURE;
    };
    match client.sign_in(&token, &code).await {
        Ok(_) => {}
        Err(SignInError::PasswordRequired(pw)) => {
            let Some(password) = read_line("2FA password: ") else {
                return ExitCode::FAILURE;
            };
            if let Err(e) = client.check_password(pw, password.as_bytes()).await {
                let _ = writeln!(err, "login: the password was refused: {e}");
                return ExitCode::FAILURE;
            }
        }
        Err(e) => {
            let _ = writeln!(err, "login: sign-in failed: {e}");
            return ExitCode::FAILURE;
        }
    }
    match session::store(&session) {
        Ok(text) => {
            let _ = writeln!(std::io::stdout().lock(), "{text}");
            let _ = writeln!(err, "login: signed in; the session is on stdout");
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = writeln!(err, "login: cannot read the session: {e}");
            ExitCode::FAILURE
        }
    }
}
