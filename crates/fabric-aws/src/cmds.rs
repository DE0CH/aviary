//! The subcommands that only talk to AWS (everything but the bake and the image upload, which run other programs:
//! `bake.rs`). Each is idempotent.

use crate::aws::{Aws, AwsError, call};
use crate::config::Config;
use crate::plan::{self, Held, SSH_RULE, Verdict};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::io::Write;

/// The JSON of a value for an argument (`--launch-template-data '{…}'`).
fn j(v: &Value) -> String {
    v.to_string()
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_owned)
}

/// The security group named after the template, if it exists.
///
/// # Errors
/// An AWS call failed.
pub fn sg_id(aws: &dyn Aws, cfg: &Config) -> Result<Option<String>> {
    let v = call(
        aws,
        [
            "ec2",
            "describe-security-groups",
            "--filters",
            &format!("Name=group-name,Values={}", cfg.name),
        ],
    )?;
    Ok(s(&v["SecurityGroups"][0]["GroupId"]))
}

fn vpc_of(aws: &dyn Aws, cfg: &Config) -> Result<String> {
    let v = call(aws, ["ec2", "describe-subnets", "--subnet-ids", &cfg.subnet])?;
    s(&v["Subnets"][0]["VpcId"]).with_context(|| format!("no VPC for subnet {}", cfg.subnet))
}

/// The subnet's route table, else the VPC's main one.
fn route_table(aws: &dyn Aws, cfg: &Config, vpc: &str) -> Result<String> {
    let v = call(
        aws,
        [
            "ec2",
            "describe-route-tables",
            "--filters",
            &format!("Name=association.subnet-id,Values={}", cfg.subnet),
        ],
    )?;
    if let Some(rt) = s(&v["RouteTables"][0]["RouteTableId"]) {
        return Ok(rt);
    }
    let v = call(
        aws,
        [
            "ec2",
            "describe-route-tables",
            "--filters",
            &format!("Name=vpc-id,Values={vpc}"),
            "Name=association.main,Values=true",
        ],
    )?;
    s(&v["RouteTables"][0]["RouteTableId"]).context("no route table")
}

fn account(aws: &dyn Aws) -> Result<String> {
    s(&call(aws, ["sts", "get-caller-identity"])?["Account"]).context("no account from sts")
}

/// `sg`: the security group `<template name>`: `WireGuard` (udp 51820) from the VPC only (hosts peer each other for
/// migrations; each dials the gateway itself); nothing else in.
///
/// # Errors
/// An AWS call failed.
pub fn sg(aws: &dyn Aws, cfg: &Config, out: &mut dyn Write) -> Result<()> {
    let id = if let Some(id) = sg_id(aws, cfg)? {
        id
    } else {
        let vpc = vpc_of(aws, cfg)?;
        let v = call(aws, ["ec2", "describe-vpcs", "--vpc-ids", &vpc])?;
        let cidr = s(&v["Vpcs"][0]["CidrBlock"]).context("no VPC CIDR")?;
        let v = call(
            aws,
            [
                "ec2",
                "create-security-group",
                "--group-name",
                &cfg.name,
                "--description",
                "hermit sandbox hosts: WireGuard between hosts",
                "--vpc-id",
                &vpc,
                "--tag-specifications",
                &j(&plan::tagspec(cfg, "security-group", &[])),
            ],
        )?;
        let id = s(&v["GroupId"]).context("no group id")?;
        call(
            aws,
            [
                "ec2",
                "authorize-security-group-ingress",
                "--group-id",
                &id,
                "--ip-permissions",
                &format!(
                    "IpProtocol=udp,FromPort=51820,ToPort=51820,IpRanges=[{{CidrIp={cidr},Description=sandbox hosts}}]"
                ),
            ],
        )?;
        id
    };
    writeln!(out, "{id}")?;
    Ok(())
}

