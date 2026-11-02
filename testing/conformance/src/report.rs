//! Check results: one line per check on stdout as it finishes, and JUnit XML at the end.

use std::fmt::Write as _;
use std::future::Future;
use std::io::Write as _;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Why a check did not pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    Fail(String),
    /// The check cannot run against this implementation or with these options.
    Skip(String),
}

impl From<String> for Problem {
    fn from(s: String) -> Self {
        Self::Fail(s)
    }
}

impl From<&str> for Problem {
    fn from(s: &str) -> Self {
        Self::Fail(s.to_owned())
    }
}

/// A check's result.
pub type Outcome = Result<(), Problem>;

/// Fails the check with a formatted message unless `cond` holds.
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err($crate::report::Problem::Fail(format!($($arg)+)));
        }
    };
}

/// Fails the check with a formatted message.
#[macro_export]
macro_rules! fail {
    ($($arg:tt)+) => {
        return Err($crate::report::Problem::Fail(format!($($arg)+)))
    };
}

/// One finished check.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub name: String,
    pub outcome: Outcome,
    pub time: Duration,
}

/// The results of one suite run.
#[derive(Debug)]
pub struct Report {
    suite: String,
    only: Vec<String>,
    print: bool,
    started: Instant,
    results: Mutex<Vec<Recorded>>,
}

impl Report {
    /// A report for `suite`; `only` (check-name prefixes, empty = all) selects what runs; `print` writes each line
    /// to stdout.
    #[must_use]
    pub fn new(suite: &str, only: Vec<String>, print: bool) -> Self {
        Self {
            suite: suite.to_owned(),
            only,
            print,
            started: Instant::now(),
            results: Mutex::new(Vec::new()),
        }
    }

    /// Whether a check named `name` is selected.
    #[must_use]
    pub fn selected(&self, name: &str) -> bool {
        self.only.is_empty() || self.only.iter().any(|p| name.starts_with(p.as_str()))
    }

    /// Whether any check with this prefix is selected.
    #[must_use]
    pub fn any_selected(&self, prefix: &str) -> bool {
        self.only.is_empty()
            || self
                .only
                .iter()
                .any(|p| p.starts_with(prefix) || prefix.starts_with(p.as_str()))
    }

    /// Runs `check` if selected and records it.
    pub async fn check<F: Future<Output = Outcome>>(&self, name: &str, check: F) {
        if !self.selected(name) {
            return;
        }
        let t = Instant::now();
        let outcome = check.await;
        self.record(name, outcome, t.elapsed());
    }

    /// Records a result.
    pub fn record(&self, name: &str, outcome: Outcome, time: Duration) {
        if self.print {
            let line = match &outcome {
                Ok(()) => format!("PASS {}/{name} {:.2}s", self.suite, time.as_secs_f64()),
                Err(Problem::Fail(m)) => format!("FAIL {}/{name} {:.2}s: {m}", self.suite, time.as_secs_f64()),
                Err(Problem::Skip(m)) => format!("SKIP {}/{name}: {m}", self.suite),
            };
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
        self.results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Recorded {
                name: name.to_owned(),
                outcome,
                time,
            });
    }

    /// Every result so far.
    #[must_use]
    pub fn results(&self) -> Vec<Recorded> {
        self.results.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The names of the failed checks with their messages.
    #[must_use]
    pub fn failures(&self) -> Vec<(String, String)> {
        self.results()
            .into_iter()
            .filter_map(|r| match r.outcome {
                Err(Problem::Fail(m)) => Some((r.name, m)),
                _ => None,
            })
            .collect()
    }

    /// No check failed and at least one ran.
    #[must_use]
    pub fn passed(&self) -> bool {
        let r = self.results();
        !r.is_empty() && !r.iter().any(|r| matches!(r.outcome, Err(Problem::Fail(_))))
    }

    /// JUnit XML: one `testsuite` named after the suite, one `testcase` per check.
    #[must_use]
    pub fn junit(&self) -> String {
        let results = self.results();
        let failures = results
            .iter()
            .filter(|r| matches!(r.outcome, Err(Problem::Fail(_))))
            .count();
        let skipped = results
            .iter()
            .filter(|r| matches!(r.outcome, Err(Problem::Skip(_))))
            .count();
        let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        let _ = writeln!(
            x,
            "<testsuites name=\"aviary-conformance\" tests=\"{}\" failures=\"{failures}\" skipped=\"{skipped}\" time=\"{:.3}\">",
            results.len(),
            self.started.elapsed().as_secs_f64()
        );
        let _ = writeln!(
            x,
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{failures}\" skipped=\"{skipped}\" time=\"{:.3}\">",
            escape(&self.suite),
            results.len(),
            self.started.elapsed().as_secs_f64()
        );
        for r in &results {
            let _ = write!(
                x,
                "    <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\"",
                escape(&self.suite),
                escape(&r.name),
                r.time.as_secs_f64()
            );
            match &r.outcome {
                Ok(()) => x.push_str("/>\n"),
                Err(Problem::Fail(m)) => {
                    let _ = writeln!(
                        x,
                        ">\n      <failure message=\"{}\">{}</failure>\n    </testcase>",
                        escape(m),
                        escape(m)
                    );
                }
                Err(Problem::Skip(m)) => {
                    let _ = writeln!(x, ">\n      <skipped message=\"{}\"/>\n    </testcase>", escape(m));
                }
            }
        }
        x.push_str("  </testsuite>\n</testsuites>\n");
        x
    }
}

/// XML text and attribute escaping (control characters other than tab, CR and LF dropped).
#[must_use]
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junit_counts_and_escapes() {
        let r = Report::new("gateway", vec![], false);
        r.record("a", Ok(()), Duration::from_millis(10));
        r.record("b<1>", Err(Problem::Fail("x & \"y\"".into())), Duration::ZERO);
        r.record("c", Err(Problem::Skip("no".into())), Duration::ZERO);
        let x = r.junit();
        assert!(x.contains("tests=\"3\" failures=\"1\" skipped=\"1\""));
        assert!(x.contains("name=\"b&lt;1&gt;\""));
        assert!(x.contains("<failure message=\"x &amp; &quot;y&quot;\">"));
        assert!(x.contains("<skipped message=\"no\"/>"));
        assert!(!r.passed());
        assert_eq!(r.failures().len(), 1);
    }

    #[test]
    fn selection_by_prefix() {
        let r = Report::new("gateway", vec!["route.".into()], false);
        assert!(r.selected("route.magpie"));
        assert!(!r.selected("drain"));
        assert!(r.any_selected("route"));
        let all = Report::new("gateway", vec![], false);
        assert!(all.selected("anything"));
    }
}
