//! `aviary-e2e --env-file F [scenario…]`: runs the harness's scenarios (platform 7.8) from inside the
//! harness cluster (testing/e2e/run.sh `test`), in order, and fails on the first failure. HTTPS trusts the
//! harness CA through `SSL_CERT_FILE` (the Job sets it).

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "aviary's end-to-end driver")]
struct Args {
    /// The admin and monitor tokens, `KEY=VALUE` lines (testing/e2e/gen.sh's driver.env).
    #[arg(long)]
    env_file: PathBuf,
    /// Scenarios (`e1` …); default all.
    scenarios: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();
    let mut err = std::io::stderr();
    let env = match aviary_e2e::read_env(&args.env_file) {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(err, "aviary-e2e: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let h = match aviary_e2e::Harness::new(env).await {
        Ok(h) => h,
        Err(e) => {
            let _ = writeln!(err, "aviary-e2e: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let names: Vec<String> = if args.scenarios.is_empty() {
        aviary_e2e::scenarios::ALL.iter().map(|s| (*s).to_owned()).collect()
    } else {
        args.scenarios
    };
    for name in &names {
        let t = Instant::now();
        match aviary_e2e::scenarios::run(&h, name).await {
            Ok(()) => {
                let _ = writeln!(err, "PASS {name} ({:.1} s)", t.elapsed().as_secs_f64());
            }
            Err(e) => {
                let _ = writeln!(err, "FAIL {name} ({:.1} s): {e:#}", t.elapsed().as_secs_f64());
                return ExitCode::FAILURE;
            }
        }
    }
    let _ = writeln!(err, "all passed: {}", names.join(" "));
    ExitCode::SUCCESS
}
