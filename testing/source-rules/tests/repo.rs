//! The source rules of platform 4.5 hold on this repository.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may allow it (Cargo.toml lints)

use std::path::{Path, PathBuf};

use aviary_source_rules::{
    Violation, cargo_metadata, dependency_violations, env_var_violations, name_violations, testing_feature_violations,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn assert_none(rule: &str, found: &[Violation]) {
    let list: Vec<String> = found.iter().map(ToString::to_string).collect();
    assert!(found.is_empty(), "{rule}:\n  {}", list.join("\n  "));
}

#[test]
fn no_deployment_names_in_neutral_crates() {
    assert_none(
        "deployment names in neutral crates (platform 4.5)",
        &name_violations(&root()).unwrap(),
    );
}

#[test]
fn no_environment_reads_outside_aviary_core_env() {
    assert_none(
        "environment reads outside aviary_core::env (platform 4.5)",
        &env_var_violations(&root()).unwrap(),
    );
}

#[test]
fn only_hermit_depends_on_ec2_and_kube() {
    let metadata = cargo_metadata(env!("CARGO"), &root()).unwrap();
    assert_none(
        "EC2/Kubernetes dependencies outside hermit (platform 4.5)",
        &dependency_violations(&metadata).unwrap(),
    );
}

#[test]
fn no_service_enables_the_testing_feature() {
    let metadata = cargo_metadata(env!("CARGO"), &root()).unwrap();
    assert_none(
        "aviary-core/testing outside [dev-dependencies] (platform 4.5)",
        &testing_feature_violations(&metadata).unwrap(),
    );
}

#[test]
fn every_crate_is_agpl() {
    let metadata = cargo_metadata(env!("CARGO"), &root()).unwrap();
    let members: Vec<&str> = metadata["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    for p in metadata["packages"].as_array().unwrap() {
        if members.contains(&p["id"].as_str().unwrap()) {
            assert_eq!(
                p["license"].as_str(),
                Some("AGPL-3.0-or-later"),
                "licence of {}",
                p["name"]
            );
        }
    }
}
