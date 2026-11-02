//! The body of the supervisor's `POST /v1/exec` (supervisor 15.4). Defined here because hermit's audited admin route
//! `POST /v1/admin/sandboxes/{id}/exec` takes the same body; `supervisor-api` re-exports both types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Runs as uid 1000; argv non-empty without NUL, cwd `/workspace` or below, stdin at most 1 MiB, timeout at most
/// 60 s.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecRequest {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Base64 (standard alphabet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
    /// Variable names are data, not field names.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// stdout and stderr are base64, at most 1 MiB each before encoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

pub const EXEC_MAX_TIMEOUT_MS: u32 = 60_000;
pub const EXEC_MAX_STDIN: usize = 1024 * 1024;
pub const EXEC_MAX_OUTPUT: usize = 1024 * 1024;
