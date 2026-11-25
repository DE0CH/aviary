//! ci.yml `publish` (platform 5.4): the GitHub release `aviary-<sha12>` of a main commit whose images were pushed.
//!
//! Assets: pigeon, hermit, supervisor, supervisor-keeper, sandboxd and aviary-conformance from the dist directory,
//! pigeon's frame corpus (`pigeon-frames.ndjson`) and the contract harness (`paseo-contract.tgz`, testing/paseo-contract
//! without `node_modules`) the app's CI validates against, and `SHA256SUMS` over them. The body is the commit subject,
//! the image digests and whether the sandbox image's supervisor (label `dev.de0ch.aviary.supervisor`) changed against
//! the previous release. Not "latest", not a prerelease.
//!
//! Every GitHub call is tried again when it fails on its own (no answer, 5xx, 429): a network request is the one
//! thing in CI that may fail by itself. A re-run finishes what an earlier one began: a release that exists keeps its
//! body and gets the assets it lacks (one whose upload broke off is uploaded again).
//!
//!   ci-release <dist dir> <40 hex commit> <digests.json> <work dir>
//!
//! On the tag the step before pushed, with no target commit: a `GITHUB_TOKEN` may not name a commit whose workflow
//! files differ from main's ("HTTP 403: Resource not accessible by integration"), which is every commit published
//! after a later one changed .github/workflows (aviary-20bfea738955, 2026-11-17).

use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// The binaries a release carries, from the dist directory.
const BINARIES: [&str; 6] = [
    "pigeon",
    "hermit",
    "supervisor",
    "supervisor-keeper",
    "sandboxd",
    "aviary-conformance",
];
/// Tries of one GitHub call, and the first pause between two (doubled after each, at most [`PAUSE_MAX`]).
const TRIES: u32 = 6;
const PAUSE: Duration = Duration::from_secs(3);
const PAUSE_MAX: Duration = Duration::from_secs(30);
const API: &str = "https://api.github.com";
const UPLOADS: &str = "https://uploads.github.com";

#[derive(Parser)]
#[command(about = "Make the GitHub release aviary-<sha12> of a published commit")]
struct Cli {
    dist: PathBuf,
    sha: String,
    digests: PathBuf,
    work: PathBuf,
}

struct Github {
    http: reqwest::Client,
    token: String,
    repo: String,
}

/// What one try of a call came to.
enum Answer {
    Done(StatusCode, Value),
    /// Failed on its own: tried again.
    Again(String),
}

impl Github {
    /// `method url` with `body`, tried again while it fails on its own; the status and JSON of the first answer that
    /// is not such a failure.
    async fn call(&self, method: Method, url: &str, body: Option<Body>) -> Result<(StatusCode, Value)> {
        let mut pause = PAUSE;
        let mut tries = 0;
        loop {
            tries += 1;
            let answer = self.once(method.clone(), url, body.as_ref()).await;
            match answer {
                Answer::Done(status, v) => return Ok((status, v)),
                Answer::Again(why) if tries < TRIES => {
                    eprintln!("ci-release: {method} {url}: {why} (try {tries} of {TRIES}, again in {pause:?})");
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(PAUSE_MAX);
                }
                Answer::Again(why) => bail!("{method} {url}: {why} ({TRIES} tries)"),
            }
        }
    }

    async fn once(&self, method: Method, url: &str, body: Option<&Body>) -> Answer {
        let mut req = self
            .http
            .request(method, url)
            .bearer_auth(&self.token)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .timeout(Duration::from_secs(300));
        match body {
            Some(Body::Json(v)) => req = req.json(v),
            Some(Body::File(bytes)) => {
                req = req
                    .header("content-type", "application/octet-stream")
                    .body(bytes.clone());
            }
            None => {}
        }
        let res = match req.send().await {
            Ok(r) => r,
            Err(e) => return Answer::Again(format!("{e:#}")),
        };
        let status = res.status();
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Answer::Again(format!("HTTP {status}"));
        }
        let text = match res.text().await {
            Ok(t) => t,
            Err(e) => return Answer::Again(format!("reading the answer: {e:#}")),
        };
        Answer::Done(status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn api(&self, path: &str) -> String {
        format!("{API}/repos/{}{path}", self.repo)
    }
}

enum Body {
    Json(Value),
    File(Vec<u8>),
}

fn ok(what: &str, (status, v): &(StatusCode, Value)) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        bail!("{what}: HTTP {status}: {v}")
    }
}