fn vpc_v6(aws: &dyn Aws, vpc: &str) -> Result<Option<String>> {
    let v = call(aws, ["ec2", "describe-vpcs", "--vpc-ids", vpc])?;
    let set = v["Vpcs"][0]["Ipv6CidrBlockAssociationSet"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(set
        .iter()
        .find(|a| a["Ipv6CidrBlockState"]["State"] == "associated")
        .and_then(|a| s(&a["Ipv6CidrBlock"])))
}

/// `ipv6`: IPv6 for the hosts (free): an Amazon /56 on the VPC, a /64 on the subnet, `::/0` to the internet gateway,
/// IPv6 egress in the group. The gateway (a cluster node, IPv6 only from the outside) is reached over it.
///
/// # Errors
/// An AWS call failed, or the VPC's /56 never came.
pub fn ipv6(aws: &dyn Aws, cfg: &Config, out: &mut dyn Write, sleep: &dyn Fn(u64)) -> Result<()> {
    let vpc = vpc_of(aws, cfg)?;
    let mut v6 = vpc_v6(aws, &vpc)?;
    if v6.is_none() {
        call(
            aws,
            [
                "ec2",
                "associate-vpc-cidr-block",
                "--vpc-id",
                &vpc,
                "--amazon-provided-ipv6-cidr-block",
            ],
        )?;
        for _ in 0..30 {
            v6 = vpc_v6(aws, &vpc)?;
            if v6.is_some() {
                break;
            }
            sleep(2);
        }
    }
    let v6 = v6.context("the VPC's IPv6 /56 was never associated")?;
    let v = call(aws, ["ec2", "describe-subnets", "--subnet-ids", &cfg.subnet])?;
    let sub6 = if let Some(b) = s(&v["Subnets"][0]["Ipv6CidrBlockAssociationSet"][0]["Ipv6CidrBlock"]) {
        b
    } else {
        // the subnet's /64: the /56's first one (one subnet uses IPv6 here)
        let b = format!("{}::/64", v6.trim_end_matches("::/56"));
        call(
            aws,
            [
                "ec2",
                "associate-subnet-cidr-block",
                "--subnet-id",
                &cfg.subnet,
                "--ipv6-cidr-block",
                &b,
            ],
        )?;
        b
    };
    let rt = route_table(aws, cfg, &vpc)?;
    let v = call(
        aws,
        [
            "ec2",
            "describe-internet-gateways",
            "--filters",
            &format!("Name=attachment.vpc-id,Values={vpc}"),
        ],
    )?;
    let igw = s(&v["InternetGateways"][0]["InternetGatewayId"]).context("no internet gateway")?;
    // both exist already on a second run: AWS refuses a duplicate, which is the state wanted
    let _ = call(
        aws,
        [
            "ec2",
            "create-route",
            "--route-table-id",
            &rt,
            "--destination-ipv6-cidr-block",
            "::/0",
            "--gateway-id",
            &igw,
        ],
    );
    let sg = sg_id(aws, cfg)?;
    if let Some(sg) = &sg {
        let _ = call(
            aws,
            [
                "ec2",
                "authorize-security-group-egress",
                "--group-id",
                sg,
                "--ip-permissions",
                "IpProtocol=-1,Ipv6Ranges=[{CidrIpv6=::/0,Description=gateway over IPv6}]",
            ],
        );
    }
    writeln!(
        out,
        "vpc {vpc} {v6}, subnet {} {sub6}, route ::/0 -> {igw} in {rt}, sg {}",
        cfg.subnet,
        sg.unwrap_or_default()
    )?;
    Ok(())
}

/// The default template version's data.
fn default_version(aws: &dyn Aws, cfg: &Config) -> Result<Value> {
    let v = call(
        aws,
        [
            "ec2",
            "describe-launch-template-versions",
            "--launch-template-name",
            &cfg.name,
            "--versions",
            "$Default",
        ],
    )?;
    Ok(v["LaunchTemplateVersions"][0]["LaunchTemplateData"].clone())
}

/// The template's default AMI and disk.
///
/// # Errors
/// An AWS call failed, or the template has no AMI.
pub fn current_ami(aws: &dyn Aws, cfg: &Config) -> Result<(String, Option<u32>)> {
    let d = default_version(aws, cfg)?;
    let ami = s(&d["ImageId"]).context("the launch template has no AMI")?;
    let disk = d["BlockDeviceMappings"][0]["Ebs"]["VolumeSize"]
        .as_u64()
        .and_then(|x| u32::try_from(x).ok());
    Ok((ami, disk))
}

/// What an AMI's tags say it holds.
///
/// # Errors
/// An AWS call failed.
pub fn held(aws: &dyn Aws, ami: &str) -> Result<Held> {
    let v = call(aws, ["ec2", "describe-images", "--image-ids", ami])?;
    let tags = v["Images"][0]["Tags"].as_array().cloned().unwrap_or_default();
    let tag = |k: &str| tags.iter().find(|t| t["Key"] == k).and_then(|t| s(&t["Value"]));
    Ok(Held {
        image: tag("image"),
        sandboxd: tag("sandboxd"),
        runsc: tag("runsc"),
    })
}

/// `current`: the template's default AMI, its tags (image, sandboxd, runsc) and the disk.
///
/// # Errors
/// An AWS call failed.
pub fn current(aws: &dyn Aws, cfg: &Config, out: &mut dyn Write) -> Result<()> {
    let (ami, disk) = current_ami(aws, cfg)?;
    let h = held(aws, &ami)?;
    let o = |x: Option<String>| x.unwrap_or_default();
    writeln!(
        out,
        "{ami} image={} sandboxd={} runsc={} disk={}",
        o(h.image),
        o(h.sandboxd),
        o(h.runsc),
        disk.map(|d| d.to_string()).unwrap_or_default()
    )?;
    Ok(())
}

/// `check`: whether the template's AMI holds exactly the pins (`want`: the pinned image's digest).
///
/// # Errors
/// An AWS call failed.
pub fn check(aws: &dyn Aws, cfg: &Config, want: &str) -> Result<Verdict> {
    let (ami, disk) = current_ami(aws, cfg)?;
    let h = held(aws, &ami)?;
    Ok(plan::verdict(cfg, want, &ami, &h, disk))
}

fn template_id(aws: &dyn Aws, cfg: &Config) -> Result<Option<(String, u64)>> {
    match call(
        aws,
        ["ec2", "describe-launch-templates", "--launch-template-names", &cfg.name],
    ) {
        Ok(v) => Ok(s(&v["LaunchTemplates"][0]["LaunchTemplateId"]).map(|id| {
            (
                id,
                v["LaunchTemplates"][0]["DefaultVersionNumber"].as_u64().unwrap_or(0),
            )
        })),
        // the CLI's answer for a name that does not exist
        Err(AwsError { status: 254 | 255, .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `template <ami>`: a new default version of the launch template with that AMI ([`plan::template_data`]); the
/// template's other versions are deleted.
///
/// # Errors
/// An AWS call failed, or there is no security group yet.
pub fn template(aws: &dyn Aws, cfg: &Config, ami: &str, out: &mut dyn Write) -> Result<()> {
    let sg = sg_id(aws, cfg)?.context("no security group: run `sg` first")?;
    let data = j(&plan::template_data(cfg, ami, &sg));
    if template_id(aws, cfg)?.is_some() {
        let v = call(
            aws,
            [
                "ec2",
                "create-launch-template-version",
                "--launch-template-name",
                &cfg.name,
                "--launch-template-data",
                &data,
            ],
        )?;
        let ver = v["LaunchTemplateVersion"]["VersionNumber"]
            .as_u64()
            .context("no version number")?;
        call(
            aws,
            [
                "ec2",
                "modify-launch-template",
                "--launch-template-name",
                &cfg.name,
                "--default-version",
                &ver.to_string(),
            ],
        )?;
        let all = call(
            aws,
            [
                "ec2",
                "describe-launch-template-versions",
                "--launch-template-name",
                &cfg.name,
            ],
        )?;
        let old: Vec<String> = all["LaunchTemplateVersions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x["VersionNumber"].as_u64())
                    .filter(|n| *n != ver)
                    .map(|n| n.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !old.is_empty() {
            let mut args = vec![
                "ec2".to_owned(),
                "delete-launch-template-versions".to_owned(),
                "--launch-template-name".to_owned(),
                cfg.name.clone(),
                "--versions".to_owned(),
            ];
            args.extend(old);
            aws.call(&args)?;
        }
    } else {
        call(
            aws,
            [
                "ec2",
                "create-launch-template",
                "--launch-template-name",
                &cfg.name,
                "--launch-template-data",
                &data,
                "--tag-specifications",
                &j(&plan::tagspec(cfg, "launch-template", &[])),
            ],
        )?;
    }
    let (id, ver) = template_id(aws, cfg)?.context("the launch template is not there after writing it")?;
    writeln!(out, "{id}\t{ver}")?;
    Ok(())
}

/// `prune-amis`: deregisters every AMI tagged `app=<tags.app>` but the template's, and deletes their snapshots.
///
/// # Errors
/// An AWS call failed.
pub fn prune_amis(aws: &dyn Aws, cfg: &Config, out: &mut dyn Write) -> Result<()> {
    let (keep, _) = current_ami(aws, cfg)?;
    let v = call(
        aws,
        [
            "ec2",
            "describe-images",
            "--owners",
            "self",
            "--filters",
            &format!("Name=tag:app,Values={}", cfg.app),
        ],
    )?;
    for img in v["Images"].as_array().cloned().unwrap_or_default() {
        let Some(ami) = s(&img["ImageId"]) else { continue };
        if ami == keep {
            continue;
        }
        call(aws, ["ec2", "deregister-image", "--image-id", &ami])?;
        let snap = s(&img["BlockDeviceMappings"][0]["Ebs"]["SnapshotId"]);
        if let Some(snap) = &snap {
            call(aws, ["ec2", "delete-snapshot", "--snapshot-id", snap])?;
        }
        writeln!(out, "deleted {ami} {}", snap.unwrap_or_default())?;
    }
    Ok(())
}

/// `iam`: the user hermit's scaler runs as, with only what `scaler-policy.json` allows (an inline policy).
///
/// # Errors
/// An AWS call failed, a config value is not a plain name, or the template is missing.
pub fn iam(aws: &dyn Aws, cfg: &Config, out: &mut dyn Write) -> Result<()> {
    let acct = account(aws)?;
    let (lt, _) = template_id(aws, cfg)?.context("no launch template: run `template` first")?;
    let pol = plan::fill_policy(crate::SCALER_POLICY, &plan::policy_values(cfg, &acct, &lt))?;
    if call(aws, ["iam", "get-user", "--user-name", plan::SCALER_USER]).is_err() {
        call(
            aws,
            [
                "iam",
                "create-user",
                "--user-name",
                plan::SCALER_USER,
                "--tags",
                &format!("Key=app,Value={}", cfg.app),
            ],
        )?;
    }
    call(
        aws,
        [
            "iam",
            "put-user-policy",
            "--user-name",
            plan::SCALER_USER,
            "--policy-name",
            "hermit-hosts",
            "--policy-document",
            &j(&pol),
        ],
    )?;
    writeln!(
        out,
        "user {}: inline policy hermit-hosts (template {lt})",
        plan::SCALER_USER
    )?;
    Ok(())
}

/// `ci-iam`: the AMI workflow's user with the managed policy of its name (`ami-ci-policy.json`: a customer managed
/// policy, 6144 characters; an inline one allows 2048, too few for the region-pinned ARNs).
///
/// # Errors
/// An AWS call failed, a config value is not a plain name, or the template is missing.
pub fn ci_iam(aws: &dyn Aws, cfg: &Config, out: &mut dyn Write) -> Result<()> {
    let acct = account(aws)?;
    let (lt, _) = template_id(aws, cfg)?.context("no launch template: run `template` first")?;
    let pol = j(&plan::fill_policy(
        crate::AMI_CI_POLICY,
        &plan::policy_values(cfg, &acct, &lt),
    )?);
    let arn = format!("arn:aws:iam::{acct}:policy/{}", plan::CI_USER);
    let tag = format!("Key=app,Value={}", cfg.app);
    if call(aws, ["iam", "get-user", "--user-name", plan::CI_USER]).is_err() {
        call(
            aws,
            ["iam", "create-user", "--user-name", plan::CI_USER, "--tags", &tag],
        )?;
    }
    if call(aws, ["iam", "get-policy", "--policy-arn", &arn]).is_ok() {
        // IAM keeps at most 5 versions: the old non-default ones go first (git holds the history)
        let v = call(aws, ["iam", "list-policy-versions", "--policy-arn", &arn])?;
        for ver in v["Versions"].as_array().cloned().unwrap_or_default() {
            if ver["IsDefaultVersion"] == true {
                continue;
            }
            if let Some(id) = s(&ver["VersionId"]) {
                call(
                    aws,
                    [
                        "iam",
                        "delete-policy-version",
                        "--policy-arn",
                        &arn,
                        "--version-id",
                        &id,
                    ],
                )?;
            }
        }
        call(
            aws,
            [
                "iam",
                "create-policy-version",
                "--policy-arn",
                &arn,
                "--policy-document",
                &pol,
                "--set-as-default",
            ],
        )?;
    } else {
        call(
            aws,
            [
                "iam",
                "create-policy",
                "--policy-name",
                plan::CI_USER,
                "--policy-document",
                &pol,
                "--tags",
                &tag,
            ],
        )?;
    }
    call(
        aws,
        [
            "iam",
            "attach-user-policy",
            "--user-name",
            plan::CI_USER,
            "--policy-arn",
            &arn,
        ],
    )?;
    writeln!(out, "user {}: managed policy {arn} (template {lt})", plan::CI_USER)?;
    Ok(())
}

/// `iam-key` / `ci-iam-key`: a new access key of that user, written to `key_out` (mode 600), never to the output.
///
/// # Errors
/// An AWS call failed, or the file could not be written.
pub fn iam_key(aws: &dyn Aws, user: &str, key_out: &std::path::Path, out: &mut dyn Write) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let v = call(aws, ["iam", "create-access-key", "--user-name", user])?;
    let id = s(&v["AccessKey"]["AccessKeyId"]).context("no access key id")?;
    let secret = s(&v["AccessKey"]["SecretAccessKey"]).context("no secret key")?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(key_out)?;
    writeln!(f, "{id}\t{secret}")?;
    writeln!(out, "access key of {user} written to {}", key_out.display())?;
    Ok(())
}

/// `image-bucket`: the hosts' image store (`fabric.aws.imageBucket`): a private bucket in the config's region, an S3
/// gateway endpoint on the hosts' route table (free), and [`plan::bucket_policy`]. Nothing is ever deleted from it.
///
/// # Errors
/// An AWS call failed.
pub fn image_bucket(aws: &dyn Aws, cfg: &Config, out: &mut dyn Write) -> Result<()> {
    let bucket = cfg.bucket()?;
    let acct = account(aws)?;
    let vpc = vpc_of(aws, cfg)?;
    let rt = route_table(aws, cfg, &vpc)?;
    if call(aws, ["s3api", "head-bucket", "--bucket", bucket]).is_err() {
        call(
            aws,
            [
                "s3api",
                "create-bucket",
                "--bucket",
                bucket,
                "--create-bucket-configuration",
                &format!("LocationConstraint={}", cfg.region),
            ],
        )?;
    }
    call(
        aws,
        [
            "s3api",
            "put-public-access-block",
            "--bucket",
            bucket,
            "--public-access-block-configuration",
            "BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true",
        ],
    )?;
    call(
        aws,
        [
            "s3api",
            "put-bucket-tagging",
            "--bucket",
            bucket,
            "--tagging",
            &j(&serde_json::json!({"TagSet": plan::tag_set(cfg)})),
        ],
    )?;
    let svc = format!("com.amazonaws.{}.s3", cfg.region);
    let v = call(
        aws,
        [
            "ec2",
            "describe-vpc-endpoints",
            "--filters",
            &format!("Name=vpc-id,Values={vpc}"),
            &format!("Name=service-name,Values={svc}"),
            "Name=vpc-endpoint-type,Values=Gateway",
        ],
    )?;
    let ep = if let Some(ep) = s(&v["VpcEndpoints"][0]["VpcEndpointId"]) {
        // already on the route table on a second run: AWS refuses the duplicate, the state wanted
        let _ = call(
            aws,
            [
                "ec2",
                "modify-vpc-endpoint",
                "--vpc-endpoint-id",
                &ep,
                "--add-route-table-ids",
                &rt,
            ],
        );
        ep
    } else {
        let v = call(
            aws,
            [
                "ec2",
                "create-vpc-endpoint",
                "--vpc-id",
                &vpc,
                "--vpc-endpoint-type",
                "Gateway",
                "--service-name",
                &svc,
                "--route-table-ids",
                &rt,
                "--tag-specifications",
                &j(&plan::tagspec(cfg, "vpc-endpoint", &[])),
            ],
        )?;
        s(&v["VpcEndpoint"]["VpcEndpointId"]).context("no endpoint id")?
    };
    call(
        aws,
        [
            "s3api",
            "put-bucket-policy",
            "--bucket",
            bucket,
            "--policy",
            &j(&plan::bucket_policy(bucket, &ep, &acct)),
        ],
    )?;
    writeln!(
        out,
        "bucket {bucket} ({}), endpoint {ep} on {rt}, policy: hosts read images/* through the endpoint, {} reads, {} writes",
        cfg.region,
        plan::SCALER_USER,
        plan::CI_USER
    )?;
    Ok(())
}

/// What an earlier bake left when it was killed: its builder instances (tagged `app=<tags.app>-build`), their key
/// pairs, and the group's SSH rules it opened (described [`SSH_RULE`]). A bake begins by removing them, so a killed
/// job costs nothing past the next run.
///
/// # Errors
/// An AWS call failed.
pub fn clear_leftovers(aws: &dyn Aws, cfg: &Config, sg: &str, out: &mut dyn Write) -> Result<()> {
    let v = call(
        aws,
        [
            "ec2",
            "describe-instances",
            "--filters",
            &format!("Name=tag:app,Values={}-build", cfg.app),
            "Name=instance-state-name,Values=pending,running,stopping,stopped",
        ],
    )?;
    let mut ids = Vec::new();
    for r in v["Reservations"].as_array().cloned().unwrap_or_default() {
        for i in r["Instances"].as_array().cloned().unwrap_or_default() {
            if let Some(id) = s(&i["InstanceId"]) {
                if let Some(kp) = s(&i["KeyName"]).filter(|k| k.starts_with(&format!("{}-ami-", cfg.name))) {
                    call(aws, ["ec2", "delete-key-pair", "--key-name", &kp])?;
                }
                ids.push(id);
            }
        }
    }
    if !ids.is_empty() {
        let mut args = vec![
            "ec2".to_owned(),
            "terminate-instances".to_owned(),
            "--instance-ids".to_owned(),
        ];
        args.extend(ids.iter().cloned());
        aws.call(&args)?;
        writeln!(out, "terminated an earlier bake's builders: {}", ids.join(" "))?;
    }
    let v = call(aws, ["ec2", "describe-security-groups", "--group-ids", sg])?;
    for p in v["SecurityGroups"][0]["IpPermissions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if p["IpProtocol"] != "tcp" || p["FromPort"] != 22 {
            continue;
        }
        for r in p["IpRanges"].as_array().cloned().unwrap_or_default() {
            if r["Description"] == SSH_RULE
                && let Some(cidr) = s(&r["CidrIp"])
            {
                revoke_ssh(aws, sg, &cidr)?;
                writeln!(out, "revoked an earlier bake's SSH rule for {cidr}")?;
            }
        }
    }
    Ok(())
}

/// Revokes the builder's SSH rule for `cidr`.
///
/// # Errors
/// An AWS call failed.
pub fn revoke_ssh(aws: &dyn Aws, sg: &str, cidr: &str) -> Result<()> {
    call(
        aws,
        [
            "ec2",
            "revoke-security-group-ingress",
            "--group-id",
            sg,
            "--ip-permissions",
            &format!("IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{{CidrIp={cidr}}}]"),
        ],
    )?;
    Ok(())
}

/// Waits until `ready` says so, asking every `every` seconds, for at most `limit` seconds; says how long it took, and
/// what it last saw every few minutes (a 220 GiB snapshot takes far longer than the CLI's own 10-minute waiters).
///
/// # Errors
/// The limit passed, or an AWS call failed.
pub fn wait_for(
    what: &str,
    limit: u64,
    every: u64,
    sleep: &dyn Fn(u64),
    log: &mut dyn Write,
    mut ready: impl FnMut() -> Result<(bool, String)>,
) -> Result<()> {
    let mut waited = 0;
    loop {
        let (done, state) = ready()?;
        if done {
            writeln!(log, "{what}: {state} after {waited} s")?;
            return Ok(());
        }
        if waited >= limit {
            bail!("{what}: still {state} after {waited} s");
        }
        if waited > 0 && waited % 300 < every {
            writeln!(log, "{what}: {state} ({waited} s)")?;
        }
        sleep(every);
        waited += every;
    }
}
