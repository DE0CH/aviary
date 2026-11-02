//! The process environment: the one module that reads it (platform 4.3, 4.5; CI's source rules allow
//! `std::env::var*` only in this file).
//!
//! A service reads only declared secrets, bindings and downward-API facts, each required: a missing or empty value
//! aborts start with one JSON line `{"msg":"missing env","name":…}` (the name, never a value) and exit code
//! [`EXIT_CONFIG`]; a value that is present but unusable (a token shared by two callers, [`crate::auth`]) aborts
//! the same way with `{"msg":"invalid env",…}`. The line goes where [`crate::log::init`] sends logs (stdout, or
//! stderr for the supervisor), stdout when the variable is read before that. There is no optional configuration: what differs between deployments is an answer from another
//! service, not an environment switch.

use std::ffi::OsString;
use std::fmt;

/// Exit code of a start aborted by a missing or empty variable (`EX_CONFIG` of sysexits.h).
pub const EXIT_CONFIG: i32 = 78;

/// Prefix of the variables test code may read with `test_only` (feature `testing`).
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

/// A required value (a secret or a binding), trimmed (a Secret written with a trailing newline gives the same value
/// the callee side compares). Aborts start when it is unset, empty or not UTF-8.
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
/// When `get` has no value, an empty or whitespace-only value, or one that is not UTF-8. The value is returned
/// trimmed.
pub fn required_from(name: &str, get: impl Fn(&str) -> Option<OsString>) -> Result<String, EnvError> {
    let value = get(name).ok_or_else(|| EnvError::Missing(name.to_owned()))?;
    let value = value.into_string().map_err(|_| EnvError::NotUnicode(name.to_owned()))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(EnvError::Empty(name.to_owned()));
    }
    Ok(if trimmed.len() == value.len() {
        value
    } else {
        trimmed.to_owned()
    })
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
/// unset or empty. Only for test code: a service never has optional configuration, so it exists only in this
/// crate's own tests and behind the `testing` feature, which a crate enables from `[dev-dependencies]` only (the
/// source rules refuse it in `[dependencies]`).
///
/// # Panics
/// When `name` does not start with [`TEST_PREFIX`], so it cannot become a way to read service configuration.
#[cfg(any(test, feature = "testing"))]
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

/// Writes the start failure line for a missing or empty variable and exits with [`EXIT_CONFIG`].
fn abort(e: &EnvError) -> ! {
    abort_start("missing env", e.name(), e.reason())
}

/// Ends start with one JSON line and exit code [`EXIT_CONFIG`]: `msg`, the variable's `name` and `why` it cannot
/// be used, never a value. Logging may not be set up yet, so the line is written directly, in the log line's shape
/// (15.1): the fixed fields `t`, `level`, `svc`, `ver`, `pod` (when known) and `msg` in that order. `svc` is the
/// service named by [`crate::log::init`] or [`crate::log::init_in_pod`], or the binary's name (each binary is named
/// after its service) when the variable is read before that; the line goes to that call's output, else stdout.
pub(crate) fn abort_start(msg: &str, name: &str, why: &str) -> ! {
    let mut line = abort_line(msg, name, why);
    line.push('\n');
    crate::log::Sink::write_line(&crate::log::output(), line.as_bytes());
    std::process::exit(EXIT_CONFIG)
}

