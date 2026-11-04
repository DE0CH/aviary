//! The gate's socket check (DESIGN 9.4 condition 1, cutover 7.5 and 7.9 `allow-handover`): every ui pod's switch
//! module report (`GET /__jarvis/sockets` of the old router, through the old API's `pods/proxy`, bridge B7), each
//! generated in this read, with no first-hop socket that is not LU (an opaque one included) and no HTTP in flight that
//! is not LU. A pod listed by the API (Terminating ones included) whose report is missing, unreadable or stale fails
//! the check: nothing is released while the old path may still carry a person's writes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::kube::Kube;

/// How far a report's `generatedAt` may stand from this process's clock and still count as generated in this read
/// (the pods' clocks and the importer's are NTP-synced; the read itself takes at most [`crate::kube::EXEC_TIMEOUT`]).
pub const FRESH: time::Duration = time::Duration::seconds(15);

/// The route of the report.
pub const ROUTE: &str = "/__jarvis/sockets";

/// One person's part of a report (only what the check reads).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonPart {
    /// Their first-hop sockets that are not LU.
    pub sockets: Vec<serde_json::Value>,
    /// Their in-flight HTTP that is not LU.
    pub http: u64,
}

/// The totals of a report.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    /// First-hop sockets that are not LU (opaque ones included).
    pub sockets: u64,
    /// Opaque first-hop sockets, LU ones included.
    pub opaque: u64,
    /// LU sockets.
    pub lu: u64,
    /// Opaque LU sockets.
    #[serde(rename = "luOpaque")]
    pub lu_opaque: u64,
    /// In-flight HTTP that is not LU.
    pub http: u64,
}

/// One pod's report.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// When the router made it.
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    /// By person.
    #[serde(default)]
    pub persons: BTreeMap<String, PersonPart>,
    /// Totals.
    pub totals: Totals,
}

/// One pod's outcome in a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PodCheck {
    /// The pod.
    pub pod: String,
    /// Being deleted.
    pub terminating: bool,
    /// Why it fails the check; `None`: clean.
    pub problem: Option<String>,
    /// Its LU sockets.
    pub lu: u64,
    /// Its sockets that are not LU.
    pub old_path: u64,
}

/// A gate check over every ui pod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// When this process started the read.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The pods could be listed.
    pub listed: bool,
    /// Every pod.
    pub pods: Vec<PodCheck>,
}

impl Check {
    /// Every pod listed, read, fresh and clean (for `person` only, when given).
    #[must_use]
    pub fn clean(&self) -> bool {
        self.listed && self.pods.iter().all(|p| p.problem.is_none())
    }

    /// The problems, one line per pod.
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .pods
            .iter()
            .filter_map(|p| p.problem.as_ref().map(|w| format!("{}: {w}", p.pod)))
            .collect();
        if !self.listed {
            out.push("the ui pods could not be listed".to_owned());
        }
        out
    }

    /// LU sockets over every pod.
    #[must_use]
    pub fn lu(&self) -> u64 {
        self.pods.iter().map(|p| p.lu).sum()
    }

    /// Sockets that are not LU over every pod.
    #[must_use]
    pub fn old_path(&self) -> u64 {
        self.pods.iter().map(|p| p.old_path).sum()
    }
}

/// Judges one pod's answer (`status`, `body`) read at `at`: for every person, or for `person` only.
#[must_use]
pub fn judge(status: u16, body: &str, at: OffsetDateTime, person: Option<&str>) -> (Option<String>, u64, u64) {
    if status != 200 {
        return (Some(format!("the report answered {status}")), 0, 0);
    }
    let r: Report = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return (Some(format!("the report does not parse: {e}")), 0, 0),
    };
    let (lu, old) = (r.totals.lu, r.totals.sockets);
    if (r.generated_at - at).abs() > FRESH {
        return (Some(format!("stale report (generated {})", r.generated_at)), lu, old);
    }
    let problem = match person {
        None => {
            let opaque_old = r.totals.opaque.saturating_sub(r.totals.lu_opaque);
            if r.totals.sockets > 0 || opaque_old > 0 {
                Some(format!(
                    "{} socket(s) not LU ({opaque_old} opaque)",
                    r.totals.sockets.max(opaque_old)
                ))
            } else if r.totals.http > 0 {
                Some(format!("{} HTTP request(s) not LU in flight", r.totals.http))
            } else {
                None
            }
        }
        Some(p) => match r.persons.get(p) {
            Some(e) if !e.sockets.is_empty() => Some(format!("{} socket(s) of the person not LU", e.sockets.len())),
            Some(e) if e.http > 0 => Some(format!("{} HTTP request(s) of the person not LU in flight", e.http)),
            _ => None,
        },
    };
    (problem, lu, old)
}

