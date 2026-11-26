//! aviary-deploy: the deploy run's decisions (docs/DEPLOY.md).
//!
//!   aviary-deploy tags deploy/images.json [REV]           image → tag (JSON)
//!   aviary-deploy ready HEAD WORKFLOW,… [--again]          go SHA | wait … | nothing … (stdout); red = exit 1:
//!                                                           the newest green commit newer than what runs (decide.rs)
//!   aviary-deploy digests TAGS.json                         image → {tag, digest}; a missing image = exit 1
//!   aviary-deploy set KUSTOMIZATION TAGS.json               the tags written into its `images:`
//!   aviary-deploy fabric DIR TAGS.json DIGESTS.json         Jarvis 4's fabric (DE0CH/jarvis4 k8s/cluster/fabric): hermit's tag and
//!                                                           digest, hermit.json's fabric.sandboxd
//!   aviary-deploy yyc DIR TAGS.json                         YYC's folder (yyc/deploy): its kustomization's tags, hermit.json's
//!                                                           release and sandbox digest, gateway.release
//!
//! `ready` reads GitHub with `GITHUB_TOKEN` for `GITHUB_REPOSITORY` (`GITHUB_API_URL`, default api.github.com).

mod decide;
mod kustomize;
mod registry;
mod tags;
mod yyc;

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

fn env(k: &str) -> Result<String> {
    std::env::var(k).with_context(|| format!("{k} is not set"))
}

fn read_tags(p: &str) -> Result<BTreeMap<String, String>> {
    serde_json::from_str(&std::fs::read_to_string(p).with_context(|| format!("read {p}"))?)
        .with_context(|| format!("parse {p}"))
}