fn abort_line(msg: &str, name: &str, why: &str) -> String {
    let q = |v: &str| serde_json::to_string(v).unwrap_or_default();
    let svc = crate::log::svc_name().map_or_else(crate::version::program, str::to_owned);
    let pod = crate::log::pod_name()
        .map(|p| format!(r#","pod":{}"#, q(p)))
        .unwrap_or_default();
    format!(
        r#"{{"t":{},"level":"error","svc":{},"ver":{}{pod},"msg":{},"name":{},"why":{}}}"#,
        q(&crate::log::now()),
        q(&svc),
        q(crate::version::sha12()),
        q(msg),
        q(name),
        q(why),
    )
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
    fn required_trims_the_value() {
        // a Secret written with a trailing newline gives the token the callee side compares
        assert_eq!(required_from("A", lookup(&[("A", "tok-1\n")])).unwrap(), "tok-1");
        assert_eq!(required_from("A", lookup(&[("A", " tok-1 \r\n")])).unwrap(), "tok-1");
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
            Some("named") => {
                let build = crate::version::Build::new("magpie");
                crate::log::init(&build, Some("magpie-7c9".to_owned()), crate::log::Output::Stdout);
                drop(required(PROBE));
            }
            Some("named_stderr") => {
                let build = crate::version::Build::new("supervisor");
                crate::log::init(&build, None, crate::log::Output::Stderr);
                drop(required(PROBE));
            }
            Some("shared_token") => {
                // PROBE_ACCEPT_PIGEON and PROBE_ACCEPT_HERMIT hold the same token
                drop(crate::auth::Callers::from_env(
                    "AVIARY_CORE_ENV_ABORT_PROBE",
                    &[crate::auth::Caller::Pigeon, crate::auth::Caller::Hermit],
                ));
            }
            _ => {}
        }
        // reached only when the value was usable
        std::process::exit(0);
    }

    /// Runs [`abort_child`] in a child process with `PROBE` set to `probe` (or unset) and returns its exit code and
    /// the one JSON line it printed.
    fn run_child(mode: &str, probe: Option<&str>) -> (Option<i32>, serde_json::Value) {
        let (code, raw) = run_child_raw(mode, probe);
        let line = if raw.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&raw).unwrap()
        };
        (code, line)
    }

    /// [`run_child`], with the line as printed.
    fn run_child_raw(mode: &str, probe: Option<&str>) -> (Option<i32>, String) {
        let (code, stdout, _) = run_child_env(mode, probe, &[]);
        (code, stdout)
    }

    /// Runs [`abort_child`] with `PROBE` set to `probe` (or unset) and the variables of `extra`; returns its exit
    /// code and the first JSON line it printed on stdout and on stderr.
    fn run_child_env(mode: &str, probe: Option<&str>, extra: &[(&str, &str)]) -> (Option<i32>, String, String) {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "env::tests::abort_child", "--nocapture", "--test-threads=1"])
            .env(CHILD, mode);
        match probe {
            Some(v) => cmd.env(PROBE, v),
            None => cmd.env_remove(PROBE),
        };
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        // libtest may have written "test … " on the same line before the child exited
        let first_json = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .find_map(|l| l.find("{\"").map(|at| l[at..].to_owned()))
                .unwrap_or_default()
        };
        (out.status.code(), first_json(&out.stdout), first_json(&out.stderr))
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

    /// The keys of a JSON object line, in order.
    fn keys_in_order(line: &str) -> Vec<String> {
        let v: serde_json::Map<String, serde_json::Value> = serde_json::from_str(line).unwrap();
        let mut keys: Vec<(usize, String)> = v
            .keys()
            .map(|k| (line.find(&format!("\"{k}\":")).unwrap(), k.clone()))
            .collect();
        keys.sort();
        keys.into_iter().map(|(_, k)| k).collect()
    }

    #[test]
    fn abort_line_has_the_fixed_fields_in_order() {
        let e = EnvError::Missing("MAGPIE_DATABASE_URL".into());
        let line = abort_line("missing env", e.name(), e.reason());
        assert_eq!(
            keys_in_order(&line),
            ["t", "level", "svc", "ver", "msg", "name", "why"],
            "{line}"
        );
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ver"], crate::version::sha12());
        assert_eq!(v["msg"], "missing env");
        assert_eq!(v["level"], "error");
    }

    #[test]
    fn abort_line_names_the_service_and_pod_once_known() {
        // only the child process names a service, so this process's globals stay unset
        let (code, raw) = run_child_raw("named", None);
        assert_eq!(code, Some(EXIT_CONFIG));
        let line: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(line["svc"], "magpie");
        assert_eq!(line["pod"], "magpie-7c9");
        assert_eq!(line["ver"], crate::version::sha12());
        assert_eq!(
            keys_in_order(&raw),
            ["t", "level", "svc", "ver", "pod", "msg", "name", "why"],
            "{raw}"
        );
    }

    #[test]
    fn abort_line_goes_to_the_log_output() {
        let (code, stdout, stderr) = run_child_env("named_stderr", None, &[]);
        assert_eq!(code, Some(EXIT_CONFIG));
        assert_eq!(stdout, "", "the supervisor logs to stderr");
        let line: serde_json::Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(line["svc"], "supervisor");
        assert_eq!(line["name"], PROBE);
    }

    #[test]
    fn a_shared_token_aborts_like_a_missing_variable() {
        let (code, stdout, stderr) = run_child_env(
            "shared_token",
            None,
            &[
                ("AVIARY_CORE_ENV_ABORT_PROBE_ACCEPT_PIGEON", "tok-same"),
                ("AVIARY_CORE_ENV_ABORT_PROBE_ACCEPT_HERMIT", "tok-other, tok-same"),
            ],
        );
        assert_eq!(code, Some(EXIT_CONFIG), "stderr: {stderr}");
        let line: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(line["level"], "error");
        assert_eq!(line["msg"], "invalid env");
        assert_eq!(line["name"], "AVIARY_CORE_ENV_ABORT_PROBE_ACCEPT_HERMIT");
        assert_eq!(
            line["why"],
            "shares a token with AVIARY_CORE_ENV_ABORT_PROBE_ACCEPT_PIGEON"
        );
        assert!(!stdout.contains("tok-same") && !stderr.contains("tok-same"));
        assert!(!stderr.contains("panicked"), "{stderr}");
    }

    #[test]
    fn required_passes_a_set_value() {
        let (code, _) = run_child("required", Some("v"));
        assert_eq!(code, Some(0));
    }
}
