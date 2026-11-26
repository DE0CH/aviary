//! `ios-build-number number | record BUNDLE… | check BUNDLE…` (the crate's lib.rs says the rule).
//!
//! - `number`: this commit's build number, `<commits>.<GITHUB_RUN_ATTEMPT>` (app-ios.yml's `CFBundleVersion`).
//! - `record`: App Store Connect's highest build of each app (`ASC_KEY_ID`, `ASC_ISSUER_ID`, `ASC_PRIVATE_KEY`: the
//!   main-only `release` environment's key), written into the body of the GitHub release `ios-builds` (`GH_TOKEN`,
//!   `GITHUB_REPOSITORY`; one PATCH, so a reader never sees half a record). ios-builds.yml, on main only.
//! - `check`: fails when this commit's build number (attempt 1) is not above an app's recorded highest. Every branch
//!   (app.yml), so a number App Store Connect would refuse is red before the merge, not at main's upload.
//!
//! Every network call is tried again while it fails on its own (a connection error, HTTP 5xx or 429).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use ios_build_number::{BuildNumber, build_number, highest, parse_record, record_body, refusal, token};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const TRIES: u32 = 6;
const PAUSE: Duration = Duration::from_secs(3);
const PAUSE_MAX: Duration = Duration::from_secs(30);
const ASC: &str = "https://api.appstoreconnect.apple.com";
const GITHUB: &str = "https://api.github.com";
const TAG: &str = "ios-builds";

#[derive(Parser)]
#[command(about = "The iOS apps' build number and the check against App Store Connect's builds")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// This commit's build number.
    Number,
    /// Write App Store Connect's highest build of each app into the `ios-builds` release.
    Record { bundles: Vec<String> },
    /// Fail when this commit's build number is not above each app's recorded highest.
    Check { bundles: Vec<String> },
}

struct Http(reqwest::Client);

