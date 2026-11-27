//! Moving the sandbox image's pins forward (images/sandbox/Dockerfile's `ARG`s): the newest version a repository
//! lists, a release's sha256 from its checksums file, and the Dockerfile with an `ARG`'s default replaced. Pure
//! functions: main.rs fetches and writes.

use anyhow::{Result, bail};

use crate::debver;

/// the newest version of `package` in an apt `Packages` file
pub fn newest(packages: &str, package: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut name = "";
    for line in packages.lines() {
        if let Some(n) = line.strip_prefix("Package: ") {
            name = n.trim();
        } else if let Some(v) = line.strip_prefix("Version: ")
            && name == package
        {
            let v = v.trim();
            if best.as_deref().is_none_or(|b| debver::compare(v, b).is_gt()) {
                best = Some(v.to_string());
            }
        }
    }
    best
}

/// the sha256 a `<sha256>  <file>` checksums list gives `file`
pub fn checksum(list: &str, file: &str) -> Option<String> {
    list.lines().find_map(|l| {
        let (sum, name) = l.split_once(char::is_whitespace)?;
        (name.trim() == file && sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit())).then(|| sum.to_string())
    })
}

/// the Dockerfile with `ARG name=<value>` (the one line declaring it) given `value`
pub fn set_arg(dockerfile: &str, name: &str, value: &str) -> Result<String> {
    let prefix = format!("ARG {name}=");
    let n = dockerfile.lines().filter(|l| l.starts_with(&prefix)).count();
    if n != 1 {
        bail!("the Dockerfile declares ARG {name}= {n} times, not once");
    }
    let mut out: Vec<String> = dockerfile
        .lines()
        .map(|l| {
            if l.starts_with(&prefix) {
                format!("{prefix}{value}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if dockerfile.ends_with('\n') {
        out.push(String::new());
    }
    Ok(out.join("\n"))
}

/// an `ARG`'s default as the Dockerfile has it
pub fn arg(dockerfile: &str, name: &str) -> Option<String> {
    let prefix = format!("ARG {name}=");
    dockerfile
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKAGES: &str = "Package: docker-ce\nVersion: 5:29.9.0-1~debian.13~trixie\n\nPackage: docker-ce-cli\nVersion: 5:30.0.0-1~debian.13~trixie\n\nPackage: docker-ce\nVersion: 5:29.10.1-1~debian.13~trixie\nArchitecture: amd64\n\nPackage: docker-ce\nVersion: 5:29.2.0-1~debian.13~trixie\n";

    #[test]
    fn the_newest_by_debians_order_not_the_last_listed() {
        assert_eq!(
            newest(PACKAGES, "docker-ce").as_deref(),
            Some("5:29.10.1-1~debian.13~trixie")
        );
        assert_eq!(
            newest(PACKAGES, "docker-ce-cli").as_deref(),
            Some("5:30.0.0-1~debian.13~trixie")
        );
        assert_eq!(newest(PACKAGES, "containerd.io"), None);
    }

    #[test]
    fn a_files_checksum() {
        let list = "aaaa  gh_1_linux_arm64.tar.gz\na476449fa9625413c54a3734eded62e9e480e49f421ff08e7f3b3879c059a033  gh_2.106.0_linux_amd64.tar.gz\n";
        assert_eq!(
            checksum(list, "gh_2.106.0_linux_amd64.tar.gz").as_deref(),
            Some("a476449fa9625413c54a3734eded62e9e480e49f421ff08e7f3b3879c059a033")
        );
        assert_eq!(checksum(list, "gh_1_linux_arm64.tar.gz"), None, "not 64 hex");
        assert_eq!(checksum(list, "nope"), None);
    }

    #[test]
    fn an_args_default_is_replaced_and_nothing_else() {
        let d = "FROM x\nARG GH_VERSION=2.1.0\nARG GH_SHA256=00\nRUN echo ${GH_VERSION}\n";
        let out = set_arg(d, "GH_VERSION", "2.106.0").unwrap();
        assert_eq!(
            out,
            "FROM x\nARG GH_VERSION=2.106.0\nARG GH_SHA256=00\nRUN echo ${GH_VERSION}\n"
        );
        assert_eq!(arg(&out, "GH_VERSION").as_deref(), Some("2.106.0"));
        assert!(set_arg(d, "NOPE", "1").is_err());
        assert!(set_arg("ARG A=1\nARG A=2\n", "A", "3").is_err());
    }

    #[test]
    fn the_sandbox_dockerfile_declares_every_pin_once() {
        let d = include_str!("../../../images/sandbox/Dockerfile");
        for a in [
            "DEBIAN_SNAPSHOT",
            "DOCKER_VERSION",
            "CONTAINERD_VERSION",
            "BUILDX_VERSION",
            "GH_VERSION",
            "GH_SHA256",
        ] {
            set_arg(d, a, "x").unwrap();
        }
    }
}