/// `program args…`'s standard output, which must succeed.
fn output(program: &str, args: &[&str]) -> Result<String> {
    let o = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !o.status.success() {
        bail!(
            "{program} {}: {}: {}",
            args.join(" "),
            o.status,
            String::from_utf8_lossy(&o.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_owned())
}

/// The release's assets in `dir`, in their order (`SHA256SUMS` last).
fn assets(dist: &Path, sha: &str, dir: &Path) -> Result<Vec<String>> {
    std::fs::create_dir_all(dir)?;
    let mut names = Vec::new();
    for b in BINARIES {
        let to = dir.join(b);
        std::fs::copy(dist.join(b), &to).with_context(|| format!("{b} from {}", dist.display()))?;
        std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755))?;
        names.push(b.to_owned());
    }
    std::fs::copy(
        "testing/paseo-contract/fixtures/pigeon-frames.ndjson",
        dir.join("pigeon-frames.ndjson"),
    )
    .context("the frame corpus")?;
    names.push("pigeon-frames.ndjson".into());
    let mtime = format!("@{}", output("git", &["log", "-1", "--format=%ct", sha])?);
    let tgz = dir.join("paseo-contract.tgz");
    output(
        "tar",
        &[
            "--sort=name",
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            &format!("--mtime={mtime}"),
            "--exclude=node_modules",
            "-czf",
            &tgz.to_string_lossy(),
            "-C",
            "testing",
            "paseo-contract",
        ],
    )?;
    names.push("paseo-contract.tgz".into());
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let sums = Command::new("sha256sum")
        .args(&refs)
        .current_dir(dir)
        .output()
        .context("sha256sum")?;
    if !sums.status.success() {
        bail!("sha256sum: {}", sums.status);
    }
    std::fs::write(dir.join("SHA256SUMS"), sums.stdout)?;
    names.push("SHA256SUMS".into());
    Ok(names)
}

/// digests.json's entries in the file's order.
fn digests(path: &Path) -> Result<Vec<(String, String)>> {
    struct Ordered(Vec<(String, String)>);
    impl<'de> serde::Deserialize<'de> for Ordered {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct V;
            impl<'de> serde::de::Visitor<'de> for V {
                type Value = Ordered;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("an object of image digests")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> std::result::Result<Ordered, A::Error> {
                    let mut out = Vec::new();
                    while let Some((k, v)) = m.next_entry::<String, String>()? {
                        out.push((k, v));
                    }
                    Ok(Ordered(out))
                }
            }
            d.deserialize_map(V)
        }
    }
    let o: Ordered = serde_json::from_slice(&std::fs::read(path)?).context("digests.json")?;
    Ok(o.0)
}

/// The supervisor a release's body names (`supervisor: <64 hex>`).
fn supervisor_of(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("supervisor: "))
        .map(|rest| rest.chars().take_while(char::is_ascii_hexdigit).collect::<String>())
        .filter(|h| h.len() == 64)
}

async fn body(gh: &Github, tag: &str, sha: &str, digests_path: &Path) -> Result<String> {
    let short = &sha[..12];
    let supervisor = output(
        "docker",
        &[
            "image",
            "inspect",
            "-f",
            "{{index .Config.Labels \"dev.de0ch.aviary.supervisor\"}}",
            &format!("hermit-sandbox:{short}"),
        ],
    )?;
    let list = gh.call(Method::GET, &gh.api("/releases?per_page=50"), None).await?;
    ok("the releases", &list)?;
    let mut earlier: Vec<&Value> = list
        .1
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| {
            r["tag_name"]
                .as_str()
                .is_some_and(|t| t.starts_with("aviary-") && t != tag)
        })
        .collect();
    earlier.sort_by_key(|r| r["created_at"].as_str().unwrap_or("").to_owned());
    let changed = match earlier.last() {
        None => "no earlier release".to_owned(),
        Some(prev) => {
            let name = prev["tag_name"].as_str().unwrap_or("");
            match supervisor_of(prev["body"].as_str().unwrap_or("")) {
                None => format!("{name} names no supervisor"),
                Some(before) if before == supervisor => format!("unchanged against {name}"),
                Some(before) => format!("changed against {name} ({before})"),
            }
        }
    };
    let mut b = output("git", &["log", "-1", "--format=%s", sha])?;
    b.push_str("\n\n");
    let _ = writeln!(
        b,
        "Images (ghcr.io/de0ch/<name>:{short}; pigeon, magpie, hermit, herald, sandboxd and aviary-testing are \
         linux/amd64 + linux/arm64 indexes, hermit-sandbox is linux/amd64):"
    );
    b.push('\n');
    for (name, digest) in digests(digests_path)? {
        let _ = writeln!(b, "- {name}@{digest}");
    }
    b.push('\n');
    let _ = writeln!(b, "supervisor: {supervisor} ({changed})");
    Ok(b)
}

