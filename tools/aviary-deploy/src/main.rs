//! aviary-deploy: the deploy run's decisions (docs/DEPLOY.md).
//!
//!   aviary-deploy tags deploy/images.json [REV]           image → tag (JSON)
//!   aviary-deploy ready SHA WORKFLOW,…                     go | wait | stale (stdout); red = exit 1
//!   aviary-deploy digests TAGS.json                         image → {tag, digest}; a missing image = exit 1
//!   aviary-deploy set KUSTOMIZATION TAGS.json               the tags written into its `images:`
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

fn ready(sha: &str, workflows: &str) -> Result<()> {
    let repo = env("GITHUB_REPOSITORY")?;
    let head = gh(&format!("/repos/{repo}/branches/main"))?["commit"]["sha"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut runs = Vec::new();
    for page in 1..=10 {
        let v = gh(&format!(
            "/repos/{repo}/actions/runs?head_sha={sha}&per_page=100&page={page}"
        ))?;
        let list = v["workflow_runs"].as_array().cloned().unwrap_or_default();
        let n = list.len();
        runs.extend(list.iter().map(|r| decide::Run {
            id: r["id"].as_u64().unwrap_or_default(),
            workflow: r["name"].as_str().unwrap_or_default().to_string(),
            status: r["status"].as_str().unwrap_or_default().to_string(),
            conclusion: r["conclusion"].as_str().map(String::from),
        }));
        if n < 100 {
            break;
        }
    }
    let ws: Vec<String> = workflows
        .split(',')
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty())
        .collect();
    match decide::ready(sha, &head, &ws, &runs) {
        decide::Ready::Go => println!("go"),
        decide::Ready::Stale { head } => println!("stale (main is at {head})"),
        decide::Ready::Wait(w) => println!("wait ({})", w.join(", ")),
        decide::Ready::Fail(f) => bail!("red: {}", f.join("; ")),
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
        ["ready", sha, workflows] => ready(sha, workflows)?,
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
            "usage: aviary-deploy tags CONFIG [REV] | ready SHA WORKFLOWS | digests TAGS | set KUSTOMIZATION TAGS | yyc DIR TAGS"
        ),
    }
    Ok(())
}