impl Http {
    /// `method url`, with `bearer` and a JSON `body`, tried again while it fails on its own; the first other answer.
    async fn call(
        &self,
        method: Method,
        url: &str,
        bearer: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        let mut pause = PAUSE;
        for tries in 1..=TRIES {
            let mut req = self
                .0
                .request(method.clone(), url)
                .timeout(Duration::from_secs(120))
                .header("accept", "application/json");
            if let Some(b) = bearer {
                req = req.bearer_auth(b);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let why = match req.send().await {
                Ok(res) if res.status().is_server_error() || res.status() == StatusCode::TOO_MANY_REQUESTS => {
                    format!("HTTP {}", res.status())
                }
                Ok(res) => {
                    let status = res.status();
                    match res.text().await {
                        Ok(text) => return Ok((status, serde_json::from_str(&text).unwrap_or(Value::String(text)))),
                        Err(e) => format!("reading the answer: {e:#}"),
                    }
                }
                Err(e) => format!("{e:#}"),
            };
            if tries == TRIES {
                bail!("{method} {url}: {why} ({TRIES} tries)");
            }
            eprintln!("ios-build-number: {method} {url}: {why} (try {tries} of {TRIES}, again in {pause:?})");
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(PAUSE_MAX);
        }
        bail!("{method} {url}: no tries")
    }

    /// A 2xx answer's JSON; any other answer is an error with what it said.
    async fn ok(&self, method: Method, url: &str, bearer: Option<&str>, body: Option<&Value>) -> Result<Value> {
        let (status, v) = self.call(method.clone(), url, bearer, body).await?;
        if !status.is_success() {
            bail!("{method} {url}: HTTP {status}: {v}");
        }
        Ok(v)
    }
}

/// A variable of the CI job's environment (the workflow's `env`, not a service's configuration).
#[allow(clippy::disallowed_methods)] // CI's own environment, as GitHub sets it (platform 4.5 is for services)
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// This commit's number of commits; a shallow clone's count is not main's, so it is refused.
fn commits() -> Result<u64> {
    let git = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .args(args)
            .output()
            .with_context(|| format!("git {}", args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    if git(&["rev-parse", "--is-shallow-repository"])? != "false" {
        bail!("a shallow clone counts too few commits: check out with fetch-depth: 0");
    }
    git(&["rev-list", "--count", "HEAD"])?
        .parse()
        .context("git rev-list --count HEAD")
}

fn attempt() -> Result<u64> {
    env("GITHUB_RUN_ATTEMPT").map_or(Ok(1), |a| a.parse().context("GITHUB_RUN_ATTEMPT"))
}

fn repo() -> String {
    env("GITHUB_REPOSITORY").unwrap_or_else(|| "DE0CH/aviary".into())
}

/// The highest build number App Store Connect holds of the app `bundle` (`None`: it has none).
async fn asc_highest(http: &Http, jwt: &str, bundle: &str) -> Result<Option<BuildNumber>> {
    let apps = http
        .ok(
            Method::GET,
            &format!("{ASC}/v1/apps?filter[bundleId]={bundle}&fields[apps]=bundleId&limit=200"),
            Some(jwt),
            None,
        )
        .await?;
    let id = apps["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["attributes"]["bundleId"] == bundle)
        .and_then(|a| a["id"].as_str())
        .with_context(|| format!("App Store Connect has no app {bundle}"))?
        .to_string();
    let mut versions = Vec::new();
    let mut next = Some(format!(
        "{ASC}/v1/builds?filter[app]={id}&fields[builds]=version&limit=200"
    ));
    while let Some(url) = next {
        let page = http.ok(Method::GET, &url, Some(jwt), None).await?;
        for b in page["data"].as_array().into_iter().flatten() {
            if let Some(v) = b["attributes"]["version"].as_str() {
                versions.push(v.to_string());
            }
        }
        next = page["links"]["next"].as_str().map(str::to_string);
    }
    Ok(highest(versions.iter().map(String::as_str)))
}

async fn record(http: &Http, bundles: &[String]) -> Result<String> {
    let key_id = env("ASC_KEY_ID").context("ASC_KEY_ID")?;
    let issuer = env("ASC_ISSUER_ID").context("ASC_ISSUER_ID")?;
    let pem = env("ASC_PRIVATE_KEY").context("ASC_PRIVATE_KEY")?;
    let gh = env("GH_TOKEN").context("GH_TOKEN")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let jwt = token(&key_id, &issuer, now, &pem, &ring::rand::SystemRandom::new())?;
    let mut held = BTreeMap::new();
    for bundle in bundles {
        held.insert(
            bundle.clone(),
            asc_highest(http, &jwt, bundle).await?.map(|b| b.to_string()),
        );
    }
    let body = record_body(&held);
    let release = format!("{GITHUB}/repos/{}/releases/tags/{TAG}", repo());
    let (status, found) = http.call(Method::GET, &release, Some(&gh), None).await?;
    if status == StatusCode::NOT_FOUND {
        let new = json!({
            "tag_name": TAG,
            "target_commitish": env("GITHUB_SHA").unwrap_or_else(|| "main".into()),
            "name": "iOS build numbers",
            "body": body,
            "prerelease": true,
            "make_latest": "false",
        });
        http.ok(
            Method::POST,
            &format!("{GITHUB}/repos/{}/releases", repo()),
            Some(&gh),
            Some(&new),
        )
        .await?;
    } else if status.is_success() {
        let id = found["id"].as_u64().context("the ios-builds release's id")?;
        let url = format!("{GITHUB}/repos/{}/releases/{id}", repo());
        http.ok(Method::PATCH, &url, Some(&gh), Some(&json!({ "body": body })))
            .await?;
    } else {
        bail!("GET {release}: HTTP {status}: {found}");
    }
    Ok(format!("{held:?}"))
}

async fn check(http: &Http, bundles: &[String]) -> Result<String> {
    let ours = build_number(commits()?, 1);
    let url = format!("{GITHUB}/repos/{}/releases/tags/{TAG}", repo());
    let (status, release) = http.call(Method::GET, &url, env("GH_TOKEN").as_deref(), None).await?;
    if status == StatusCode::NOT_FOUND {
        bail!("no record of App Store Connect's builds yet (the release {TAG}): ios-builds.yml makes it on main");
    }
    if !status.is_success() {
        bail!("GET {url}: HTTP {status}: {release}");
    }
    let held = parse_record(release["body"].as_str().unwrap_or_default())?;
    let mut refused = Vec::new();
    for bundle in bundles {
        let Some(recorded) = held.get(bundle) else {
            bail!("the ios-builds record has no {bundle}: add it to ios-builds.yml's apps");
        };
        let highest = match recorded {
            Some(v) => Some(BuildNumber::parse(v).with_context(|| format!("{bundle}'s recorded build {v}"))?),
            None => None,
        };
        if let Some(why) = refusal(bundle, &ours, highest.as_ref()) {
            refused.push(why);
        }
    }
    if !refused.is_empty() {
        bail!("{}", refused.join("\n"));
    }
    Ok(format!(
        "build {ours} is above every recorded build of {}",
        bundles.join(", ")
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let http = Http(
        reqwest::Client::builder()
            .tls_backend_preconfigured(aviary_core::http::tls_config())
            .user_agent("aviary-ios-build-number")
            .connect_timeout(Duration::from_secs(30))
            .build()?,
    );
    let said = match cli.command {
        Cmd::Number => build_number(commits()?, attempt()?).to_string(),
        Cmd::Record { bundles } => record(&http, &bundles).await?,
        Cmd::Check { bundles } => check(&http, &bundles).await?,
    };
    writeln!(std::io::stdout(), "{said}")?;
    Ok(())
}
