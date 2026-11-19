//! The pure parts: what to ask AWS for, from the config and what AWS answered. No I/O here.

use crate::config::{Config, release_pin};
use anyhow::{Result, bail};
use serde_json::{Value, json};

/// The IAM user hermit's scaler runs as (`iam`, `iam-key`).
pub const SCALER_USER: &str = "hermit-scaler";
/// The IAM user the AMI workflow runs as (`ci-iam`, `ci-iam-key`).
pub const CI_USER: &str = "hermit-ami-ci";
/// The builder's SSH rule's description: how a later bake finds a rule an earlier one left.
pub const SSH_RULE: &str = "ami build";

/// The config's tags plus `extra` (which win over a tag of the same key), sorted by key, as an EC2 `TagSpecification`
/// for one resource type.
#[must_use]
pub fn tagspec(cfg: &Config, resource: &str, extra: &[(&str, &str)]) -> Value {
    let mut tags: Vec<(String, String)> = cfg.tags.clone();
    for (k, v) in extra {
        tags.retain(|(tk, _)| tk != k);
        tags.push(((*k).to_owned(), (*v).to_owned()));
    }
    tags.sort_by(|a, b| a.0.cmp(&b.0));
    json!({"ResourceType": resource, "Tags": tags.iter().map(|(k, v)| json!({"Key": k, "Value": v})).collect::<Vec<_>>()})
}

/// The config's tags as an S3 or IAM `TagSet` list.
#[must_use]
pub fn tag_set(cfg: &Config) -> Value {
    Value::Array(cfg.tags.iter().map(|(k, v)| json!({"Key": k, "Value": v})).collect())
}

/// The launch template's data: the config's instance type, that AMI, the group, a public IPv4 and an IPv6 address in
/// the config's subnet (one AZ: host-to-host traffic is free), `IMDSv2` with hop limit 1, no instance profile, no key
/// pair, gp3 of `fabric.aws.diskGiB` deleted with the instance, terminate on shutdown.
#[must_use]
pub fn template_data(cfg: &Config, ami: &str, sg: &str) -> Value {
    json!({
        "ImageId": ami, "InstanceType": cfg.instance_type,
        "NetworkInterfaces": [{"DeviceIndex": 0, "SubnetId": cfg.subnet, "Groups": [sg], "AssociatePublicIpAddress": true,
                               "Ipv6AddressCount": 1, "DeleteOnTermination": true}],
        "MetadataOptions": {"HttpTokens": "required", "HttpPutResponseHopLimit": 1, "HttpEndpoint": "enabled",
                            "InstanceMetadataTags": "disabled"},
        "BlockDeviceMappings": [{"DeviceName": "/dev/sda1",
                                 "Ebs": {"VolumeSize": cfg.disk_gib, "VolumeType": "gp3", "DeleteOnTermination": true}}],
        "InstanceInitiatedShutdownBehavior": "terminate",
        "TagSpecifications": [tagspec(cfg, "network-interface", &[])]
    })
}

/// A policy's placeholders, in the order they are matched.
const PLACEHOLDERS: [&str; 6] = ["ACCOUNT", "LT_ID", "REGION", "TAG_APP", "TEMPLATE", "INSTANCE_TYPE"];

fn plain_name(v: &str) -> bool {
    let b = v.as_bytes();
    b.first().is_some_and(u8::is_ascii_alphanumeric)
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn fill_str(s: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        for p in PLACEHOLDERS {
            if let Some(after) = rest.strip_prefix(p) {
                let v = values.iter().find(|(k, _)| *k == p).map_or(p, |(_, v)| v);
                out.push_str(v);
                rest = after;
                continue 'outer;
            }
        }
        let mut chars = rest.chars();
        if let Some(c) = chars.next() {
            out.push(c);
        }
        rest = chars.as_str();
    }
    out
}

