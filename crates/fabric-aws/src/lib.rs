//! The AWS side of hermit's fabric (crates/sandboxd/docs/HOST.md, "Burst hosts"): the hosts' security group and IPv6,
//! the AMI and its launch template, the scaler's and the AMI workflow's IAM users, the hosts' image bucket. Every
//! deployment value comes from the deployment's `hermit.json` (hermit spec 9); nothing here names a deployment. AWS is
//! reached through its CLI (`aws.rs`), so the tests answer for it.

pub mod aws;
pub mod bake;
pub mod cmds;
pub mod config;
pub mod plan;

/// hermit-scaler's inline policy, with placeholders ([`plan::fill_policy`])
pub const SCALER_POLICY: &str = include_str!("../../../fabric/aws/scaler-policy.json");
/// hermit-ami-ci's managed policy, with placeholders
pub const AMI_CI_POLICY: &str = include_str!("../../../fabric/aws/ami-ci-policy.json");
