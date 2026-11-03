//! `magpie import <command>` (cutover 7.9). Each command prints one JSON document on standard output (logs go to
//! standard error) and exits 0, 1 when it found a blocker or a failure, 2 on a usage error.
//!
//! Bindings (platform 12.2): magpie's own (`MAGPIE_DATABASE_URL`, `MAGPIE_S3_*`) from Secret `magpie`, the old
//! databases from Secret `magpie-import` (`MAGPIE_IMPORT_CLOUDCLI_URL`, `MAGPIE_IMPORT_JARVIS_URL`; every read is a
//! read-only transaction), the `JuiceFS` mount at [`JFS`], `POD_NAME`.
//! The hand-over loop and the commands that drive it (`handover`, `holds`, `allow-handover`, `reverse`, `parity`,
//! `export`) are not built yet.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use aviary_core::{env, log, ops, version};
use serde::Serialize;
use uuid::Uuid;

use crate::Importer;

/// Where the `JuiceFS` volume is mounted (read-only) in the import Job and Deployment (cutover 7.2).
pub const JFS: &str = "/jfs";

const USAGE: &str = "usage: magpie import plan | run | catch-up | verify [--all | <id>…] | status | hold on|off \
                     | reset --yes | unfence <id> | release <id> --force\n";

fn out(v: &impl Serialize) {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"));
    s.push('\n');
    let _ = std::io::Write::write_all(&mut std::io::stdout().lock(), s.as_bytes());
}

fn usage() -> ExitCode {
    let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), USAGE.as_bytes());
    ExitCode::from(2)
}

fn ids(args: &[String]) -> Option<Vec<Uuid>> {
    args.iter().map(|a| Uuid::parse_str(a).ok()).collect()
}

/// Connects to magpie (its library, no jobs) and the old database.
///
/// # Errors
/// A database URL that does not parse, a magpie start that cannot go on.
pub async fn connect(build: &version::Build) -> Result<Importer, String> {
    let mut registry = ops::registry(build);
    let legacy_metrics = magpie::legacy::Metrics::register(&mut registry);
    let cfg = magpie::app::Config {
        database_url: env::required("MAGPIE_DATABASE_URL"),
        callers: aviary_core::auth::Callers::new(Vec::<(aviary_core::auth::Caller, Vec<String>)>::new())
            .map_err(|e| e.to_string())?,
        hermit_url: "http://hermit".to_owned(),
        hermit_token: env::try_required("MAGPIE_TOKEN_HERMIT").unwrap_or_default(),
        pod: env::field("POD_NAME"),
        bucket: magpie::blobs::store::BucketConfig {
            endpoint: env::required("MAGPIE_S3_ENDPOINT"),
            region: env::required("MAGPIE_S3_REGION"),
            bucket: env::required("MAGPIE_S3_BUCKET"),
            access_key_id: env::required("MAGPIE_S3_ACCESS_KEY_ID"),
            secret_access_key: env::required("MAGPIE_S3_SECRET_ACCESS_KEY"),
        },
        import_url: magpie::legacy::ingest::URL.to_owned(),
        import_token: String::new(),
        legacy_metrics,
    };
    let readiness = aviary_core::ops::Readiness::detached();
    let drain = aviary_core::drain::Drain::new(readiness.clone());
    let service = magpie::app::Service::start_library(cfg, &readiness, drain.draining())
        .await
        .map_err(|e| e.to_string())?;
    let cloudcli = aviary_pg::connect(
        &env::required("MAGPIE_IMPORT_CLOUDCLI_URL"),
        "magpie-import",
        4,
        Duration::from_secs(120),
    )
    .map_err(|e| e.to_string())?;
    let jarvis = aviary_pg::connect(
        &env::required("MAGPIE_IMPORT_JARVIS_URL"),
        "magpie-import",
        2,
        Duration::from_secs(120),
    )
    .map_err(|e| e.to_string())?;
    Ok(Importer {
        magpie: service.app.clone(),
        cloudcli,
        jarvis,
        jfs: PathBuf::from(JFS),
    })
}

/// Runs `magpie import <args>`.
#[must_use]
pub fn main(build: &version::Build, args: &[String]) -> ExitCode {
    let Some(cmd) = args.first().map(String::as_str) else {
        return usage();
    };
    let rest = &args[1..];
    let known = matches!(
        (cmd, rest.len()),
        ("plan" | "run" | "catch-up" | "status", 0) | ("verify" | "hold" | "reset" | "unfence", _) | ("release", 2)
    );
    if !known {
        return usage();
    }
    log::init_in_pod(build, log::Output::Stderr);
    let Ok(rt) = tokio::runtime::Runtime::new() else {
        return ExitCode::FAILURE;
    };
    rt.block_on(async {
        let imp = match connect(build).await {
            Ok(i) => i,
            Err(e) => {
                tracing::error!(error = %e, "cannot connect");
                return ExitCode::FAILURE;
            }
        };
        run(&imp, cmd, rest).await
    })
}

#[allow(clippy::too_many_lines)] // one arm per command
async fn run(imp: &Importer, cmd: &str, rest: &[String]) -> ExitCode {
    let fail = |e: crate::Error| {
        tracing::error!(error = %e, "magpie import {cmd} failed");
        out(&serde_json::json!({ "error": e.to_string() }));
        ExitCode::FAILURE
    };
    match cmd {
        "plan" => match crate::plan::plan(imp).await {
            Ok(p) => {
                out(&p);
                if p.blockers.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => fail(e),
        },
        "run" | "catch-up" => match crate::pass::pass(imp, None).await {
            Ok(r) => {
                out(&r);
                if r.errors.is_empty() && r.conflicts.is_empty() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => fail(e),
        },
        "verify" => {
            let given = if rest == ["--all"] {
                Vec::new()
            } else if rest.is_empty() {
                return usage();
            } else {
                match ids(rest) {
                    Some(i) => i,
                    None => return usage(),
                }
            };
            let targets = match crate::verify::targets(imp, &given).await {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            let mut verdicts = Vec::new();
            let mut ok = true;
            for id in targets {
                match crate::verify::verify(imp, id).await {
                    Ok(v) => {
                        ok &= v.ok();
                        verdicts.push(serde_json::to_value(&v).unwrap_or_default());
                    }
                    Err(e) => {
                        ok = false;
                        verdicts.push(serde_json::json!({ "conversation": id, "error": e.to_string() }));
                    }
                }
            }
            out(&verdicts);
            if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        "status" => match crate::control::status(imp).await {
            Ok(s) => {
                out(&s);
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        "hold" => {
            let on = match rest {
                [a] if a == "on" => true,
                [a] if a == "off" => false,
                _ => return usage(),
            };
            match crate::control::hold(imp, on).await {
                Ok(()) => {
                    out(&serde_json::json!({ "hold": on }));
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        "reset" => {
            if rest != ["--yes"] {
                return usage();
            }
            match crate::control::reset(imp).await {
                Ok(n) => {
                    out(&serde_json::json!({ "removed": n }));
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        "unfence" => {
            let [a] = rest else { return usage() };
            let Ok(id) = Uuid::parse_str(a) else { return usage() };
            match crate::control::unfence(imp, id).await {
                Ok(b) => {
                    out(&serde_json::json!({ "unfenced": b }));
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        "release" => {
            let (Some(id), true) = (Uuid::parse_str(&rest[0]).ok(), rest[1] == "--force") else {
                return usage();
            };
            match crate::control::release_forced(imp, id).await {
                Ok(f) => {
                    out(&f);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        _ => usage(),
    }
}
