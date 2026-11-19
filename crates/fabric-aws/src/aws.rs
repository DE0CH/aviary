//! The AWS CLI, called with `--output json` and read as JSON: the one way this crate reaches AWS. A trait, so the tests
//! answer for it and record what was asked.

use serde_json::Value;
use std::process::Command;

/// A failed call: the CLI's exit status and what it said.
#[derive(Debug)]
pub struct AwsError {
    pub status: i32,
    pub message: String,
}

impl std::fmt::Display for AwsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "aws exited {}: {}", self.status, self.message.trim())
    }
}

impl std::error::Error for AwsError {}

/// One CLI call: `aws --region <region> <args…> --output json`, its JSON answer (`Null` for an empty one).
pub trait Aws {
    /// # Errors
    /// The CLI exited non-zero, or answered something that is not JSON.
    fn call(&self, args: &[String]) -> Result<Value, AwsError>;
}

/// The real CLI, in the config's region; credentials are the standard `AWS_*` environment of whoever runs it.
#[derive(Debug)]
pub struct Cli {
    pub region: String,
}

impl Aws for Cli {
    fn call(&self, args: &[String]) -> Result<Value, AwsError> {
        let out = Command::new("aws")
            .arg("--region")
            .arg(&self.region)
            .args(args)
            .args(["--output", "json"])
            .output()
            .map_err(|e| AwsError {
                status: -1,
                message: format!("running aws: {e}"),
            })?;
        if !out.status.success() {
            return Err(AwsError {
                status: out.status.code().unwrap_or(-1),
                message: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let s = String::from_utf8_lossy(&out.stdout);
        if s.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&s).map_err(|e| AwsError {
            status: 0,
            message: format!("not JSON ({e}): {s}"),
        })
    }
}

/// `aws(&a, ["ec2", "describe-x", …])` with the strings owned.
///
/// # Errors
/// See [`Aws::call`].
pub fn call<I, S>(aws: &dyn Aws, args: I) -> Result<Value, AwsError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    aws.call(&args)
}
