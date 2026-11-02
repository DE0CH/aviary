//! The process environment: the one module that reads it (platform 4.3, 4.5; CI's source rules allow
//! `std::env::var*` only in this file).
//!
//! A service reads only declared secrets, bindings and downward-API facts, each required: a missing or empty value
//! aborts start with one JSON line `{"msg":"missing env","name":…}` (the name, never a value) and exit code
//! [`EXIT_CONFIG`]. There is no optional configuration: what differs between deployments is an answer from another
//! service, not an environment switch.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;

/// Exit code of a start aborted by a missing or empty variable (`EX_CONFIG` of sysexits.h).
pub const EXIT_CONFIG: i32 = 78;

/// Prefix of the variables test code may read with [`test_only`].
pub const TEST_PREFIX: &str = "AVIARY_TEST_";

/// Why a required variable cannot be used. Carries the name only, never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvError {
    /// Not set.
    Missing(String),
    /// Set to the empty string (or only whitespace), or a list with an empty item.
    Empty(String),
    /// Set, but not valid UTF-8.
    NotUnicode(String),
}

impl EnvError {
    /// The variable's name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Missing(n) | Self::Empty(n) | Self::NotUnicode(n) => n,
        }
    }

    /// A short reason: `unset`, `empty` or `not utf-8`.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Missing(_) => "unset",
            Self::Empty(_) => "empty",
            Self::NotUnicode(_) => "not utf-8",
        }
    }
}

impl fmt::Display for EnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "env {} is {}", self.name(), self.reason())
    }
}

impl std::error::Error for EnvError {}

/// A required value (a secret or a binding). Aborts start when it is unset, empty or not UTF-8.
#[must_use]
pub fn required(name: &str) -> String {
    try_required(name).unwrap_or_else(|e| abort(&e))
}

/// A required comma-separated list (for example `MAGPIE_ACCEPT_PIGEON`, two values during a rotation): each item
/// trimmed; aborts start when the variable is unset or empty or any item is empty.
#[must_use]
pub fn required_list(name: &str) -> Vec<String> {
    try_required_list(name).unwrap_or_else(|e| abort(&e))
}

/// A downward-API fact (`POD_NAME`, `POD_IP`). Required like [`required`]; a separate name so call sites say what
/// kind of value they read.
#[must_use]
pub fn field(name: &str) -> String {
    required(name)
}

/// [`required`] without the abort.
///
/// # Errors
/// When the variable is unset, empty or not UTF-8.
pub fn try_required(name: &str) -> Result<String, EnvError> {
    required_from(name, process_env)
}

/// [`required_list`] without the abort.
///
/// # Errors
/// When the variable is unset, empty or not UTF-8, or an item is empty.
pub fn try_required_list(name: &str) -> Result<Vec<String>, EnvError> {
    required_list_from(name, process_env)
}

/// The rule of [`required`] over any lookup (tests pass a map).
///
/// # Errors
/// When `get` has no value, an empty or whitespace-only value, or one that is not UTF-8.
pub fn required_from(name: &str, get: impl Fn(&str) -> Option<OsString>) -> Result<String, EnvError> {
    let value = get(name).ok_or_else(|| EnvError::Missing(name.to_owned()))?;
    let value = value.into_string().map_err(|_| EnvError::NotUnicode(name.to_owned()))?;
    if value.trim().is_empty() {
        return Err(EnvError::Empty(name.to_owned()));
    }
    Ok(value)
}

/// The rule of [`required_list`] over any lookup.
///
/// # Errors
/// As [`required_from`], and when an item of the comma list is empty after trimming.
pub fn required_list_from(name: &str, get: impl Fn(&str) -> Option<OsString>) -> Result<Vec<String>, EnvError> {
    let value = required_from(name, get)?;
    value
        .split(',')
        .map(|item| {
            let item = item.trim();
            if item.is_empty() {
                Err(EnvError::Empty(name.to_owned()))
            } else {
                Ok(item.to_owned())
            }
        })
        .collect()
}

/// A test-only variable (`AVIARY_TEST_DATABASE_URL`, `AVIARY_TEST_S3_*`, set by CI for the test job), `None` when
/// unset or empty. Only for test code: a service never has optional configuration.
///
/// # Panics
/// When `name` does not start with [`TEST_PREFIX`], so it cannot become a way to read service configuration.
#[must_use]
pub fn test_only(name: &str) -> Option<String> {
    assert!(
        name.starts_with(TEST_PREFIX),
        "env::test_only reads only {TEST_PREFIX}* variables, not {name}"
    );
    try_required(name).ok()
}

