//! ci.yml `publish` (platform 5.4): the GitHub release `aviary-<sha12>` of a main commit whose images were pushed.
//! Assets: [`BINARIES`] from `dist/`, pigeon's frame corpus (`pigeon-frames.ndjson`) and the contract harness
//! (`paseo-contract.tgz`, `testing/paseo-contract` without `node_modules`) the app's CI validates against, and
//! `SHA256SUMS` over them. The body is the commit subject, the image digests and whether the sandbox image's supervisor
//! (label `dev.de0ch.aviary.supervisor`) changed against the previous release. Not "latest", not a prerelease. A
//! release that exists already (a re-run) is left alone. Runs `gh`, `git`, `tar`, `sha256sum` and `docker`.
//!
//!   `aviary-release <dist dir> <40 hex commit> <digests.json> <work dir>`   (`GITHUB_REPOSITORY`, `GH_TOKEN` as gh reads them)

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// The binaries a release carries: what a deployment fetches by release (sandboxd for its hosts, fabric-aws for its
/// AMI workflow), what the app's CI runs (pigeon, aviary-conformance), and the rest of the platform's (platform 5.4).
const BINARIES: [&str; 7] = [
    "pigeon",
    "hermit",
    "supervisor",
    "supervisor-keeper",
    "sandboxd",
    "aviary-conformance",
    "fabric-aws",
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "aviary-release: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn output(c: &mut Command, what: &str) -> Result<String> {
    let o = c
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running {what}"))?;
    if !o.status.success() {
        bail!("{what} failed ({})", o.status);
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_owned())
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dist, sha, digests, work] = args.as_slice() else {
        bail!("usage: aviary-release <dist dir> <40 hex commit> <digests.json> <work dir>");
    };
    if sha.len() != 40 || !sha.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("{sha} is not a 40-hex commit");
    }
    let sha12 = &sha[..12];
    let tag = format!("aviary-{sha12}");
    // GITHUB_REPOSITORY is gh's own default for -R when unset; naming it keeps a run outside Actions on aviary
    let repo = repository();
    if Command::new("gh")
        .args(["release", "view", &tag, "-R", &repo])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success()
    {
        writeln!(std::io::stdout(), "release {tag} exists: left alone")?;
        return Ok(());
    }
    let work = PathBuf::from(work);
    let assets = work.join("assets");
    std::fs::create_dir_all(&assets)?;
    for b in BINARIES {
        let from = Path::new(dist).join(b);
        let to = assets.join(b);
        std::fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
        std::fs::set_permissions(&to, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    }
    std::fs::copy(
        "testing/paseo-contract/fixtures/pigeon-frames.ndjson",
        assets.join("pigeon-frames.ndjson"),
    )?;
    let mtime = output(Command::new("git").args(["log", "-1", "--format=%ct", sha]), "git log")?;
    output(
        Command::new("tar")
            .args([
                "--sort=name",
                "--owner=0",
                "--group=0",
                "--numeric-owner",
                &format!("--mtime=@{mtime}"),
                "--exclude=node_modules",
                "-czf",
            ])
            .arg(assets.join("paseo-contract.tgz"))
            .args(["-C", "testing", "paseo-contract"]),
        "tar of the contract harness",
    )?;
    let mut summed: Vec<&str> = BINARIES.to_vec();
    summed.extend(["pigeon-frames.ndjson", "paseo-contract.tgz"]);
    let sums = output(
        Command::new("sha256sum").args(&summed).current_dir(&assets),
        "sha256sum",
    )?;
    std::fs::write(assets.join("SHA256SUMS"), format!("{sums}\n"))?;
    let supervisor = output(
        Command::new("docker").args([
            "image",
            "inspect",
            "-f",
            r#"{{index .Config.Labels "dev.de0ch.aviary.supervisor"}}"#,
            &format!("hermit-sandbox:{sha12}"),
        ]),
        "docker image inspect",
    )?;
    let list = output(
        Command::new("gh").args([
            "release",
            "list",
            "-R",
            &repo,
            "--limit",
            "50",
            "--json",
            "tagName,createdAt",
        ]),
        "gh release list",
    )?;
    let prev = previous_release(&serde_json::from_str(&list)?);
    let prev_body = match &prev {
        Some(p) => Some(output(
            Command::new("gh").args(["release", "view", p, "-R", &repo, "--json", "body", "--jq", ".body"]),
            "gh release view",
        )?),
        None => None,
    };
    let changed = supervisor_change(&supervisor, prev.as_deref(), prev_body.as_deref());
    let subject = output(Command::new("git").args(["log", "-1", "--format=%s", sha]), "git log")?;
    let digests: Value = serde_json::from_str(&std::fs::read_to_string(digests)?)?;
    let body_text = body(&subject, sha12, &digests, &supervisor, &changed);
    let body_file = work.join("body.md");
    std::fs::write(&body_file, &body_text)?;
    write!(std::io::stdout(), "{body_text}")?;
    // on the tag the step before pushed, with no target commit: a GITHUB_TOKEN may not name a commit whose workflow
    // files differ from main's ("HTTP 403: Resource not accessible by integration"), which is every commit published
    // after a later one changed .github/workflows (aviary-20bfea738955, 2026-11-17)
    let mut create = Command::new("gh");
    create
        .args([
            "release",
            "create",
            &tag,
            "-R",
            &repo,
            "--verify-tag",
            "--title",
            &tag,
            "--notes-file",
        ])
        .arg(&body_file)
        .arg("--latest=false");
    for a in summed.iter().chain(std::iter::once(&"SHA256SUMS")) {
        create.arg(assets.join(a));
    }
    output(&mut create, "gh release create")?;
    if let Some(summary) = step_summary() {
        let mut f = std::fs::OpenOptions::new().append(true).create(true).open(summary)?;
        f.write_all(body_text.as_bytes())?;
    }
    Ok(())
}

