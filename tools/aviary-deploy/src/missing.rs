//! What a deploy must build first (docs/DEPLOY.md, Deyao 2026-11-26): every image of the commit it deploys is built
//! from that commit's source. GHCR is only a cache keyed by each part's last commit (tags.rs): a part whose image is
//! there is not built again; a part with any image absent (a build cancelled on main, an image deleted) is built by its
//! own workflow at its commit before the deploy goes on. The source is the only state a deploy depends on.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::tags::Config;

/// The parts to build (part → the full commit to build it at): those with an image, or a release, `present` says is
/// absent. `present(image, tag)` answers for one image at its tag (the commit's first 12 characters); the part
/// `release` names, if any, must also have `present("release:<part>", tag)`.
pub fn missing(
    config: &Config,
    commits: &BTreeMap<String, String>,
    release: Option<&str>,
    mut present: impl FnMut(&str, &str) -> Result<bool>,
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (part, spec) in config {
        let Some(sha) = commits.get(part) else {
            anyhow::bail!("no commit for part {part}");
        };
        let tag = sha.get(..12).unwrap_or(sha);
        let mut whole = true;
        for image in &spec.images {
            whole &= present(image, tag)?;
        }
        if release == Some(part.as_str()) {
            whole &= present(&format!("release:{part}"), tag)?;
        }
        if !whole {
            out.insert(part.clone(), sha.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn config() -> Config {
        serde_json::from_str(
            r#"{"aviary": {"images": ["pigeon", "magpie"], "paths": ["crates"]},
                "pigeon-app": {"images": ["pigeon-app"], "paths": ["app"]},
                "egress": {"images": ["egress"], "paths": ["egress"]}}"#,
        )
        .unwrap()
    }

    fn commits() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("aviary".into(), "a".repeat(40)),
            ("pigeon-app".into(), "b".repeat(40)),
            ("egress".into(), "c".repeat(40)),
        ])
    }

    fn with(have: &[&str]) -> impl FnMut(&str, &str) -> Result<bool> {
        let have: BTreeSet<String> = have.iter().map(|s| (*s).to_string()).collect();
        move |image, tag| Ok(have.contains(&format!("{image}:{tag}")))
    }

    #[test]
    fn a_part_with_every_image_is_not_built_again() {
        let all = [
            "pigeon:aaaaaaaaaaaa",
            "magpie:aaaaaaaaaaaa",
            "release:aviary:aaaaaaaaaaaa",
            "pigeon-app:bbbbbbbbbbbb",
            "egress:cccccccccccc",
        ];
        assert!(
            missing(&config(), &commits(), Some("aviary"), with(&all))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_part_with_one_image_absent_is_built_at_its_commit() {
        // main's app build on af83802f99 was cancelled: pigeon-app:af83802f9920 never existed (2026-11-26)
        let have = [
            "pigeon:aaaaaaaaaaaa",
            "magpie:aaaaaaaaaaaa",
            "release:aviary:aaaaaaaaaaaa",
            "egress:cccccccccccc",
        ];
        assert_eq!(
            missing(&config(), &commits(), Some("aviary"), with(&have)).unwrap(),
            BTreeMap::from([("pigeon-app".to_string(), "b".repeat(40))])
        );
        let have = [
            "pigeon:aaaaaaaaaaaa",
            "release:aviary:aaaaaaaaaaaa",
            "pigeon-app:bbbbbbbbbbbb",
            "egress:cccccccccccc",
        ];
        assert_eq!(
            missing(&config(), &commits(), Some("aviary"), with(&have)).unwrap(),
            BTreeMap::from([("aviary".to_string(), "a".repeat(40))])
        );
    }

    #[test]
    fn the_release_counts_as_one_of_its_parts_images() {
        let have = [
            "pigeon:aaaaaaaaaaaa",
            "magpie:aaaaaaaaaaaa",
            "pigeon-app:bbbbbbbbbbbb",
            "egress:cccccccccccc",
        ];
        assert_eq!(
            missing(&config(), &commits(), Some("aviary"), with(&have)).unwrap(),
            BTreeMap::from([("aviary".to_string(), "a".repeat(40))])
        );
    }

    #[test]
    fn nothing_there_builds_every_part() {
        assert_eq!(
            missing(&config(), &commits(), Some("aviary"), with(&[])).unwrap(),
            commits()
        );
    }

    #[test]
    fn a_part_without_its_commit_is_an_error() {
        let mut c = commits();
        c.remove("egress");
        assert!(missing(&config(), &c, None, with(&[])).is_err());
    }
}