fn walk(v: &Value, values: &[(&str, &str)]) -> Value {
    match v {
        Value::String(s) => Value::String(fill_str(s, values)),
        Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, values)).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), walk(x, values))).collect()),
        other => other.clone(),
    }
}

/// A policy template with its placeholders filled in, in one pass (a value is never read as another placeholder).
/// Every value must be a plain name (`[A-Za-z0-9][A-Za-z0-9._-]*`), so nothing in the config can change the policy's
/// shape or widen an ARN.
///
/// # Errors
/// The template is not JSON, or a value is not a plain name.
pub fn fill_policy(template: &str, values: &[(&str, &str)]) -> Result<Value> {
    let bad: Vec<&str> = values.iter().filter(|(_, v)| !plain_name(v)).map(|(k, _)| *k).collect();
    if !bad.is_empty() {
        bail!("not a plain name in the config: {}", bad.join(", "));
    }
    let t: Value = serde_json::from_str(template)?;
    Ok(walk(&t, values))
}

/// The policy values of a deployment, for [`fill_policy`].
#[must_use]
pub fn policy_values<'a>(cfg: &'a Config, account: &'a str, lt_id: &'a str) -> [(&'static str, &'a str); 6] {
    [
        ("ACCOUNT", account),
        ("LT_ID", lt_id),
        ("REGION", &cfg.region),
        ("TAG_APP", &cfg.app),
        ("TEMPLATE", &cfg.name),
        ("INSTANCE_TYPE", &cfg.instance_type),
    ]
}

/// The image bucket's policy: anyone may read `images/*` through the hosts' S3 endpoint only (the hosts have no
/// instance profile; sandboxd fetches with plain curl), the scaler user may read (hermit asks whether an image is
/// there), the AMI workflow's user may write.
#[must_use]
pub fn bucket_policy(bucket: &str, endpoint: &str, account: &str) -> Value {
    let b = format!("arn:aws:s3:::{bucket}");
    let images = format!("arn:aws:s3:::{bucket}/images/*");
    json!({"Version": "2012-10-17", "Statement": [
        {"Sid": "HostsReadThroughTheEndpoint", "Effect": "Allow", "Principal": "*", "Action": "s3:GetObject",
         "Resource": images, "Condition": {"StringEquals": {"aws:SourceVpce": endpoint}}},
        {"Sid": "TheScalerAsksWhetherAnImageIsThere", "Effect": "Allow",
         "Principal": {"AWS": format!("arn:aws:iam::{account}:user/{SCALER_USER}")},
         "Action": ["s3:GetObject", "s3:ListBucket"], "Resource": [b, images]},
        {"Sid": "TheAmiWorkflowPublishes", "Effect": "Allow",
         "Principal": {"AWS": format!("arn:aws:iam::{account}:user/{CI_USER}")},
         "Action": ["s3:PutObject", "s3:GetObject", "s3:ListBucket", "s3:AbortMultipartUpload",
                    "s3:ListBucketMultipartUploads", "s3:ListMultipartUploadParts"],
         "Resource": [b, images]}]})
}

/// What an AMI holds, from its tags.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Held {
    pub image: Option<String>,
    pub sandboxd: Option<String>,
    pub runsc: Option<String>,
}

/// `check`'s verdict on the template's AMI against the pins.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The AMI holds the three pins and the template's disk is the config's.
    Ok(String),
    /// The AMI must be baked again (or never can match: the reason says).
    Stale(String),
    /// The AMI is right; only the template's disk differs: a new template version, no bake.
    DiskOnly(String),
}

impl Verdict {
    /// `check`'s exit status: 0, 1 (bake), 4 (template only).
    #[must_use]
    pub fn code(&self) -> u8 {
        match self {
            Verdict::Ok(_) => 0,
            Verdict::Stale(_) => 1,
            Verdict::DiskOnly(_) => 4,
        }
    }
}