fn gh(path: &str) -> Result<serde_json::Value> {
    let api = std::env::var("GITHUB_API_URL").unwrap_or_else(|_| "https://api.github.com".into());
    let tok = env("GITHUB_TOKEN")?;
    ureq::get(&format!("{api}{path}"))
        .header("Authorization", &format!("Bearer {tok}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("GET {path}"))?
        .body_mut()
        .read_json()
        .with_context(|| format!("GET {path} body"))
}

/// the git of the checkout, in the current directory
fn git(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git").args(args).output().context("git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn runs_of(v: &serde_json::Value) -> Vec<(String, decide::Run)> {
    v["workflow_runs"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    let run = decide::Run {
                        id: r["id"].as_u64().unwrap_or_default(),
                        workflow: r["name"].as_str().unwrap_or_default().to_string(),
                        status: r["status"].as_str().unwrap_or_default().to_string(),
                        conclusion: r["conclusion"].as_str().map(String::from),
                        on_main: r["head_branch"].as_str() == Some("main")
                            && matches!(r["event"].as_str(), Some("push" | "workflow_dispatch" | "schedule")),
                    };
                    (r["head_sha"].as_str().unwrap_or_default().to_string(), run)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `ready HEAD WORKFLOWS [--again]` in a checkout of main at HEAD with its tags: the newest commit main has pointed at
/// that is newer than every deploy (`deploy-<sha12>` tags) and whose parts are green; `--again` (the schedule's
/// re-apply) lets the deployed commit itself count
fn ready(head: &str, workflows: &str, again: bool) -> Result<()> {
    let repo = env("GITHUB_REPOSITORY")?;
    let ws: Vec<String> = workflows
        .split(',')
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty())
        .collect();
    // what runs: the deploy tags in HEAD's history; the commits newer than all of them (and the newest of them, again)
    let deployed: Vec<String> = git(&["tag", "-l", "deploy-*", "--merged", head])?
        .lines()
        .map(|t| git(&["rev-parse", &format!("{t}^{{commit}}")]))
        .collect::<Result<_>>()?;
    let not: Vec<String> = deployed.iter().map(|d| format!("^{d}")).collect();
    let mut range = vec!["rev-list", head];
    range.extend(not.iter().map(String::as_str));
    let mut newer: std::collections::HashSet<String> = git(&range)?.lines().map(String::from).collect();
    if again && !deployed.is_empty() {
        let mut tips = vec!["merge-base", "--independent"];
        tips.extend(deployed.iter().map(String::as_str));
        newer.extend(git(&tips)?.lines().map(String::from));
    }
    // the commits main has pointed at: those with main's own runs, newest push first
    let mut first_run: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for page in 1..=5 {
        let v = gh(&format!(
            "/repos/{repo}/actions/runs?branch=main&per_page=100&page={page}"
        ))?;
        let list = runs_of(&v);
        let n = list.len();
        for (sha, r) in list.into_iter().filter(|(_, r)| r.on_main) {
            if newer.contains(&sha) {
                let e = first_run.entry(sha).or_insert(r.id);
                *e = (*e).min(r.id);
            }
        }
        if n < 100 {
            break;
        }
    }
    let mut shas: Vec<(String, u64)> = first_run.into_iter().collect();
    shas.sort_by_key(|s| std::cmp::Reverse(s.1));
    let mut commits = Vec::new();
    for (sha, _) in shas.into_iter().take(30) {
        let mut runs = Vec::new();
        for page in 1..=10 {
            let list = runs_of(&gh(&format!(
                "/repos/{repo}/actions/runs?head_sha={sha}&per_page=100&page={page}"
            ))?);
            let n = list.len();
            runs.extend(list.into_iter().map(|(_, r)| r));
            if n < 100 {
                break;
            }
        }
        commits.push(decide::Commit { sha, runs });
    }
    match decide::pick(&ws, &commits) {
        decide::Pick::Go(sha) => println!("go {sha}"),
        decide::Pick::Nothing(None) => println!("nothing newer than what runs"),
        decide::Pick::Nothing(Some((sha, decide::Ready::Fail(f)))) => {
            bail!("red, and nothing older is ready: {sha} {}", f.join("; "))
        }
        decide::Pick::Nothing(Some((sha, r))) => println!("wait: the newest, {sha}, is {r:?}"),
    }
    Ok(())
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["tags", config, rest @ ..] => {
            let rev = rest.first().copied().unwrap_or("HEAD");
            let cfg: tags::Config = serde_json::from_str(&std::fs::read_to_string(config)?)?;
            let dir = std::path::Path::new(".");
            let t = tags::tags(&cfg, |p| tags::git_last(dir, rev, p))?;
            println!("{}", serde_json::to_string_pretty(&t)?);
        }
        ["ready", head, workflows] => ready(head, workflows, false)?,
        ["ready", head, workflows, "--again"] => ready(head, workflows, true)?,
        ["digests", tags] => {
            let mut out = BTreeMap::new();
            let mut missing = Vec::new();
            for (image, tag) in read_tags(tags)? {
                match registry::digest(&image, &tag)? {
                    Some(d) => {
                        out.insert(image, serde_json::json!({"tag": tag, "digest": d}));
                    }
                    None => missing.push(format!("ghcr.io/de0ch/{image}:{tag}")),
                }
            }
            if !missing.is_empty() {
                bail!("not built: {}", missing.join(", "));
            }
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        ["set", kustomization, tags] => {
            let text = std::fs::read_to_string(kustomization)?;
            std::fs::write(kustomization, kustomize::set_tags(&text, &read_tags(tags)?)?)?;
        }
        ["fabric", dir, tags, digests] => {
            let tag_of = read_tags(tags)?;
            let digest_of: BTreeMap<String, String> =
                serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&std::fs::read_to_string(digests)?)?
                    .into_iter()
                    .filter_map(|(k, v)| v["digest"].as_str().map(|d| (k, d.to_string())))
                    .collect();
            let dir = std::path::Path::new(dir);
            let kust = dir.join("kustomization.yaml");
            std::fs::write(
                &kust,
                kustomize::set_images(&std::fs::read_to_string(&kust)?, &tag_of, &digest_of)?,
            )?;
            let Some(aviary) = tag_of.get("sandboxd") else {
                bail!("no tag for sandboxd")
            };
            let h = dir.join("hermit.json");
            std::fs::write(
                &h,
                yyc::set_string(&std::fs::read_to_string(&h)?, "sandboxd", &format!("aviary-{aviary}"))?,
            )?;
        }
        ["yyc", dir, tags] => {
            let t = read_tags(tags)?;
            let dir = std::path::Path::new(dir);
            let k = dir.join("kustomization.yaml");
            std::fs::write(&k, kustomize::set_tags(&std::fs::read_to_string(&k)?, &t)?)?;
            let Some(aviary) = t.get("sandboxd") else {
                bail!("no tag for sandboxd")
            };
            let Some(digest) = registry::digest("hermit-sandbox", aviary)? else {
                bail!("not built: ghcr.io/de0ch/hermit-sandbox:{aviary}")
            };
            let h = dir.join("hermit/hermit.json");
            std::fs::write(&h, yyc::hermit_json(&std::fs::read_to_string(&h)?, aviary, &digest)?)?;
            std::fs::write(dir.join("hermit/gateway.release"), format!("aviary-{aviary}\n"))?;
        }
        _ => bail!(
            "usage: aviary-deploy tags CONFIG [REV] | ready HEAD WORKFLOWS [--again] | digests TAGS | set KUSTOMIZATION TAGS | yyc DIR TAGS"
        ),
    }
    Ok(())
}
