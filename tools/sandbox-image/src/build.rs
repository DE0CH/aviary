//! `docker buildx build`'s arguments for the sandbox image: images/sandbox as the context, the supervisor binaries as
//! the named context `dist`, platform 5.3's labels as build arguments (images/sandbox/Dockerfile names them), and
//! the image's timestamps from the commit (platform 5.2: `SOURCE_DATE_EPOCH`, `rewrite-timestamp=true`).

/// what the labels say and where the build reads from
pub struct Build<'a> {
    /// images/sandbox
    pub dir: &'a str,
    /// the directory with supervisor and supervisor-keeper
    pub bin: &'a str,
    pub tag: &'a str,
    /// the 40 hex commit
    pub revision: &'a str,
    /// `supervisor --speaks`
    pub speaks: &'a str,
    /// the supervisor binary's sha256 (says whether a new sandbox image changed the supervisor)
    pub supervisor_sha256: &'a str,
    /// the commit's time, seconds since the epoch
    pub epoch: &'a str,
    /// extra flags (a cache, build arguments), as the caller gives them
    pub extra: &'a [String],
}

pub fn args(b: &Build) -> Vec<String> {
    let version = b.revision.get(..12).unwrap_or(b.revision);
    let mut a: Vec<String> = vec!["buildx".into(), "build".into()];
    a.extend(b.extra.iter().cloned());
    for (k, v) in [
        ("REVISION", b.revision),
        ("VERSION", version),
        ("SOURCE_DATE_EPOCH", b.epoch),
        ("SPEAKS", b.speaks),
        ("SUPERVISOR_SHA256", b.supervisor_sha256),
    ] {
        a.push("--build-arg".into());
        a.push(format!("{k}={v}"));
    }
    a.extend([
        "--build-context".into(),
        format!("dist={}", b.bin),
        "-f".into(),
        format!("{}/Dockerfile", b.dir),
        "--output".into(),
        "type=docker,rewrite-timestamp=true".into(),
        "-t".into(),
        b.tag.into(),
        b.dir.into(),
    ]);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_labels_the_context_and_the_binaries() {
        let extra = vec!["--cache-from".to_string(), "type=gha,scope=hermit-sandbox".to_string()];
        let a = args(&Build {
            dir: "images/sandbox",
            bin: "/tmp/dist",
            tag: "hermit-sandbox:0123456789ab",
            revision: "0123456789abcdef0123456789abcdef01234567",
            speaks: r#"{"magpie-sandbox":2}"#,
            supervisor_sha256: "ff",
            epoch: "1764115200",
            extra: &extra,
        });
        let s = a.join(" ");
        assert!(
            s.starts_with("buildx build --cache-from type=gha,scope=hermit-sandbox --build-arg"),
            "{s}"
        );
        for want in [
            "--build-arg REVISION=0123456789abcdef0123456789abcdef01234567",
            "--build-arg VERSION=0123456789ab",
            "--build-arg SOURCE_DATE_EPOCH=1764115200",
            r#"--build-arg SPEAKS={"magpie-sandbox":2}"#,
            "--build-arg SUPERVISOR_SHA256=ff",
            "--build-context dist=/tmp/dist",
            "-f images/sandbox/Dockerfile",
            "--output type=docker,rewrite-timestamp=true",
            "-t hermit-sandbox:0123456789ab",
        ] {
            assert!(s.contains(want), "{want} not in {s}");
        }
        assert_eq!(a.last().map(String::as_str), Some("images/sandbox"));
    }
}
