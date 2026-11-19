//! Offline: a stand-in for the AWS CLI records every call and answers from canned values, so these check that every
//! deployment value comes from the config (region, subnet, template, tags, the three pins), that the IAM policies are
//! filled in and stay under IAM's size limits, and that a bake first clears what a killed one left.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fabric_aws::aws::{Aws, AwsError};
use fabric_aws::cmds;
use fabric_aws::config::Config;
use fabric_aws::plan::{self, Verdict};
use serde_json::{Value, json};
use std::cell::RefCell;

type Answer = Box<dyn Fn(&str) -> Result<Value, AwsError>>;

/// The stand-in: each call's arguments joined by spaces, answered by the first rule whose words all appear.
struct Fake {
    log: RefCell<Vec<String>>,
    rules: Vec<(Vec<&'static str>, Answer)>,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            log: RefCell::new(Vec::new()),
            rules: Vec::new(),
        }
    }
    fn on(mut self, words: &[&'static str], v: Value) -> Fake {
        self.rules.push((words.to_vec(), Box::new(move |_| Ok(v.clone()))));
        self
    }
    fn fail(mut self, words: &[&'static str], status: i32) -> Fake {
        self.rules.push((
            words.to_vec(),
            Box::new(move |_| {
                Err(AwsError {
                    status,
                    message: "no such thing".into(),
                })
            }),
        ));
        self
    }
    fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
    fn has(&self, part: &str) -> bool {
        self.log.borrow().iter().any(|c| c.contains(part))
    }
    fn call_with(&self, part: &str) -> String {
        self.log
            .borrow()
            .iter()
            .find(|c| c.contains(part))
            .cloned()
            .unwrap_or_else(|| panic!("no call with {part}: {:?}", self.calls()))
    }
}

impl Aws for Fake {
    fn call(&self, args: &[String]) -> Result<Value, AwsError> {
        let line = args.join(" ");
        self.log.borrow_mut().push(line.clone());
        for (words, answer) in &self.rules {
            if words.iter().all(|w| line.contains(w)) {
                return answer(&line);
            }
        }
        Ok(Value::Null)
    }
}

const D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

fn config(edit: impl FnOnce(&mut Value)) -> Config {
    let mut v = json!({"sandbox": {"image": format!("ghcr.io/example/sandbox@sha256:{D}")},
        "fabric": {"sandboxd": "aviary-0123456789ab", "runsc": "precopy-20261021.2",
                   "aws": {"region": "xx-test-1", "launchTemplate": "test-host", "maxHosts": 3, "subnet": "subnet-0test",
                           "tags": {"app": "test-host", "deployment": "example"}}}});
    edit(&mut v);
    Config::from_value(&v).unwrap()
}

/// The stand-in answering as an existing deployment does: a template on ami-0cur with `disk` GiB, whose AMI holds
/// `image` and the two release pins.
fn deployed(image: &str, disk: u64) -> Fake {
    Fake::new()
        .on(&["sts get-caller-identity"], json!({"Account": "123456789012"}))
        .on(
            &["describe-launch-template-versions", "$Default"],
            json!({"LaunchTemplateVersions": [{"LaunchTemplateData": {
            "ImageId": "ami-0cur", "BlockDeviceMappings": [{"Ebs": {"VolumeSize": disk}}]}}]}),
        )
        .on(
            &["describe-launch-template-versions"],
            json!({"LaunchTemplateVersions": [{"VersionNumber": 1}, {"VersionNumber": 2}]}),
        )
        .on(
            &["describe-launch-templates"],
            json!({"LaunchTemplates": [{"LaunchTemplateId": "lt-0abc", "DefaultVersionNumber": 2}]}),
        )
        .on(
            &["create-launch-template-version"],
            json!({"LaunchTemplateVersion": {"VersionNumber": 2}}),
        )
        .on(
            &["describe-security-groups", "group-name"],
            json!({"SecurityGroups": [{"GroupId": "sg-0abc"}]}),
        )
        .on(
            &["describe-images", "--image-ids"],
            json!({"Images": [{"Tags": [
            {"Key": "image", "Value": image}, {"Key": "sandboxd", "Value": "aviary-0123456789ab"},
            {"Key": "runsc", "Value": "precopy-20261021.2"}, {"Key": "app", "Value": "test-host"}]}]}),
        )
        .on(
            &["iam list-policy-versions"],
            json!({"Versions": [{"VersionId": "v2", "IsDefaultVersion": true},
                                                               {"VersionId": "v1", "IsDefaultVersion": false}]}),
        )
}

fn text(f: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut out = Vec::new();
    f(&mut out);
    String::from_utf8(out).unwrap()
}

/// The JSON argument after `flag` in a logged call.
fn arg_json(call: &str, flag: &str) -> Value {
    let rest = call
        .split_once(&format!("{flag} "))
        .unwrap_or_else(|| panic!("no {flag} in {call}"))
        .1;
    let mut de = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    de.next().unwrap().unwrap()
}

#[test]
fn current_reads_the_template_and_the_amis_tags() {
    let aws = deployed(D, 40);
    let out = text(|o| cmds::current(&aws, &config(|_| {}), o).unwrap());
    assert_eq!(
        out,
        format!("ami-0cur image={D} sandboxd=aviary-0123456789ab runsc=precopy-20261021.2 disk=40\n")
    );
    assert!(aws.has("ec2 describe-launch-template-versions --launch-template-name test-host"));
}

#[test]
fn check_compares_the_ami_with_the_three_pins() {
    let cfg = config(|_| {});
    assert!(matches!(
        cmds::check(&deployed(D, 40), &cfg, D).unwrap(),
        Verdict::Ok(_)
    ));
    let other = D.replacen('d', "e", 1);
    let v = cmds::check(&deployed(&other, 40), &cfg, D).unwrap();
    assert_eq!(v.code(), 1, "{v:?}");
}

#[test]
fn a_pin_that_is_no_release_never_matches_and_says_why() {
    let cfg = config(|v| v["fabric"]["sandboxd"] = json!("local"));
    match cmds::check(&deployed(D, 40), &cfg, D).unwrap() {
        Verdict::Stale(why) => assert!(why.contains("not an aviary-<sha12> release"), "{why}"),
        v => panic!("{v:?}"),
    }
}

#[test]
fn only_the_disk_differs_without_a_bake() {
    let cfg = config(|v| v["fabric"]["aws"]["diskGiB"] = json!(220));
    match cmds::check(&deployed(D, 40), &cfg, D).unwrap() {
        Verdict::DiskOnly(why) => assert!(why.contains("disk is 40 GiB; fabric.aws.diskGiB is 220"), "{why}"),
        v => panic!("{v:?}"),
    }
    assert!(matches!(
        cmds::check(&deployed(D, 220), &cfg, D).unwrap(),
        Verdict::Ok(_)
    ));
    // the template written for it keeps the AMI and takes the disk
    let aws = deployed(D, 40);
    let _ = text(|o| cmds::template(&aws, &cfg, "ami-0cur", o).unwrap());
    let data = arg_json(
        &aws.call_with("create-launch-template-version"),
        "--launch-template-data",
    );
    assert_eq!(data["ImageId"], "ami-0cur");
    assert_eq!(data["BlockDeviceMappings"][0]["Ebs"]["VolumeSize"], 220);
}

#[test]
fn template_takes_the_configs_subnet_and_tags_and_deletes_old_versions() {
    let aws = deployed(D, 40);
    let out = text(|o| cmds::template(&aws, &config(|_| {}), "ami-0new", o).unwrap());
    assert_eq!(out, "lt-0abc\t2\n");
    let data = arg_json(
        &aws.call_with("create-launch-template-version --launch-template-name test-host"),
        "--launch-template-data",
    );
    assert_eq!(data["ImageId"], "ami-0new");
    assert_eq!(
        data["BlockDeviceMappings"][0]["Ebs"]["VolumeSize"], 40,
        "the default disk"
    );
    assert_eq!(data["NetworkInterfaces"][0]["SubnetId"], "subnet-0test");
    assert_eq!(data["NetworkInterfaces"][0]["Groups"], json!(["sg-0abc"]));
    let tags = &data["TagSpecifications"][0]["Tags"];
    assert!(
        tags.as_array()
            .unwrap()
            .contains(&json!({"Key": "app", "Value": "test-host"}))
    );
    assert!(
        tags.as_array()
            .unwrap()
            .contains(&json!({"Key": "deployment", "Value": "example"}))
    );
    assert!(aws.has("modify-launch-template --launch-template-name test-host --default-version 2"));
    assert!(aws.has("delete-launch-template-versions --launch-template-name test-host --versions 1"));
}

#[test]
fn iam_fills_the_scalers_policy() {
    let aws = deployed(D, 40);
    let _ = text(|o| cmds::iam(&aws, &config(|_| {}), o).unwrap());
    let pol = arg_json(
        &aws.call_with("put-user-policy --user-name hermit-scaler"),
        "--policy-document",
    );
    let s = pol.to_string();
    assert!(
        s.contains("arn:aws:ec2:xx-test-1:123456789012:launch-template/lt-0abc"),
        "{s}"
    );
    assert!(s.contains(r#""aws:RequestTag/app":"test-host""#), "{s}");
    assert!(s.contains(r#""ec2:InstanceType":"m7i.xlarge""#), "{s}");
    for p in ["ACCOUNT", "LT_ID", "REGION", "TAG_APP", "TEMPLATE", "INSTANCE_TYPE"] {
        assert!(!s.contains(p), "placeholder {p} left: {s}");
    }
    // IAM's inline policy limit, without whitespace
    assert!(s.len() <= 2048, "{} characters", s.len());
}

#[test]
fn ci_iam_makes_the_managed_policy_then_replaces_its_version() {
    let aws = deployed(D, 40).fail(&["iam get-policy"], 254);
    let _ = text(|o| cmds::ci_iam(&aws, &config(|_| {}), o).unwrap());
    let pol = arg_json(
        &aws.call_with("create-policy --policy-name hermit-ami-ci"),
        "--policy-document",
    )
    .to_string();
    assert!(pol.contains(r#""arn:aws:ec2:xx-test-1:*:instance/*""#), "{pol}");
    assert!(pol.contains("key-pair/test-host-ami-*"), "{pol}");
    assert!(pol.contains(r#""test-host-build""#), "{pol}");
    for p in ["ACCOUNT", "LT_ID", "REGION", "TAG_APP", "TEMPLATE"] {
        assert!(!pol.contains(p), "placeholder {p} left: {pol}");
    }
    assert!(aws.has(
        "iam attach-user-policy --user-name hermit-ami-ci --policy-arn arn:aws:iam::123456789012:policy/hermit-ami-ci"
    ));
    // a deployment naming its template and app tag with 24 characters each must still fit the managed policy limit
    let long = "x".repeat(24);
    let filled = plan::fill_policy(
        fabric_aws::AMI_CI_POLICY,
        &[
            ("ACCOUNT", "123456789012"),
            ("LT_ID", "lt-0123456789abcdef0"),
            ("REGION", "ap-southeast-3"),
            ("TAG_APP", &long),
            ("TEMPLATE", &long),
            ("INSTANCE_TYPE", "m7i.xlarge"),
        ],
    )
    .unwrap()
    .to_string();
    assert!(
        filled.len() <= 6144,
        "{} characters with 24-character names",
        filled.len()
    );

    // a second run: the old non-default version deleted, a new default version, no second policy
    let aws = deployed(D, 40);
    let _ = text(|o| cmds::ci_iam(&aws, &config(|_| {}), o).unwrap());
    assert!(
        aws.has(
            "iam delete-policy-version --policy-arn arn:aws:iam::123456789012:policy/hermit-ami-ci --version-id v1"
        )
    );
    assert!(!aws.has("--version-id v2"));
    assert!(aws.has("iam create-policy-version --policy-arn arn:aws:iam::123456789012:policy/hermit-ami-ci"));
    assert!(!aws.has("create-policy --policy-name"));
}

#[test]
fn a_config_value_that_is_no_plain_name_never_reaches_a_policy() {
    let aws = deployed(D, 40);
    let cfg = config(|v| v["fabric"]["aws"]["launchTemplate"] = json!("a/*"));
    assert!(cmds::iam(&aws, &cfg, &mut Vec::new()).is_err());
    assert!(!aws.has("put-user-policy"));
    // the config's instance type; a value that is a placeholder's name is not filled in again
    let aws = deployed(D, 40);
    let cfg = config(|v| {
        v["fabric"]["aws"]["instanceType"] = json!("c7i.2xlarge");
        v["fabric"]["aws"]["tags"]["app"] = json!("REGION");
    });
    let _ = text(|o| cmds::iam(&aws, &cfg, o).unwrap());
    let s = arg_json(&aws.call_with("put-user-policy"), "--policy-document").to_string();
    assert!(s.contains(r#""ec2:InstanceType":"c7i.2xlarge""#), "{s}");
    assert!(s.contains(r#""aws:RequestTag/app":"REGION""#), "{s}");
}

#[test]
fn sg_finds_the_group_by_the_templates_name() {
    let aws = deployed(D, 40);
    assert_eq!(text(|o| cmds::sg(&aws, &config(|_| {}), o).unwrap()), "sg-0abc\n");
    assert!(aws.has("describe-security-groups --filters Name=group-name,Values=test-host"));
    assert!(!aws.has("create-security-group"));
}

#[test]
fn a_bake_first_clears_what_a_killed_one_left() {
    let aws = deployed(D, 40)
        .on(
            &["describe-instances", "test-host-build"],
            json!({"Reservations": [{"Instances": [
            {"InstanceId": "i-0old", "KeyName": "test-host-ami-1763500000"},
            {"InstanceId": "i-0older", "KeyName": "someone-elses"}]}]}),
        )
        .on(
            &["describe-security-groups", "--group-ids sg-0abc"],
            json!({"SecurityGroups": [{"IpPermissions": [
            {"IpProtocol": "tcp", "FromPort": 22, "ToPort": 22, "IpRanges": [
                {"CidrIp": "203.0.113.7/32", "Description": "ami build"},
                {"CidrIp": "198.51.100.1/32", "Description": "an operator's"}]},
            {"IpProtocol": "udp", "FromPort": 51820, "ToPort": 51820, "IpRanges": [
                {"CidrIp": "172.31.0.0/16", "Description": "sandbox hosts"}]}]}]}),
        );
    let log = text(|o| cmds::clear_leftovers(&aws, &config(|_| {}), "sg-0abc", o).unwrap());
    assert!(
        aws.has("Name=tag:app,Values=test-host-build Name=instance-state-name,Values=pending,running,stopping,stopped")
    );
    assert!(
        aws.has("ec2 terminate-instances --instance-ids i-0old i-0older"),
        "{:?}",
        aws.calls()
    );
    assert!(aws.has("ec2 delete-key-pair --key-name test-host-ami-1763500000"));
    assert!(!aws.has("--key-name someone-elses"), "only the bake's own key pairs");
    assert!(aws.has("revoke-security-group-ingress --group-id sg-0abc --ip-permissions IpProtocol=tcp,FromPort=22,ToPort=22,IpRanges=[{CidrIp=203.0.113.7/32}]"));
    assert!(!aws.has("198.51.100.1"), "another rule on port 22 stays");
    assert!(log.contains("i-0old") && log.contains("203.0.113.7/32"), "{log}");

    // nothing left: nothing terminated or revoked
    let aws = deployed(D, 40);
    let _ = text(|o| cmds::clear_leftovers(&aws, &config(|_| {}), "sg-0abc", o).unwrap());
    assert!(
        !aws.has("terminate-instances") && !aws.has("revoke-security-group-ingress"),
        "{:?}",
        aws.calls()
    );
}

#[test]
fn a_wait_outlasts_the_clis_ten_minutes_and_has_a_limit() {
    // an AMI that takes 25 minutes, asked every 20 s: still waited for
    let mut asked = 0u64;
    let log = text(|o| {
        cmds::wait_for("AMI ami-0new", 90 * 60, 20, &|_| {}, o, || {
            asked += 1;
            Ok((
                asked * 20 >= 25 * 60,
                if asked * 20 >= 25 * 60 { "available" } else { "pending" }.to_owned(),
            ))
        })
        .unwrap();
    });
    assert!(log.contains("AMI ami-0new: available after 1480 s"), "{log}");
    assert!(log.contains("pending (300 s)"), "progress every 5 minutes: {log}");
    // one that never comes: an error at the limit, not a hang
    let e = cmds::wait_for("AMI ami-0never", 600, 20, &|_| {}, &mut Vec::new(), || {
        Ok((false, "pending".to_owned()))
    })
    .unwrap_err();
    assert!(e.to_string().contains("still pending after 600 s"), "{e}");
}