#[allow(clippy::disallowed_methods)] // a CI tool: the Actions environment is its configuration
fn repository() -> String {
    std::env::var("GITHUB_REPOSITORY").unwrap_or_else(|_| "DE0CH/aviary".to_owned())
}

#[allow(clippy::disallowed_methods)] // a CI tool: the Actions environment is its configuration
fn step_summary() -> Option<String> {
    std::env::var("GITHUB_STEP_SUMMARY").ok().filter(|s| !s.is_empty())
}

/// The newest `aviary-*` release by creation time (releases are not marked "latest", platform 5.4).
fn previous_release(list: &Value) -> Option<String> {
    let mut rels: Vec<(&str, &str)> = list
        .as_array()?
        .iter()
        .filter_map(|r| Some((r["tagName"].as_str()?, r["createdAt"].as_str()?)))
        .filter(|(t, _)| t.starts_with("aviary-"))
        .collect();
    rels.sort_by(|a, b| a.1.cmp(b.1));
    rels.last().map(|(t, _)| (*t).to_owned())
}

/// Whether the sandbox image's supervisor changed against the previous release, as its body says it (`supervisor:
/// <64 hex> …`).
fn supervisor_change(now: &str, prev: Option<&str>, prev_body: Option<&str>) -> String {
    let Some(prev) = prev else {
        return "no earlier release".to_owned();
    };
    let before = prev_body.unwrap_or_default().lines().find_map(|l| {
        let h = l.strip_prefix("supervisor: ")?.get(..64)?;
        h.bytes().all(|c| c.is_ascii_hexdigit()).then_some(h)
    });
    match before {
        None => format!("{prev} names no supervisor"),
        Some(b) if b == now => format!("unchanged against {prev}"),
        Some(b) => format!("changed against {prev} ({b})"),
    }
}

/// The release's body.
#[allow(clippy::items_after_statements)]
fn body(subject: &str, sha12: &str, digests: &Value, supervisor: &str, changed: &str) -> String {
    use std::fmt::Write as _;
    let mut s = format!(
        "{subject}\n\nImages (ghcr.io/de0ch/<name>:{sha12}; pigeon, magpie, hermit, herald, sandboxd and aviary-testing are \
         linux/amd64 + linux/arm64 indexes, hermit-sandbox is linux/amd64):\n\n"
    );
    if let Some(m) = digests.as_object() {
        for (k, v) in m {
            let _ = writeln!(s, "- {k}@{}", v.as_str().unwrap_or_default());
        }
    }
    let _ = writeln!(s, "\nsupervisor: {supervisor} ({changed})");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_release_carries_fabric_aws_and_sandboxd() {
        assert!(BINARIES.contains(&"fabric-aws") && BINARIES.contains(&"sandboxd"));
    }

    #[test]
    fn the_previous_release_is_the_newest_aviary_one() {
        let l = json!([{"tagName": "aviary-b", "createdAt": "2026-11-19T05:00:00Z"},
                       {"tagName": "other", "createdAt": "2026-11-19T09:00:00Z"},
                       {"tagName": "aviary-a", "createdAt": "2026-11-18T05:00:00Z"}]);
        assert_eq!(previous_release(&l).as_deref(), Some("aviary-b"));
        assert_eq!(previous_release(&json!([])), None);
    }

    #[test]
    fn the_supervisor_change_reads_the_previous_body() {
        let h = "a".repeat(64);
        let body = format!("x\n\nsupervisor: {h} (unchanged against aviary-z)\n");
        assert_eq!(
            supervisor_change(&h, Some("aviary-p"), Some(&body)),
            "unchanged against aviary-p"
        );
        assert_eq!(
            supervisor_change(&"b".repeat(64), Some("aviary-p"), Some(&body)),
            format!("changed against aviary-p ({h})")
        );
        assert_eq!(
            supervisor_change(&h, Some("aviary-p"), Some("no line")),
            "aviary-p names no supervisor"
        );
        assert_eq!(supervisor_change(&h, None, None), "no earlier release");
    }

    #[test]
    fn the_body_lists_the_digests_and_the_supervisor() {
        let b = body(
            "subj",
            "0123456789ab",
            &json!({"hermit": "sha256:1"}),
            "s",
            "no earlier release",
        );
        assert!(b.starts_with("subj\n\nImages (ghcr.io/de0ch/<name>:0123456789ab;"));
        assert!(b.contains("- hermit@sha256:1\n"));
        assert!(b.ends_with("\nsupervisor: s (no earlier release)\n"));
    }
}
