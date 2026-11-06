//! `magpie import <command>` (cutover 7.9). Each command prints one JSON document on standard output (logs go to
//! standard error) and exits 0, 1 when it found a blocker or a failure, 2 on a usage error.
//!
//! Bindings (platform 12.2): magpie's own (`MAGPIE_DATABASE_URL`, `MAGPIE_S3_*`) from Secret `magpie`, the old
//! databases from Secret `magpie-import` (`MAGPIE_IMPORT_CLOUDCLI_URL`, `MAGPIE_IMPORT_JARVIS_URL`; every read is a
//! read-only transaction, and every command but `export` connects with `default_transaction_read_only = on`, cutover
//! 7.2), the `JuiceFS` mount at [`JFS`], `POD_NAME`.
//! `holds` reads the running sets through the old cluster's API ([`crate::kube`]'s bindings). The hand-over loop
//! and the commands that drive it: `handover` (the loop, [`crate::server::handover`]), `allow-handover [--person
//! <email>]` (the gate, for everyone or one person), `export-server` (the writer sidecar's API) and `reverse <id>…`
//! (cutover 14.2, [`crate::reverse`]; run in the hand-over container, it exports through the sidecar). `export` is
//! the only command that writes the old side (`JuiceFS` and `cloudcli`): it runs in the writer sidecar, whose mount
//! and `cloudcli` connection are read-write (cutover 14.3).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use aviary_core::{env, log, ops, version};
use serde::Serialize;
use uuid::Uuid;

use crate::Importer;

/// Where the `JuiceFS` volume is mounted (read-only) in the import Job and Deployment (cutover 7.2).
pub const JFS: &str = "/jfs";

/// The `JuiceFS` client in the import image (the official static build), used for metadata-only clones.
pub const JUICEFS: &str = "/usr/local/bin/juicefs";

const USAGE: &str = "usage: magpie import plan | run | catch-up | verify [--all | <id>…] | status | holds | hold on|off \
                     | reset --yes | unfence <id> | release <id> --force | purge <id>… \
                     | export [--unfenced] [--since <time>] [<id>…] | parity <id>… | handover | export-server \
                     | allow-handover [--person <email>] | reverse <id>…\n";

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

/// Connects to magpie (its library, no jobs) and the old databases: read-only sessions unless `writes_old_side` (the
/// export, the only command that writes `cloudcli`; `jarvis` is always read-only).
///
/// # Errors
/// A database URL that does not parse, a magpie start that cannot go on.
pub async fn connect(build: &version::Build, writes_old_side: bool) -> Result<Importer, String> {
    connect_with(build, writes_old_side).await.map(|(i, _)| i)
}