/// Whether the template's AMI holds exactly the pins (the image's digest `want`) and the disk is the config's.
#[must_use]
pub fn verdict(cfg: &Config, want: &str, ami: &str, held: &Held, disk: Option<u32>) -> Verdict {
    if !release_pin(&cfg.pin_sandboxd) {
        return Verdict::Stale(format!(
            "fabric.sandboxd '{}' is not an aviary-<sha12> release, so no AMI can hold it",
            cfg.pin_sandboxd
        ));
    }
    let q = |o: &Option<String>| o.clone().unwrap_or_else(|| "?".to_owned());
    if held.sandboxd.as_deref() == Some("local") {
        return Verdict::Stale(format!(
            "{ami} holds a local sandboxd build (sandboxd=local), which never matches the pin {}",
            cfg.pin_sandboxd
        ));
    }
    let same = !want.is_empty()
        && held.image.as_deref() == Some(want)
        && held.sandboxd.as_deref() == Some(cfg.pin_sandboxd.as_str())
        && held.runsc.as_deref() == Some(cfg.pin_runsc.as_str());
    if !same {
        return Verdict::Stale(format!(
            "{ami} holds image {} sandboxd {} runsc {}; the pins are {want} {} {}",
            q(&held.image),
            q(&held.sandboxd),
            q(&held.runsc),
            cfg.pin_sandboxd,
            cfg.pin_runsc
        ));
    }
    if disk != Some(cfg.disk_gib) {
        return Verdict::DiskOnly(format!(
            "the template's disk is {} GiB; fabric.aws.diskGiB is {} ({ami} holds the pins)",
            disk.map_or_else(|| "?".to_owned(), |d| d.to_string()),
            cfg.disk_gib
        ));
    }
    Verdict::Ok(format!(
        "{ami} holds image {want}, sandboxd {}, runsc {}; disk {} GiB",
        cfg.pin_sandboxd, cfg.pin_runsc, cfg.disk_gib
    ))
}

/// An image reference's manifest digest (64 hex) when it is pinned by digest; `None` when the registry must be asked.
#[must_use]
pub fn pinned_digest(image: &str) -> Option<&str> {
    image.split_once("@sha256:").map(|(_, d)| d)
}

/// The repository of an image reference, without its tag or digest (`ghcr.io/a/b:t@sha256:…` → `ghcr.io/a/b`).
#[must_use]
pub fn repository(image: &str) -> &str {
    let no_digest = image.split_once('@').map_or(image, |(r, _)| r);
    // a tag's colon comes after the last slash (a registry's port comes before it)
    match (no_digest.rfind(':'), no_digest.rfind('/')) {
        (Some(c), Some(s)) if c > s => &no_digest[..c],
        (Some(c), None) => &no_digest[..c],
        _ => no_digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_filled_once() {
        let v = fill_policy(
            r#"{"a": "x:REGION:ACCOUNT", "b": ["TAG_APP-build"]}"#,
            &[("REGION", "eu-1"), ("ACCOUNT", "12"), ("TAG_APP", "REGION")],
        )
        .unwrap();
        assert_eq!(v, json!({"a": "x:eu-1:12", "b": ["REGION-build"]}));
    }

    #[test]
    fn a_value_that_is_not_a_plain_name_is_refused() {
        assert!(fill_policy("{}", &[("TEMPLATE", "a/*")]).is_err());
        assert!(fill_policy("{}", &[("TEMPLATE", "-a")]).is_err());
    }

    #[test]
    fn repositories() {
        assert_eq!(repository("ghcr.io/a/b:t@sha256:00"), "ghcr.io/a/b");
        assert_eq!(repository("ghcr.io/a/b@sha256:00"), "ghcr.io/a/b");
        assert_eq!(repository("localhost:5000/a/b:t"), "localhost:5000/a/b");
        assert_eq!(repository("localhost:5000/a/b"), "localhost:5000/a/b");
        assert_eq!(pinned_digest("x@sha256:ab"), Some("ab"));
        assert_eq!(pinned_digest("x:t"), None);
    }
}
