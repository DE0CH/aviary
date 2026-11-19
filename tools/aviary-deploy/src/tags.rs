//! Each image's tag: the last commit that changed its part's inputs (deploy/images.json), as 12 hex characters. An
//! unchanged part keeps its tag, so its pods are not rolled, and the tag names the source the image was built from.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// deploy/images.json: part → the images built from it and the paths they are built from
#[derive(Debug, Deserialize)]
pub struct Part {
    pub images: Vec<String>,
    pub paths: Vec<String>,
}

pub type Config = BTreeMap<String, Part>;

/// `last`: the last commit (full hex) that changed any of the paths; image → tag
pub fn tags(config: &Config, last: impl Fn(&[String]) -> Result<String>) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (name, part) in config {
        let sha = last(&part.paths).with_context(|| format!("part {name}"))?;
        if sha.len() < 12 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("part {name}: no commit changed {:?}", part.paths);
        }
        for image in &part.images {
            if out.insert(image.clone(), sha[..12].to_string()).is_some() {
                bail!("image {image} is in two parts");
            }
        }
    }
    Ok(out)
}

/// `git -C DIR log -1 --format=%H REV -- PATHS`
pub fn git_last(dir: &std::path::Path, rev: &str, paths: &[String]) -> Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "-1", "--format=%H", rev, "--"])
        .args(paths)
        .output()
        .context("git log")?;
    if !out.status.success() {
        bail!("git log: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        serde_json::from_str(
            r#"{"aviary": {"images": ["pigeon", "magpie"], "paths": ["crates"]},
                "pigeon-app": {"images": ["pigeon-app"], "paths": ["app"]}}"#,
        )
        .unwrap()
    }

    #[test]
    fn each_image_takes_its_parts_last_commit() {
        let t = tags(&config(), |p| {
            Ok(if p[0] == "app" { "b".repeat(40) } else { "a".repeat(40) })
        })
        .unwrap();
        assert_eq!(t["pigeon"], "a".repeat(12));
        assert_eq!(t["magpie"], "a".repeat(12));
        assert_eq!(t["pigeon-app"], "b".repeat(12));
    }

    #[test]
    fn a_part_no_commit_touched_is_an_error() {
        assert!(tags(&config(), |_| Ok(String::new())).is_err());
    }

    #[test]
    fn an_image_in_two_parts_is_an_error() {
        let c: Config =
            serde_json::from_str(r#"{"a": {"images": ["x"], "paths": ["a"]}, "b": {"images": ["x"], "paths": ["b"]}}"#)
                .unwrap();
        assert!(tags(&c, |_| Ok("c".repeat(40))).is_err());
    }

    #[test]
    fn git_last_names_the_commit_that_touched_the_path() {
        let d = std::env::temp_dir().join(format!("aviary-deploy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("app")).unwrap();
        let git = |args: &[&str]| {
            let s = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
                .arg(&d)
                .args(args)
                .output()
                .unwrap();
            assert!(s.status.success(), "{}", String::from_utf8_lossy(&s.stderr));
            String::from_utf8_lossy(&s.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(d.join("app/x"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "app"]);
        let app = git(&["rev-parse", "HEAD"]);
        std::fs::write(d.join("other"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "other"]);
        assert_eq!(git_last(&d, "HEAD", &["app".into()]).unwrap(), app);
        let _ = std::fs::remove_dir_all(&d);
    }
}