/// Reads every ui pod's report, all at once, each within [`crate::kube::EXEC_TIMEOUT`].
pub async fn check(kube: &Kube, token: &str, person: Option<&str>) -> Check {
    let at = OffsetDateTime::now_utc();
    let pods = match kube.ui_pods().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "ui pods not listed for the gate");
            return Check {
                at,
                listed: false,
                pods: Vec::new(),
            };
        }
    };
    let pods = futures_util::future::join_all(pods.iter().map(|p| async move {
        let (problem, lu, old_path) = match kube
            .proxy(reqwest::Method::GET, &p.name, ROUTE, token, crate::kube::EXEC_TIMEOUT)
            .await
        {
            Ok((status, body)) => judge(status, &body, at, person),
            Err(e) => (Some(format!("no report: {e}")), 0, 0),
        };
        PodCheck {
            pod: p.name.clone(),
            terminating: p.terminating,
            problem,
            lu,
            old_path,
        }
    }))
    .await;
    Check { at, listed: true, pods }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(at: OffsetDateTime, totals: &str, persons: &str) -> String {
        format!(
            "{{\"generatedAt\":\"{}\",\"armed\":true,\"persons\":{persons},\"totals\":{totals},\"counters\":{{}}}}",
            at.format(&time::format_description::well_known::Rfc3339).unwrap()
        )
    }

    const CLEAN: &str = "{\"sockets\":0,\"opaque\":1,\"lu\":3,\"luOpaque\":1,\"http\":0,\"passOnInFlight\":0,\
                         \"hopSockets\":0,\"hopOpaque\":0}";

    #[test]
    fn only_a_fresh_report_with_nothing_but_lu_is_clean() {
        let now = OffsetDateTime::now_utc();
        assert_eq!(judge(200, &body(now, CLEAN, "{}"), now, None), (None, 3, 0));
        let stale = judge(200, &body(now - time::Duration::seconds(60), CLEAN, "{}"), now, None);
        assert!(stale.0.unwrap().starts_with("stale"));
        let sock = CLEAN.replace("\"sockets\":0", "\"sockets\":1");
        assert!(judge(200, &body(now, &sock, "{}"), now, None).0.is_some());
        let opaque = CLEAN.replace("\"opaque\":1", "\"opaque\":2");
        assert!(
            judge(200, &body(now, &opaque, "{}"), now, None)
                .0
                .unwrap()
                .contains("opaque")
        );
        let http = CLEAN.replace("\"http\":0", "\"http\":2");
        assert!(
            judge(200, &body(now, &http, "{}"), now, None)
                .0
                .unwrap()
                .contains("HTTP")
        );
        assert!(judge(404, "", now, None).0.is_some());
        assert!(judge(200, "nope", now, None).0.is_some());
    }

    #[test]
    fn a_person_gate_reads_that_person_only() {
        let now = OffsetDateTime::now_utc();
        let sock = CLEAN.replace("\"sockets\":0", "\"sockets\":1");
        let persons = "{\"a@x\":{\"sockets\":[{\"id\":1}],\"http\":0},\"b@x\":{\"sockets\":[],\"http\":0}}";
        let b = body(now, &sock, persons);
        assert!(judge(200, &b, now, Some("a@x")).0.is_some());
        assert!(judge(200, &b, now, Some("b@x")).0.is_none());
        assert!(judge(200, &b, now, Some("c@x")).0.is_none());
        assert!(judge(200, &b, now, None).0.is_some());
    }
}
