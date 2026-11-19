//! The deployment's values, all from its `hermit.json` (hermit spec 9): nothing in this crate names a deployment.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;

/// What the AWS side reads from `hermit.json`.
#[derive(Debug, Clone)]
pub struct Config {
    pub region: String,
    pub subnet: String,
    /// the launch template's name, also the security group's
    pub name: String,
    /// `fabric.aws.tags.app`
    pub app: String,
    /// `fabric.aws.tags`, in the file's order
    pub tags: Vec<(String, String)>,
    /// `sandbox.image`
    pub pin_image: String,
    /// `fabric.sandboxd`
    pub pin_sandboxd: String,
    /// `fabric.runsc`
    pub pin_runsc: String,
    /// `fabric.aws.instanceType` (hermit spec 13.1's host size when absent)
    pub instance_type: String,
    /// `fabric.aws.diskGiB`: every sandbox's scratch in full beside the system and the image (40 when absent)
    pub disk_gib: u32,
    /// `fabric.aws.imageBucket`
    pub image_bucket: Option<String>,
}

fn text(v: &Value, path: &[&str]) -> Result<String> {
    let mut at = v;
    for p in path {
        at = &at[*p];
    }
    match at {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        _ => bail!("{} is missing from the config", path.join(".")),
    }
}

impl Config {
    /// # Errors
    /// The file unreadable, not JSON, a value missing, or a disk size or bucket name that is not one.
    pub fn load(path: &Path) -> Result<Config> {
        let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let v: Value = serde_json::from_str(&raw).with_context(|| format!("{} is not JSON", path.display()))?;
        Config::from_value(&v)
    }

    /// # Errors
    /// See [`Config::load`].
    pub fn from_value(v: &Value) -> Result<Config> {
        let aws = &v["fabric"]["aws"];
        let tags = match &aws["tags"] {
            Value::Object(m) => m
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
                .collect(),
            _ => bail!("fabric.aws.tags is missing from the config"),
        };
        let disk_gib = match &aws["diskGiB"] {
            Value::Null => 40,
            Value::Number(n) => match n.as_u64() {
                Some(d @ 10..=9999) => u32::try_from(d)?,
                _ => bail!("fabric.aws.diskGiB '{n}' is not a whole number of GiB (10-9999)"),
            },
            other => bail!("fabric.aws.diskGiB '{other}' is not a whole number of GiB (10-9999)"),
        };
        let image_bucket = match &aws["imageBucket"] {
            Value::String(b) => {
                if !bucket_name(b) {
                    bail!("fabric.aws.imageBucket '{b}' is not a bucket name");
                }
                Some(b.clone())
            }
            _ => None,
        };
        Ok(Config {
            region: text(v, &["fabric", "aws", "region"])?,
            subnet: text(v, &["fabric", "aws", "subnet"])?,
            name: text(v, &["fabric", "aws", "launchTemplate"])?,
            app: text(v, &["fabric", "aws", "tags", "app"])?,
            tags,
            pin_image: text(v, &["sandbox", "image"])?,
            pin_sandboxd: text(v, &["fabric", "sandboxd"])?,
            pin_runsc: text(v, &["fabric", "runsc"])?,
            instance_type: aws["instanceType"].as_str().unwrap_or("m7i.xlarge").to_owned(),
            disk_gib,
            image_bucket,
        })
    }

    /// # Errors
    /// No `fabric.aws.imageBucket` in the config.
    pub fn bucket(&self) -> Result<&str> {
        self.image_bucket
            .as_deref()
            .context("fabric.aws.imageBucket is missing from the config")
    }
}

/// An S3 bucket name as AWS takes it: 3-63 characters of a-z, 0-9, dots and dashes, a letter or digit at each end.
#[must_use]
pub fn bucket_name(b: &str) -> bool {
    let bytes = b.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'.' || *c == b'-')
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
}

/// A pin of sandboxd an AMI can hold: aviary's release tag `aviary-<sha12>` (platform 5.4).
#[must_use]
pub fn release_pin(s: &str) -> bool {
    s.strip_prefix("aviary-")
        .is_some_and(|h| h.len() == 12 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({"sandbox": {"image": "ghcr.io/x/s@sha256:aa"},
               "fabric": {"sandboxd": "aviary-0123456789ab", "runsc": "r1",
                          "aws": {"region": "xx-1", "subnet": "subnet-1", "launchTemplate": "t",
                                  "tags": {"app": "t", "n": 3}}}})
    }

    #[test]
    fn defaults_and_tags() {
        let c = Config::from_value(&base()).unwrap();
        assert_eq!(c.instance_type, "m7i.xlarge");
        assert_eq!(c.disk_gib, 40);
        assert_eq!(
            c.tags,
            vec![("app".to_owned(), "t".to_owned()), ("n".to_owned(), "3".to_owned())]
        );
    }

    #[test]
    fn a_disk_that_is_no_size_is_refused() {
        for bad in [json!("big"), json!(5), json!(10000), json!(20.5)] {
            let mut v = base();
            v["fabric"]["aws"]["diskGiB"] = bad;
            assert!(Config::from_value(&v).is_err());
        }
    }

    #[test]
    fn release_pins() {
        assert!(release_pin("aviary-0123456789ab"));
        assert!(!release_pin("local"));
        assert!(!release_pin("aviary-0123456789AB"));
        assert!(!release_pin("aviary-0123"));
    }

    #[test]
    fn bucket_names() {
        assert!(bucket_name("yyc-ai-sandbox-images-551897326524"));
        assert!(!bucket_name("Bad"));
        assert!(!bucket_name("-x-"));
        assert!(!bucket_name("ab"));
    }
}