/// A variable of the CI job's environment (the workflow's `env`, not a service's configuration).
#[allow(clippy::disallowed_methods)] // CI's own environment, as GitHub sets it (platform 4.5 is for services)
fn job_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.sha.len() != 40 || !cli.sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("not a 40 hex commit: {}", cli.sha);
    }
    let token = job_env("GH_TOKEN").context("GH_TOKEN")?;
    let repo = job_env("GITHUB_REPOSITORY").unwrap_or_else(|| "DE0CH/aviary".into());
    let http = reqwest::Client::builder()
        .tls_backend_preconfigured(aviary_core::http::tls_config())
        .user_agent("aviary-ci-release")
        .connect_timeout(Duration::from_secs(30))
        .build()?;
    let gh = Github { http, token, repo };
    let tag = format!("aviary-{}", &cli.sha[..12]);

    let found = gh
        .call(Method::GET, &gh.api(&format!("/releases/tags/{tag}")), None)
        .await?;
    let (release, body) = match found.0 {
        StatusCode::OK => {
            eprintln!("ci-release: release {tag} exists: its missing assets are uploaded");
            let b = found.1["body"].as_str().unwrap_or("").to_owned();
            (found.1, b)
        }
        StatusCode::NOT_FOUND => {
            let r = gh
                .call(Method::GET, &gh.api(&format!("/git/ref/tags/{tag}")), None)
                .await?;
            ok(&format!("the tag {tag}"), &r)?;
            let body = body(&gh, &tag, &cli.sha, &cli.digests).await?;
            let create = json!({"tag_name": tag, "name": tag, "body": body, "draft": false,
                                "prerelease": false, "make_latest": "false"});
            let made = gh
                .call(Method::POST, &gh.api("/releases"), Some(Body::Json(create)))
                .await?;
            let release = if made.0 == StatusCode::UNPROCESSABLE_ENTITY {
                // a try that GitHub took but whose answer was lost: the release is there
                let again = gh
                    .call(Method::GET, &gh.api(&format!("/releases/tags/{tag}")), None)
                    .await?;
                ok(
                    &format!("the release {tag} after a refused create ({})", made.1),
                    &again,
                )?;
                again.1
            } else {
                ok("creating the release", &made)?;
                made.1
            };
            (release, body)
        }
        other => bail!("the release {tag}: HTTP {other}: {}", found.1),
    };
    let id = release["id"].as_u64().context("the release's id")?;
    let names = assets(&cli.dist, &cli.sha, &cli.work.join("assets"))?;
    let mut have: Vec<(String, u64, String)> = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some((
                a["name"].as_str()?.to_owned(),
                a["id"].as_u64()?,
                a["state"].as_str().unwrap_or("").to_owned(),
            ))
        })
        .collect();
    for name in &names {
        if let Some(i) = have.iter().position(|(n, _, _)| n == name) {
            let (_, asset, state) = have.remove(i);
            if state == "uploaded" {
                continue;
            }
            // an upload that broke off: removed, then sent whole
            let gone = gh
                .call(Method::DELETE, &gh.api(&format!("/releases/assets/{asset}")), None)
                .await?;
            if gone.0 != StatusCode::NOT_FOUND {
                ok(&format!("removing the broken {name}"), &gone)?;
            }
        }
        let bytes = std::fs::read(cli.work.join("assets").join(name))?;
        let url = format!("{UPLOADS}/repos/{}/releases/{id}/assets?name={name}", gh.repo);
        let up = gh.call(Method::POST, &url, Some(Body::File(bytes))).await?;
        if up.0 == StatusCode::UNPROCESSABLE_ENTITY {
            // a try GitHub took but whose answer was lost
            let r = gh.call(Method::GET, &gh.api(&format!("/releases/{id}")), None).await?;
            ok("the release", &r)?;
            let there = r.1["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|a| a["name"] == name.as_str() && a["state"] == "uploaded");
            if !there {
                bail!("uploading {name}: HTTP 422: {}", up.1);
            }
        } else {
            ok(&format!("uploading {name}"), &up)?;
        }
        eprintln!("ci-release: {name} uploaded");
    }
    let mut out = std::io::stdout().lock();
    out.write_all(body.as_bytes())?;
    if let Some(summary) = job_env("GITHUB_STEP_SUMMARY") {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary)?
            .write_all(body.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::supervisor_of;

    #[test]
    fn the_supervisor_line_of_a_body() {
        let h = "e1d6d23f6b8026a93a11e028dfc6641b73173618dbc98f9960df91224c7235b6";
        assert_eq!(
            supervisor_of(&format!(
                "subject\n\n- pigeon@sha256:x\n\nsupervisor: {h} (unchanged against aviary-1)\n"
            )),
            Some(h.to_owned())
        );
        assert_eq!(supervisor_of("supervisor: short (x)"), None);
        assert_eq!(supervisor_of("no such line"), None);
    }
}
