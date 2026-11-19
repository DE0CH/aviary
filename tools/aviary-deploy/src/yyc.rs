//! YYC AI's deployment values that follow the tags (yyc/deploy): hermit.json's `fabric.sandboxd` (the sandboxd
//! release, `aviary-<sha12>`) and `sandbox.image` (the sandbox image by digest), and hermit/gateway.release (the
//! gateway server's release). Each value is replaced in place, so the file's text is otherwise the same: hermit.json
//! feeds a hashed `ConfigMap`, and a reformatted file would roll hermit for nothing.

use anyhow::{Result, bail};

/// the JSON string value after `"key": "` (the first one), replaced by `value`
pub fn set_string(text: &str, key: &str, value: &str) -> Result<String> {
    let pat = format!("\"{key}\": \"");
    let Some(at) = text.find(&pat) else {
        bail!("no \"{key}\" string in the file")
    };
    let start = at + pat.len();
    let Some(len) = text[start..].find('"') else {
        bail!("\"{key}\" has no closing quote")
    };
    Ok(format!("{}{value}{}", &text[..start], &text[start + len..]))
}

/// hermit.json with the release and the sandbox image's digest
pub fn hermit_json(text: &str, aviary_tag: &str, sandbox_digest: &str) -> Result<String> {
    if !sandbox_digest.starts_with("sha256:") {
        bail!("not a digest: {sandbox_digest}");
    }
    let t = set_string(text, "sandboxd", &format!("aviary-{aviary_tag}"))?;
    let sandbox = t.find("\"sandbox\": {").unwrap_or(0);
    let (head, tail) = t.split_at(sandbox);
    Ok(format!(
        "{head}{}",
        set_string(tail, "image", &format!("ghcr.io/de0ch/hermit-sandbox@{sandbox_digest}"))?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERMIT: &str = r#"{
  "rules": "/etc/hermit/config/sandbox-rules.md",
  "sandbox": {
    "image": "ghcr.io/de0ch/hermit-sandbox@sha256:d6e3",
    "cpu": 2
  },
  "fabric": {
    "sandboxd": "aviary-786c854071e0",
    "runsc": "precopy-20261102.1"
  }
}
"#;

    #[test]
    fn the_release_and_the_digest_change_and_nothing_else() {
        let got = hermit_json(HERMIT, "aaaaaaaaaaaa", "sha256:ffff").unwrap();
        assert_eq!(
            got,
            HERMIT
                .replace("aviary-786c854071e0", "aviary-aaaaaaaaaaaa")
                .replace("sha256:d6e3", "sha256:ffff")
        );
    }

    #[test]
    fn a_file_without_the_keys_is_an_error() {
        assert!(hermit_json("{}", "a", "sha256:f").is_err());
        assert!(hermit_json(HERMIT, "a", "nope").is_err());
    }
}
