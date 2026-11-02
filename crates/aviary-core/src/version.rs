//! Build facts and protocol numbers (platform 3.1, 3.2, 3.4).
//!
//! The revision comes from `AVIARY_REVISION` at compile time (CI sets the commit's 40-hex sha; local builds say
//! `dev`), the build time from `SOURCE_DATE_EPOCH` (CI sets the commit time, so builds are reproducible). A service
//! declares the protocol surfaces it serves with [`Build::surface`]; `/version` and `--version` report them.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The 40-hex commit this binary was built from, or `dev`.
pub const REVISION: &str = match option_env!("AVIARY_REVISION") {
    Some(r) => r,
    None => "dev",
};

const SOURCE_DATE_EPOCH: Option<&str> = option_env!("SOURCE_DATE_EPOCH");

/// The short version: the first 12 characters of [`REVISION`] (`dev` locally).
#[must_use]
pub fn sha12() -> &'static str {
    REVISION.get(..12).unwrap_or(REVISION)
}

/// The build time, RFC 3339 UTC with milliseconds; the Unix epoch when `SOURCE_DATE_EPOCH` was not set.
#[must_use]
pub fn built_at() -> String {
    let secs = SOURCE_DATE_EPOCH
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let t = time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    crate::log::rfc3339(t)
}

/// The running program's name: the file name of `argv[0]` (each service's binary is named after the service).
#[must_use]
pub fn program() -> String {
    std::env::args_os()
        .next()
        .and_then(|a| {
            std::path::Path::new(&a)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "aviary".to_owned())
}

/// One protocol surface and number, as sent in `Aviary-Protocol: <surface>=<n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Protocol {
    /// `magpie-person`, `magpie-sandbox`, `hermit-core`, `hermit-sandbox`, `supervisor-control`, `gateway`.
    pub surface: &'static str,
    /// The surface's number.
    pub number: u32,
}

impl Protocol {
    /// A surface and its number.
    #[must_use]
    pub const fn new(surface: &'static str, number: u32) -> Self {
        Self { surface, number }
    }

    /// Parses an `Aviary-Protocol` value, `<surface>=<n>`.
    #[must_use]
    pub fn parse(value: &str) -> Option<(&str, u32)> {
        let (surface, n) = value.trim().split_once('=')?;
        let surface = surface.trim();
        if surface.is_empty() {
            return None;
        }
        Some((surface, n.trim().parse().ok()?))
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={}", self.surface, self.number)
    }
}

/// A service's build facts: its name, the build revision and the protocol numbers it provides and accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    name: String,
    provides: BTreeMap<String, u32>,
    accepts: BTreeMap<String, Vec<u32>>,
}

impl Build {
    /// A service named `name` (`magpie`, `pigeon`, …) with no protocol surfaces yet.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            provides: BTreeMap::new(),
            accepts: BTreeMap::new(),
        }
    }

    /// Serves `surface` at number `current` and accepts the current and the previous number (platform 3.4).
    #[must_use]
    pub fn surface(mut self, surface: &str, current: u32) -> Self {
        let accepted = if current > 1 {
            vec![current - 1, current]
        } else {
            vec![current]
        };
        self.provides.insert(surface.to_owned(), current);
        self.accepts.insert(surface.to_owned(), accepted);
        self
    }

    /// The service's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name as a Prometheus metric prefix (`-` becomes `_`).
    #[must_use]
    pub fn metric_prefix(&self) -> String {
        self.name.replace('-', "_")
    }

    /// Whether a request speaking `surface` at `number` is accepted.
    #[must_use]
    pub fn accepts(&self, surface: &str, number: u32) -> bool {
        self.accepts.get(surface).is_some_and(|ns| ns.contains(&number))
    }

    /// The `User-Agent` of this service's outbound calls: `<name>/<sha12>`.
    #[must_use]
    pub fn user_agent(&self) -> String {
        format!("{}/{}", self.name, sha12())
    }

    /// The body of `GET :9090/version`.
    #[must_use]
    pub fn info(&self) -> VersionInfo {
        VersionInfo {
            name: self.name.clone(),
            version: sha12().to_owned(),
            revision: REVISION.to_owned(),
            built_at: built_at(),
            provides: self.provides.clone(),
            accepts: self.accepts.clone(),
        }
    }

    /// The `--version` line: `<name> <sha12> protocols=<json>`, the json being `{"provides":…,"accepts":…}`.
    #[must_use]
    pub fn version_line(&self) -> String {
        let protocols = serde_json::json!({"provides": self.provides, "accepts": self.accepts});
        format!("{} {} protocols={protocols}", self.name, sha12())
    }
}

/// `GET :9090/version` (platform 3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    /// The service.
    pub name: String,
    /// [`sha12`].
    pub version: String,
    /// [`REVISION`].
    pub revision: String,
    /// [`built_at`].
    pub built_at: String,
    /// Surface → the number served.
    pub provides: BTreeMap<String, u32>,
    /// Surface → the numbers accepted.
    pub accepts: BTreeMap<String, Vec<u32>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_accept_current_and_previous() {
        let b = Build::new("magpie")
            .surface("magpie-sandbox", 2)
            .surface("magpie-person", 1);
        assert!(b.accepts("magpie-sandbox", 1));
        assert!(b.accepts("magpie-sandbox", 2));
        assert!(!b.accepts("magpie-sandbox", 3));
        assert!(b.accepts("magpie-person", 1));
        assert!(!b.accepts("magpie-person", 0));
        assert!(!b.accepts("hermit-core", 1));
    }

    #[test]
    fn info_is_camel_case() {
        let b = Build::new("magpie").surface("magpie-sandbox", 2);
        let v = serde_json::to_value(b.info()).unwrap();
        assert_eq!(v["name"], "magpie");
        assert_eq!(v["version"], sha12());
        assert_eq!(v["revision"], REVISION);
        assert!(v["builtAt"].as_str().unwrap().ends_with('Z'));
        assert_eq!(v["provides"]["magpie-sandbox"], 2);
        assert_eq!(v["accepts"]["magpie-sandbox"], serde_json::json!([1, 2]));
        assert!(v.get("built_at").is_none());
    }

    #[test]
    fn version_line_shape() {
        let b = Build::new("hermit").surface("hermit-core", 1);
        let line = b.version_line();
        let rest = line.strip_prefix(&format!("hermit {} protocols=", sha12())).unwrap();
        let p: serde_json::Value = serde_json::from_str(rest).unwrap();
        assert_eq!(p["provides"]["hermit-core"], 1);
    }

    #[test]
    fn sha12_is_at_most_twelve() {
        assert!(sha12().len() <= 12);
        assert!(REVISION.starts_with(sha12()));
    }

    #[test]
    fn protocol_header_round_trip() {
        let p = Protocol::new("magpie-sandbox", 2);
        assert_eq!(p.to_string(), "magpie-sandbox=2");
        assert_eq!(Protocol::parse(&p.to_string()), Some(("magpie-sandbox", 2)));
        assert_eq!(Protocol::parse("magpie-sandbox"), None);
        assert_eq!(Protocol::parse("=2"), None);
        assert_eq!(Protocol::parse("x=two"), None);
    }

    #[test]
    fn metric_prefix_has_no_hyphen() {
        assert_eq!(Build::new("fake-access").metric_prefix(), "fake_access");
    }
}