/// [`connect`], also answering the registry the importer's metrics are on (the hand-over's `/metrics`).
///
/// # Errors
/// As [`connect`].
pub async fn connect_with(
    build: &version::Build,
    writes_old_side: bool,
) -> Result<(Importer, prometheus_client::registry::Registry), String> {
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
    let cloudcli = if writes_old_side {
        aviary_pg::connect(
            &env::required("MAGPIE_IMPORT_CLOUDCLI_URL"),
            "magpie-import",
            4,
            Duration::from_secs(120),
        )
    } else {
        aviary_pg::connect_read_only(
            &env::required("MAGPIE_IMPORT_CLOUDCLI_URL"),
            "magpie-import",
            4,
            Duration::from_secs(120),
        )
    }
    .map_err(|e| e.to_string())?;
    // the old schedules' hand-over writes them (schedules::claim); nothing else is written on this pool
    let cloudcli_schedules = aviary_pg::connect(
        &env::required("MAGPIE_IMPORT_CLOUDCLI_URL"),
        "magpie-import-schedules",
        2,
        Duration::from_secs(120),
    )
    .map_err(|e| e.to_string())?;
    let jarvis = aviary_pg::connect_read_only(
        &env::required("MAGPIE_IMPORT_JARVIS_URL"),
        "magpie-import",
        2,
        Duration::from_secs(120),
    )
    .map_err(|e| e.to_string())?;
    let imp = Importer {
        magpie: service.app.clone(),
        cloudcli,
        cloudcli_schedules,
        jarvis,
        jfs: PathBuf::from(JFS),
        juicefs: Some(PathBuf::from(JUICEFS)),
        lock: None,
        sent_quiet: crate::sent::QUIET,
        pause_in_fence: std::time::Duration::ZERO,
        owner: Some(crate::export::OWNER_ID),
    };
    Ok((imp, registry))
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
        (
            "plan" | "run" | "catch-up" | "status" | "holds" | "handover" | "export-server",
            0
        ) | ("allow-handover", 0 | 2)
            | ("parity" | "purge" | "reverse", 1..)
            | ("verify" | "hold" | "reset" | "unfence" | "export", _)
            | ("release", 2)
    );
    if !known {
        return usage();
    }
    log::init_in_pod(build, log::Output::Stderr);
    let Ok(rt) = tokio::runtime::Runtime::new() else {
        return ExitCode::FAILURE;
    };
    rt.block_on(async {
        if matches!(cmd, "handover" | "export-server") {
            let (imp, registry) = match connect_with(build, cmd == "export-server").await {
                Ok(i) => i,
                Err(e) => {
                    tracing::error!(error = %e, "cannot connect");
                    return ExitCode::FAILURE;
                }
            };
            let ops = ops::Ops::new(build.clone(), registry);
            return if cmd == "handover" {
                crate::server::handover(imp, ops).await
            } else {
                crate::server::export_server(imp, ops).await
            };
        }
        let mut imp = match connect(build, cmd == "export").await {
            Ok(i) => i,
            Err(e) => {
                tracing::error!(error = %e, "cannot connect");
                return ExitCode::FAILURE;
            }
        };
        // every command that writes the importer's state holds the leader lock while it runs (cutover 7.1)
        if !matches!(cmd, "plan" | "status" | "holds" | "parity") {
            match crate::lock::OneShot::take(&imp.magpie.pool).await {
                Ok(l) => imp.lock = Some(std::sync::Arc::new(l)),
                Err(e) => {
                    tracing::error!(error = %e, "cannot take the importer's lock");
                    return ExitCode::FAILURE;
                }
            }
        }
        let code = run(&imp, cmd, rest).await;
        if let Some(l) = imp.lock.take().and_then(|l| std::sync::Arc::try_unwrap(l).ok()) {
            l.release().await;
        }
        code
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
                if let Err(e) = imp.check_lock().await {
                    return fail(e);
                }
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
            // `verify --all` clean: the record hermit's legacy-cleanup asks for (cutover 10.6); the output stays the
            // verdicts, the outcome is logged
            if ok && given.is_empty() {
                let done: std::collections::BTreeSet<Uuid> = verdicts
                    .iter()
                    .filter_map(|v| v["conversation"].as_str().and_then(|c| Uuid::parse_str(c).ok()))
                    .collect();
                match crate::verify::record_complete(imp, &done).await {
                    Ok(None) => {}
                    Ok(Some(why)) => tracing::warn!(why = %why, "verify --all clean, the import not recorded complete"),
                    Err(e) => return fail(e),
                }
            }
            out(&verdicts);
            if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        "allow-handover" => {
            let person = match rest {
                [] => None,
                [flag, p] if flag == "--person" && p.contains('@') => Some(p.trim().to_lowercase()),
                _ => return usage(),
            };
            match crate::kube::Kube::from_env() {
                Ok(k) => {
                    let token = env::required("MAGPIE_IMPORT_TOKEN_UI");
                    match crate::handover::allow(imp, &k, &token, person.as_deref()).await {
                        Ok((check, set)) => {
                            out(&serde_json::json!({
                                "gateSet": set,
                                "personKey": person.as_deref().map(aviary_core::log::person_key),
                                "check": check,
                                "problems": check.problems(),
                            }));
                            if set { ExitCode::SUCCESS } else { ExitCode::FAILURE }
                        }
                        Err(e) => fail(e),
                    }
                }
                Err(e) => fail(e),
            }
        }
        "reverse" => {
            let Some(given) = ids(rest) else { return usage() };
            let kube = match crate::kube::Kube::from_env() {
                Ok(k) => k,
                Err(e) => return fail(e),
            };
            let token = env::required("MAGPIE_IMPORT_TOKEN_UI");
            let url =
                env::try_required("MAGPIE_IMPORT_EXPORT_URL").unwrap_or_else(|_| crate::reverse::EXPORT_URL.to_owned());
            let old = crate::reverse::Old {
                kube: &kube,
                ui_token: &token,
                export_url: &url,
            };
            match crate::reverse::reverse(imp, &old, &given).await {
                Ok(r) => {
                    out(&r);
                    if r.iter().all(|x| x.outcome == crate::reverse::Outcome::Reversed) {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    }
                }
                Err(e) => fail(e),
            }
        }
        "holds" => match crate::kube::Kube::from_env() {
            Ok(k) => {
                let set = crate::kube::running_set(&k).await;
                out(&set);
                if set.complete {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => fail(e),
        },
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
        "purge" => {
            let Some(given) = ids(rest) else { return usage() };
            match crate::control::purge(imp, &given).await {
                Ok(r) => {
                    out(&r);
                    if r.iter().all(|p| p.error.is_none()) {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    }
                }
                Err(e) => fail(e),
            }
        }
        "parity" => {
            let Some(given) = ids(rest) else { return usage() };
            match crate::parity::parity(imp, &given).await {
                Ok(r) => {
                    out(&r);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        "export" => {
            let (unfenced, rest) = match rest {
                [flag, more @ ..] if flag == "--unfenced" => (true, more),
                _ => (false, rest),
            };
            let (since, rest) = match rest {
                [flag, t, more @ ..] if flag == "--since" => {
                    match time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339) {
                        Ok(t) => (Some(t), more),
                        Err(_) => return usage(),
                    }
                }
                _ => (None, rest),
            };
            let Some(given) = ids(rest) else { return usage() };
            match crate::export::export(imp, since, &given, unfenced).await {
                Ok(x) => {
                    out(&x);
                    if x.errors.is_empty() {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    }
                }
                Err(e) => fail(e),
            }
        }
        _ => usage(),
    }
}
