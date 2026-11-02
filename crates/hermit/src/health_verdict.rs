//! The verdict over the snapshot's "now" half (hermit 18.6, ported from jarvis3 `healthOf`, A3.F39): levels
//! `ok < warming < watch < problem`; the state is the worst finding; findings are listed worst first.

use serde::Serialize;
use serde_json::Value;

/// A host still booting after this long is a problem rather than warming up.
pub const HOST_BOOT_SLOW_S: f64 = 300.0;
/// A sandbox still starting after this long is a problem.
pub const SANDBOX_START_SLOW_S: f64 = 180.0;
/// The scaler leader's last pass older than this is a problem.
pub const SCALER_STALE_S: f64 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warming,
    Watch,
    Problem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub level: Level,
    /// What it is about: `page`, `service`, `gateway`, `host`, `sandbox`, `budget`, `errors`, `catalogue`,
    /// `egress`, `scaler`, `workspaces`, `operator`.
    pub about: &'static str,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Health {
    pub state: Level,
    pub findings: Vec<Finding>,
}

fn secs(s: f64) -> String {
    let s = s.max(0.0).round();
    if s < 90.0 {
        format!("{s} s")
    } else if s < 5400.0 {
        format!("{} min", (s / 60.0).round())
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

fn ms_of(v: &Value) -> Option<i64> {
    v.as_str()
        .and_then(|s| hermit_api::Timestamp::parse(s).ok())
        .map(hermit_api::Timestamp::unix_millis)
}

fn list<'a>(v: &'a Value, path: &[&str]) -> &'a [Value] {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_array().map_or(&[], Vec::as_slice)
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The verdict of a snapshot `d` (its `at` is "now"). `budget_watch` is `ops.budgetWatch` (0.8);
/// `recent_minutes` the window of the error counts (15).
#[must_use]
#[allow(clippy::too_many_lines)] // one rule per line of 18.6, kept in its order
#[allow(clippy::cast_precision_loss)]
pub fn health_of(d: &Value, budget_watch: f64, recent_minutes: u32) -> Health {
    let mut out: Vec<Finding> = Vec::new();
    let mut add = |level: Level, about: &'static str, text: String| out.push(Finding { level, about, text });
    let now = ms_of(&d["at"]).unwrap_or(0);
    let age = |v: &Value| ms_of(v).map(|t| (now - t) as f64 / 1000.0);

    if let Some(errors) = d["errors"].as_object() {
        for (name, msg) in errors {
            add(
                Level::Problem,
                "page",
                format!("Could not read {name}: {}", msg.as_str().unwrap_or("?")),
            );
        }
    }
    let svc = &d["service"];
    if svc["available"] == true {
        for x in list(svc, &["deployments"]) {
            let name = x["name"].as_str().unwrap_or("?");
            if x["missing"] == true {
                add(Level::Problem, "service", format!("{name}: no Deployment"));
            } else if x["state"] == "problem" {
                add(
                    Level::Problem,
                    "service",
                    format!("{name}: {} of {} pods ready", x["ready"], x["desired"]),
                );
            } else if x["state"] == "warming" {
                add(
                    Level::Warming,
                    "service",
                    format!(
                        "{name}: rolling out ({} of {} updated, {} ready)",
                        x["updated"], x["desired"], x["ready"]
                    ),
                );
            }
        }
    }
    for h in list(d, &["hosts", "list"]) {
        let name = h["name"].as_str().unwrap_or("?");
        let up = age(&h["launchedAt"]).unwrap_or(0.0);
        let status = h["status"].as_str().unwrap_or("");
        if h["gateway"] == true {
            if h["reachable"] != true {
                let seen = age(&h["lastSeenAt"]).map_or_else(|| "never".to_owned(), |s| format!("{} ago", secs(s)));
                add(
                    Level::Problem,
                    "gateway",
                    format!("Gateway {name} not answering (last seen {seen})"),
                );
            }
        } else if status == "booting" {
            if up > HOST_BOOT_SLOW_S {
                add(
                    Level::Problem,
                    "host",
                    format!("Host {name} still booting after {}", secs(up)),
                );
            } else {
                add(Level::Warming, "host", format!("Host {name} booting ({})", secs(up)));
            }
        } else if status == "retiring" {
            add(
                Level::Warming,
                "host",
                format!("Host {name} retiring: its sandboxes move off, then it is terminated"),
            );
        } else if h["reachable"] != true {
            add(Level::Problem, "host", format!("Host {name} not answering"));
        }
    }
    if d["gatewayMissing"] == true {
        add(Level::Problem, "gateway", "No gateway host is registered".into());
    }
    for s in list(d, &["sandboxes", "list"]) {
        let owner = s["owner"].as_str().unwrap_or("?");
        let a = s["ageS"].as_f64().unwrap_or(0.0);
        if s["status"] == "starting" {
            if a > SANDBOX_START_SLOW_S {
                add(
                    Level::Problem,
                    "sandbox",
                    format!("Sandbox of {owner} still starting after {}", secs(a)),
                );
            } else {
                add(
                    Level::Warming,
                    "sandbox",
                    format!("Sandbox of {owner} starting ({})", secs(a)),
                );
            }
        } else if s["status"] == "ready" && s["reporting"] == false {
            add(
                Level::Watch,
                "sandbox",
                format!("Sandbox of {owner} not reporting (its supervisor still answers)"),
            );
        }
    }
    let st = &d["budget"]["status"];
    if st.is_object() {
        let spent = st["spentUsd"].as_f64().unwrap_or(0.0);
        let limit = st["limitUsd"].as_f64();
        if st["exhausted"] == true {
            add(
                Level::Problem,
                "budget",
                format!(
                    "Budget used up: USD {spent:.2} of {}; model calls are refused",
                    limit.map_or_else(|| "?".into(), |l| format!("{l:.2}"))
                ),
            );
        } else if let Some(l) = limit.filter(|l| *l > 0.0)
            && spent / l >= budget_watch
        {
            add(
                Level::Watch,
                "budget",
                format!(
                    "Budget {}% used (USD {spent:.2} of {l:.2})",
                    ((spent / l) * 100.0).round()
                ),
            );
        }
    }
    let r = &d["recent"];
    let n = |k: &str| r[k].as_u64().unwrap_or(0);
    if n("failedStarts") > 0 {
        add(
            Level::Problem,
            "errors",
            format!(
                "{} failed in the last {recent_minutes} min",
                plural(n("failedStarts"), "sandbox start", "sandbox starts")
            ),
        );
    }
    if n("modelErrors") > 0 {
        add(
            Level::Problem,
            "errors",
            format!(
                "{} failed in the last {recent_minutes} min",
                plural(n("modelErrors"), "model or route call", "model or route calls")
            ),
        );
    }
    if n("hostErrors") > 0 {
        add(
            Level::Problem,
            "errors",
            format!(
                "{} in the last {recent_minutes} min",
                plural(n("hostErrors"), "host or move error", "host or move errors")
            ),
        );
    }
    if let Some(why) = d["catalogue"]["refused"].as_str() {
        add(
            Level::Problem,
            "catalogue",
            format!("The staff catalogue is refused (the last good one is used): {why}"),
        );
    }
    if d["fabricOps"]["egressTargets"] == 0 {
        add(Level::Problem, "egress", "The gateway has no egress targets".into());
    }
    if let Some(a) = age(&d["fabricOps"]["lastPassAt"])
        && a > SCALER_STALE_S
    {
        add(
            Level::Problem,
            "scaler",
            format!("The scaler's last pass was {} ago", secs(a)),
        );
    }
    for rep in list(d, &["replicas"]) {
        let pod = rep["pod"].as_str().unwrap_or("?");
        if let Some(e) = rep["storeError"].as_str() {
            add(
                Level::Problem,
                "workspaces",
                format!("Workspace store not mounted in hermit {pod}: {e}"),
            );
        }
        if let Some(a) = age(&rep["deleteRefusedAt"])
            && a <= f64::from(recent_minutes) * 60.0
        {
            add(
                Level::Problem,
                "workspaces",
                format!(
                    "A workspace delete was refused in hermit {pod} ({}): run hermit-jfs-owner",
                    rep["deleteRefusedPath"].as_str().unwrap_or("?")
                ),
            );
        }
    }
    let c = &d["controls"];
    if c["sandboxesEnabled"] == false {
        add(
            Level::Watch,
            "operator",
            format!(
                "Sandboxes are disabled by the operator{}",
                c["disabledReason"]
                    .as_str()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ),
        );
    }
    if let Some(left) = age(&c["freshHostUntil"]).map(|a| -a)
        && left > 0.0
    {
        add(
            Level::Watch,
            "operator",
            format!("A fresh-host test is running ({} left)", secs(left)),
        );
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.level));
    Health {
        state: out.first().map_or(Level::Ok, |f| f.level),
        findings: out,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn at() -> String {
        hermit_api::Timestamp::now().to_string()
    }

    fn ago(s: i64) -> String {
        hermit_api::Timestamp::from_unix_millis(hermit_api::Timestamp::now().unix_millis() - s * 1000)
            .unwrap()
            .to_string()
    }

    fn state(d: &Value) -> Level {
        health_of(d, 0.8, 15).state
    }

    // ported from jarvis3 ops.test.ts "health: all fine, warming, watch, problem", with hermit 18.6's additions
    #[test]
    #[allow(clippy::too_many_lines)]
    fn levels() {
        let svc = |state: &str| json!({"available": true, "deployments": [{"name": "pigeon", "state": state, "desired": 2, "ready": 2, "updated": 2}]});
        let gw = json!({"name": "gw", "gateway": true, "reachable": true, "status": "active", "launchedAt": at(), "lastSeenAt": at()});
        let all =
            json!({"at": at(), "errors": {}, "service": svc("ok"), "hosts": {"list": [gw]}, "sandboxes": {"list": []}});
        assert_eq!(
            health_of(&all, 0.8, 15),
            Health {
                state: Level::Ok,
                findings: vec![]
            }
        );
        let booting = json!({"name": "aws-9", "status": "booting", "reachable": false, "launchedAt": ago(30)});
        assert_eq!(
            state(&json!({"at": at(), "errors": {}, "service": svc("ok"), "hosts": {"list": [gw, booting]}})),
            Level::Warming
        );
        let slow = json!({"name": "aws-9", "status": "booting", "reachable": false, "launchedAt": ago(600)});
        assert_eq!(
            state(&json!({"at": at(), "errors": {}, "hosts": {"list": [gw, slow]}})),
            Level::Problem
        );
        assert_eq!(
            state(
                &json!({"at": at(), "errors": {}, "budget": {"status": {"spentUsd": 85, "limitUsd": 100, "exhausted": false}}})
            ),
            Level::Watch
        );
        assert_eq!(
            state(
                &json!({"at": at(), "errors": {}, "budget": {"status": {"spentUsd": 100, "limitUsd": 100, "exhausted": true}}})
            ),
            Level::Problem
        );
        let down = json!({"name": "gw", "gateway": true, "reachable": false, "status": "active", "launchedAt": at()});
        assert_eq!(
            health_of(&json!({"at": at(), "errors": {}, "hosts": {"list": [down]}}), 0.8, 15).findings[0].about,
            "gateway"
        );
        assert_eq!(
            state(&json!({"at": at(), "errors": {"egress": "timeout"}})),
            Level::Problem
        );
        assert_eq!(
            state(
                &json!({"at": at(), "errors": {}, "sandboxes": {"list": [{"status": "starting", "ageS": 20, "owner": "a@b"}]}})
            ),
            Level::Warming
        );
        assert_eq!(
            state(
                &json!({"at": at(), "errors": {}, "sandboxes": {"list": [{"status": "starting", "ageS": 200, "owner": "a@b"}]}})
            ),
            Level::Problem
        );
        assert_eq!(
            state(
                &json!({"at": at(), "sandboxes": {"list": [{"status": "ready", "reporting": false, "owner": "a@b"}]}})
            ),
            Level::Watch
        );
        assert_eq!(state(&json!({"at": at(), "gatewayMissing": true})), Level::Problem);
        assert_eq!(state(&json!({"at": at(), "service": svc("warming")})), Level::Warming);
        assert_eq!(
            state(
                &json!({"at": at(), "service": {"available": true, "deployments": [{"name": "x", "missing": true}]}})
            ),
            Level::Problem
        );
        assert_eq!(
            state(&json!({"at": at(), "recent": {"failedStarts": 1}})),
            Level::Problem
        );
        assert_eq!(
            state(&json!({"at": at(), "recent": {"blocked": 3}})),
            Level::Ok,
            "blocked internet requests are normal"
        );
        assert_eq!(
            state(&json!({"at": at(), "catalogue": {"refused": "admin not in users"}})),
            Level::Problem
        );
        assert_eq!(
            state(&json!({"at": at(), "fabricOps": {"egressTargets": 0}})),
            Level::Problem
        );
        assert_eq!(
            state(&json!({"at": at(), "fabricOps": {"lastPassAt": ago(45)}})),
            Level::Problem
        );
        assert_eq!(
            state(&json!({"at": at(), "fabricOps": {"lastPassAt": ago(5), "egressTargets": 2}})),
            Level::Ok
        );
        assert_eq!(
            state(&json!({"at": at(), "replicas": [{"pod": "hermit-a", "storeError": "ENOTCONN"}]})),
            Level::Problem
        );
        assert_eq!(
            state(
                &json!({"at": at(), "replicas": [{"pod": "hermit-a", "deleteRefusedAt": ago(60), "deleteRefusedPath": "ws/x"}]})
            ),
            Level::Problem
        );
        assert_eq!(
            state(&json!({"at": at(), "replicas": [{"pod": "hermit-a", "deleteRefusedAt": ago(3600)}]})),
            Level::Ok,
            "a refusal older than the window is history"
        );
        assert_eq!(
            state(&json!({"at": at(), "controls": {"sandboxesEnabled": false}})),
            Level::Watch
        );
        assert_eq!(
            state(&json!({"at": at(), "controls": {"sandboxesEnabled": true, "freshHostUntil": ago(-120)}})),
            Level::Watch
        );
        assert_eq!(
            state(&json!({"at": at(), "controls": {"sandboxesEnabled": true, "freshHostUntil": ago(120)}})),
            Level::Ok
        );
        // worst first
        let mixed = health_of(
            &json!({"at": at(), "service": svc("warming"), "recent": {"hostErrors": 2}, "controls": {"sandboxesEnabled": false}}),
            0.8,
            15,
        );
        let levels: Vec<Level> = mixed.findings.iter().map(|f| f.level).collect();
        assert_eq!(levels, vec![Level::Problem, Level::Watch, Level::Warming]);
        assert_eq!(mixed.findings[0].text, "2 host or move errors in the last 15 min");
    }
}
