//! The tags written into a kustomization's `images:` (flow style, one image a line:
//! `- {name: pigeon, newName: ghcr.io/de0ch/pigeon, newTag: "…"}`), and the digest too where an entry pins one
//! (`digest: sha256:…`, which kustomize prefers over the tag). Every entry of ours (`newName: ghcr.io/de0ch/…`) must
//! get a tag: a name the deploy run has no tag for is an error, never an old tag left standing.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

pub fn set_tags(text: &str, tags: &BTreeMap<String, String>) -> Result<String> {
    set_images(text, tags, &BTreeMap::new())
}

/// the tags, and each entry's `digest:` from `digests` (image → `sha256:…`; an entry with a digest needs one)
pub fn set_images(text: &str, tags: &BTreeMap<String, String>, digests: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&rewrite(line, tags, digests)?);
    }
    Ok(out)
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!("{key}:"))? + key.len() + 1;
    let rest = line[at..].trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().trim_matches('"'))
}

fn rewrite(line: &str, tags: &BTreeMap<String, String>, digests: &BTreeMap<String, String>) -> Result<String> {
    let t = line.trim_start();
    if !(t.starts_with("- {") && t.contains("newName:")) {
        return Ok(line.to_string());
    }
    let (Some(new_name), Some(old)) = (field(line, "newName"), field(line, "newTag")) else {
        return Ok(line.to_string());
    };
    let Some(image) = new_name.strip_prefix("ghcr.io/de0ch/") else {
        return Ok(line.to_string());
    };
    let Some(tag) = tags.get(image) else {
        bail!("no tag for {image} (deploy/images.json)")
    };
    let at = line.find("newTag:").map_or(0, |i| i + "newTag:".len());
    let rest = &line[at..];
    let start = rest.find(old).map_or(0, |i| at + i);
    let line = format!("{}{tag}{}", &line[..start], &line[start + old.len()..]);
    let Some(old_digest) = field(&line, "digest") else {
        return Ok(line);
    };
    let Some(digest) = digests.get(image) else {
        bail!("{image} pins a digest and the deploy run has none for it")
    };
    let at = line.find("digest:").map_or(0, |i| i + "digest:".len());
    let start = line[at..].find(old_digest).map_or(0, |i| at + i);
    Ok(format!(
        "{}{digest}{}",
        &line[..start],
        &line[start + old_digest.len()..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> BTreeMap<String, String> {
        [("pigeon", "aaaaaaaaaaaa"), ("pigeon-app", "bbbbbbbbbbbb")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into()
    }

    #[test]
    fn our_images_get_their_tags_and_the_rest_stays() {
        let text = "images:\n  - {name: pigeon, newName: ghcr.io/de0ch/pigeon, newTag: \"786c854071e0\"}\n  # app\n  - {name: pigeon-app, newName: ghcr.io/de0ch/pigeon-app, newTag: \"b663c206055a\"}\n  - {name: pg, newName: docker.io/postgres, newTag: \"18\"}\n";
        let want = "images:\n  - {name: pigeon, newName: ghcr.io/de0ch/pigeon, newTag: \"aaaaaaaaaaaa\"}\n  # app\n  - {name: pigeon-app, newName: ghcr.io/de0ch/pigeon-app, newTag: \"bbbbbbbbbbbb\"}\n  - {name: pg, newName: docker.io/postgres, newTag: \"18\"}\n";
        assert_eq!(set_tags(text, &tags()).unwrap(), want);
    }

    #[test]
    fn a_pinned_digest_follows_the_tag() {
        let text =
            "  - { name: hermit, newName: ghcr.io/de0ch/hermit, newTag: \"15f22fb9c7fd\", digest: sha256:b1da }\n";
        let tags = [("hermit", "aaaaaaaaaaaa")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into();
        let digests = [("hermit", "sha256:ffff")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into();
        assert_eq!(
            set_images(text, &tags, &digests).unwrap(),
            "  - { name: hermit, newName: ghcr.io/de0ch/hermit, newTag: \"aaaaaaaaaaaa\", digest: sha256:ffff }\n"
        );
        assert!(
            set_images(text, &tags, &BTreeMap::new()).is_err(),
            "a digest pin with no digest to write"
        );
    }

    #[test]
    fn an_image_of_ours_without_a_tag_is_an_error() {
        let text = "  - {name: yyc-drive, newName: ghcr.io/de0ch/yyc-drive, newTag: \"6b0f098ab09d\"}\n";
        assert!(set_tags(text, &tags()).is_err());
    }
}