#[allow(clippy::disallowed_methods)] // the one reader of the process environment (platform 4.5)
fn process_env(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// Writes the start failure line and exits with [`EXIT_CONFIG`]. Logging may not be set up yet, so the line is
/// written directly, in the log line's shape (15.1), naming the variable and never a value.
fn abort(e: &EnvError) -> ! {
    // the fixed fields first, in the order of every log line
    let q = |v: &str| serde_json::to_string(v).unwrap_or_default();
    let line = format!(
        r#"{{"t":{},"level":"error","svc":{},"msg":"missing env","name":{},"why":{}}}"#,
        q(&crate::log::now()),
        q(&crate::version::program()),
        q(e.name()),
        q(e.reason()),
    );
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    std::process::exit(EXIT_CONFIG)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::process::Command;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).into())).collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn required_reads_a_value() {
        assert_eq!(required_from("A", lookup(&[("A", "x")])).unwrap(), "x");
    }

    #[test]
    fn required_refuses_missing_and_empty() {
        assert_eq!(required_from("A", lookup(&[])), Err(EnvError::Missing("A".into())));
        assert_eq!(
            required_from("A", lookup(&[("A", "")])),
            Err(EnvError::Empty("A".into()))
        );
        assert_eq!(
            required_from("A", lookup(&[("A", "  ")])),
            Err(EnvError::Empty("A".into()))
        );
    }

    #[test]
    fn required_refuses_non_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let get = |_: &str| Some(OsString::from_vec(vec![0xff, 0xfe]));
        assert_eq!(required_from("A", get), Err(EnvError::NotUnicode("A".into())));
    }

    #[test]
    fn required_list_trims_and_refuses_empty_items() {
        assert_eq!(
            required_list_from("L", lookup(&[("L", " a , b")])).unwrap(),
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert_eq!(
            required_list_from("L", lookup(&[("L", "a")])).unwrap(),
            vec!["a".to_owned()]
        );
        assert_eq!(
            required_list_from("L", lookup(&[("L", "a,")])),
            Err(EnvError::Empty("L".into()))
        );
        assert_eq!(
            required_list_from("L", lookup(&[("L", ",")])),
            Err(EnvError::Empty("L".into()))
        );
        assert_eq!(required_list_from("L", lookup(&[])), Err(EnvError::Missing("L".into())));
    }

    #[test]
    fn errors_name_the_variable_never_a_value() {
        let e = required_from("SECRET_X", lookup(&[("SECRET_X", "")])).unwrap_err();
        assert_eq!(e.to_string(), "env SECRET_X is empty");
    }

    #[test]
    #[should_panic(expected = "reads only AVIARY_TEST_")]
    fn test_only_refuses_service_variables() {
        let _ = test_only("MAGPIE_DATABASE_URL");
    }

    /// Variables the child mode below uses; unset in a normal run, where the child test does nothing.
    const CHILD: &str = "AVIARY_CORE_ENV_ABORT_CHILD";
    const PROBE: &str = "AVIARY_CORE_ENV_ABORT_PROBE";

    /// Runs in a child process started by the abort tests: calls the aborting function named by [`CHILD`].
    #[test]
    fn abort_child() {
        let Some(mode) = process_env(CHILD) else { return };
        match mode.to_str() {
            Some("required") => drop(required(PROBE)),
            Some("required_list") => drop(required_list(PROBE)),
            Some("field") => drop(field(PROBE)),
            _ => {}
        }
        // reached only when the value was usable
        std::process::exit(0);
    }

    /// Runs [`abort_child`] in a child process with `PROBE` set to `probe` (or unset) and returns its exit code and
    /// the one JSON line it printed.
    fn run_child(mode: &str, probe: Option<&str>) -> (Option<i32>, serde_json::Value) {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "env::tests::abort_child", "--nocapture", "--test-threads=1"])
            .env(CHILD, mode);
        match probe {
            Some(v) => cmd.env(PROBE, v),
            None => cmd.env_remove(PROBE),
        };
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            // libtest may have written "test … " on the same line before the child exited
            .find_map(|l| l.find("{\"").map(|at| &l[at..]))
            .map_or(serde_json::Value::Null, |l| serde_json::from_str(l).unwrap());
        (out.status.code(), line)
    }

    #[test]
    fn required_aborts_on_missing() {
        let (code, line) = run_child("required", None);
        assert_eq!(code, Some(EXIT_CONFIG));
        assert_eq!(line["msg"], "missing env");
        assert_eq!(line["name"], PROBE);
        assert_eq!(line["why"], "unset");
        assert_eq!(line["level"], "error");
    }

    #[test]
    fn required_aborts_on_empty() {
        let (code, line) = run_child("required", Some(""));
        assert_eq!(code, Some(EXIT_CONFIG));
        assert_eq!(line["name"], PROBE);
        assert_eq!(line["why"], "empty");
    }

    #[test]
    fn required_list_aborts_on_empty_item() {
        let (code, line) = run_child("required_list", Some("tok-a,"));
        assert_eq!(code, Some(EXIT_CONFIG));
        assert_eq!(line["name"], PROBE);
        assert!(
            !line.to_string().contains("tok-a"),
            "the value is never printed: {line}"
        );
    }

    #[test]
    fn field_aborts_on_missing() {
        let (code, line) = run_child("field", None);
        assert_eq!(code, Some(EXIT_CONFIG));
        assert_eq!(line["name"], PROBE);
    }

    #[test]
    fn required_passes_a_set_value() {
        let (code, _) = run_child("required", Some("v"));
        assert_eq!(code, Some(0));
    }
}
