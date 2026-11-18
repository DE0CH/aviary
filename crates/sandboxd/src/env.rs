//! The process environment, read once at start: the only place sandboxd reads it (platform 4.5's rule, with
//! sandboxd's own file because it cannot use `aviary-core`: no tokio, no heap after start, docs/MEMORY.md).
//! sandboxd's configuration is host.json; these are where it lives and the test stand-ins.
#![allow(clippy::disallowed_methods)]

use std::ffi::OsString;

/// `SBX_ROOT` (tests only): a prefix for every path sandboxd itself reads or writes.
pub fn root() -> Option<OsString> {
    std::env::var_os("SBX_ROOT")
}

/// `SBX_RUNSC` (tests only): a replacement for the runsc binary.
pub fn runsc() -> Option<OsString> {
    std::env::var_os("SBX_RUNSC")
}

/// `PATH`, handed to the commands sandboxd runs.
pub fn path() -> Option<String> {
    std::env::var("PATH").ok()
}

/// `SBX_CONFIG`: host.json's path (default /etc/sbx/host.json).
pub fn config() -> Option<String> {
    std::env::var("SBX_CONFIG").ok()
}

/// `SBX_SEAL_KEY`: the freeze's key, for the `sandboxd seal` / `sandboxd open` stage only (seal.rs), 64 hex digits.
pub fn seal_key() -> Option<String> {
    std::env::var("SBX_SEAL_KEY").ok()
}

/// The S3 stages' credentials (s3.rs): `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`,
/// `AWS_DEFAULT_REGION`, and `SBX_S3_ENDPOINT` (tests: another endpoint, path-style).
pub fn s3_env() -> [Option<String>; 5] {
    [
        std::env::var("AWS_ACCESS_KEY_ID").ok(),
        std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
        std::env::var("AWS_SESSION_TOKEN").ok(),
        std::env::var("AWS_DEFAULT_REGION").ok(),
        std::env::var("SBX_S3_ENDPOINT").ok(),
    ]
}
