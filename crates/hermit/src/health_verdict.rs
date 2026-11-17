//! The verdict over the snapshot's "now" half (hermit 18.6, ported from jarvis3 `healthOf`, A3.F39): levels
//! `ok < warming < watch < blind < problem`; the state is the worst finding; findings are listed worst first.
//!
//! What each level means (the flight deck's colours; Deyao, 2026-11-17):
//! - `problem` (red): a service is lost right now, for everyone or for one person: no pod of a Deployment ready, the
//!   gateway gone, no egress targets, the budget used up, a host with sandboxes on it not answering, a sandbox stuck
//!   starting.
//! - `blind` (magenta): a section of the snapshot could not be read, so what it covers may be fine or down.
//! - `watch` (amber): still serving, but degraded or worth a look: a Deployment short of pods whose others serve, a
//!   hermit replica down beside a healthy one, a host that boots slowly or is empty and silent, the scaler's pass
//!   late, the catalogue refused (the last good one is used), an error that is over (the counts of the last minutes),
//!   the budget near its limit, an operator's switch.
//! - `warming` (blue): something that finishes by itself: a rollout, a host booting, a sandbox starting.

use serde::Serialize;
use serde_json::Value;

/// A host still booting after this long needs a look (watch) rather than warming up.
pub const HOST_BOOT_SLOW_S: f64 = 300.0;
/// A sandbox still starting after this long is a problem.
pub const SANDBOX_START_SLOW_S: f64 = 180.0;
/// The scaler leader's last pass older than this needs a look (watch).
pub const SCALER_STALE_S: f64 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warming,
    Watch,
    Blind,
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
        // one failed read can fill several sections (egress's summary feeds egress, budget and recent): one finding
        // per message, naming every section it emptied
        let mut by_msg: Vec<(&str, Vec<&str>)> = Vec::new();
        for (name, msg) in errors {
            let msg = msg.as_str().unwrap_or("?");
            match by_msg.iter_mut().find(|(m, _)| *m == msg) {
                Some((_, names)) => names.push(name),
                None => by_msg.push((msg, vec![name])),
            }
        }
        for (msg, names) in by_msg {
            add(
                Level::Blind,
                "page",
                format!("Could not read {}: {msg}", names.join(", ")),
            );
        }
    }
    let svc = &d["service"];
    if svc["available"] == true {
        for x in list(svc, &["deployments"]) {
            let name = x["name"].as_str().unwrap_or("?");
            if x["missing"] == true {
                add(Level::Problem, "service", format!("{name}: no Deployment"));
            } else if x["state"] == "problem" || x["state"] == "watch" {
                // watch: fewer pods ready than asked, the ready ones serve; problem: none ready
                add(
                    if x["state"] == "watch" {
                        Level::Watch
                    } else {
                        Level::Problem
                    },
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
                // capacity is late; a start that fails because of it is its own finding
                add(
                    Level::Watch,
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
            // someone's sandbox is on it: they are stopped now; an empty host stops nobody
            let n = h["sandboxes"].as_u64().unwrap_or(0);
            if n > 0 {
                add(
                    Level::Problem,
                    "host",
                    format!(
                        "Host {name} not answering ({} on it)",
                        plural(n, "sandbox", "sandboxes")
                    ),
                );
            } else {
                add(
                    Level::Watch,
                    "host",
                    format!("Host {name} not answering (no sandbox on it)"),
                );
            }
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
    // errors of the last minutes are over: a caution, never a service lost now
    let r = &d["recent"];
    let n = |k: &str| r[k].as_u64().unwrap_or(0);
    if n("failedStarts") > 0 {
        add(
            Level::Watch,
            "errors",
            format!(
                "{} failed in the last {recent_minutes} min",
                plural(n("failedStarts"), "sandbox start", "sandbox starts")
            ),
        );
    }
    if n("modelErrors") > 0 {
        add(
            Level::Watch,
            "errors",
            format!(
                "{} failed in the last {recent_minutes} min",
                plural(n("modelErrors"), "model or route call", "model or route calls")
            ),
        );
    }
    if n("hostErrors") > 0 {
        add(
            Level::Watch,
            "errors",
            format!(
                "{} in the last {recent_minutes} min",
                plural(n("hostErrors"), "host or move error", "host or move errors")
            ),
        );
    }
    if let Some(why) = d["catalogue"]["refused"].as_str() {
        add(
            Level::Watch,
            "catalogue",
            format!("The staff catalogue is refused (the last good one is used): {why}"),
        );
    }
    // a fabricOps that could not be read is an `errors` finding above; with a gateway answering, nothing known of
    // its egress targets or of the scaler's last pass is a problem too (the leader never ran a pass, or the
    // gateway reports nothing)
    let fo = &d["fabricOps"];
    let gateway_live = list(d, &["hosts", "list"])
        .iter()
        .any(|h| h["gateway"] == true && h["reachable"] == true);
    if fo["egressTargets"] == 0 {
        add(Level::Problem, "egress", "The gateway has no egress targets".into());
    } else if fo.is_object() && gateway_live && fo["egressTargets"].is_null() {
        add(Level::Problem, "egress", "The gateway reports no egress targets".into());
    }
    if let Some(a) = age(&fo["lastPassAt"]) {
        if a > SCALER_STALE_S {
            // running sandboxes go on; only new hosts are not made
            add(
                Level::Watch,
                "scaler",
                format!("The scaler's last pass was {} ago", secs(a)),
            );
        }
    } else if fo.is_object() && gateway_live {
        add(Level::Watch, "scaler", "The scaler has not reported a pass".into());
    }
    // a hermit replica down or without its store beside a healthy one is a caution (the healthy one serves); with no
    // healthy replica it is a problem
    let healthy = |r: &Value| r["reporting"] != false && r["storeError"].as_str().is_none();
    let replica_level = if list(d, &["replicas"]).iter().any(healthy) {
        Level::Watch
    } else {
        Level::Problem
    };
    for rep in list(d, &["replicas"]) {
        let pod = rep["pod"].as_str().unwrap_or("?");
        if rep["reporting"] == false {
            let seen = age(&rep["seenAt"]).map_or_else(|| "never".to_owned(), |s| format!("{} ago", secs(s)));
            add(
                replica_level,
                "workspaces",
                format!("hermit {pod} stopped reporting (last seen {seen}): its workspace mount may hang"),
            );
            continue;
        }
        if let Some(e) = rep["storeError"].as_str() {
            add(
                replica_level,
                "workspaces",
                format!("Workspace store not mounted in hermit {pod}: {e}"),
            );
        }
        if let Some(a) = age(&rep["deleteRefusedAt"])
            && a <= f64::from(recent_minutes) * 60.0
        {
            add(
                Level::Watch,
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
            Level::Watch
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
            Level::Blind
        );
        // one egress failure that emptied three sections is one finding
        let texts: Vec<String> = health_of(
            &json!({"at": at(), "errors": {"budget": "timeout", "egress": "timeout", "recent": "timeout", "hosts": "db"}}),
            0.8,
            15,
        )
        .findings
        .into_iter()
        .filter(|f| f.about == "page")
        .map(|f| f.text)
        .collect();
        assert_eq!(
            texts,
            [
                "Could not read budget, egress, recent: timeout",
                "Could not read hosts: db"
            ]
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
        assert_eq!(state(&json!({"at": at(), "recent": {"failedStarts": 1}})), Level::Watch);
        assert_eq!(
            state(&json!({"at": at(), "recent": {"blocked": 3}})),
            Level::Ok,
            "blocked internet requests are normal"
        );
        assert_eq!(
            state(&json!({"at": at(), "catalogue": {"refused": "admin not in users"}})),
            Level::Watch
        );
        assert_eq!(
            state(&json!({"at": at(), "fabricOps": {"egressTargets": 0}})),
            Level::Problem
        );
        assert_eq!(
            state(&json!({"at": at(), "fabricOps": {"lastPassAt": ago(45)}})),
            Level::Watch
        );
        assert_eq!(
            state(&json!({"at": at(), "fabricOps": {"lastPassAt": ago(5), "egressTargets": 2}})),
            Level::Ok
        );
        // nothing known with a gateway answering: no pass yet, no targets reported
        let live = json!({"list": [gw]});
        let f = health_of(
            &json!({"at": at(), "hosts": live, "fabricOps": {"lastPassAt": null, "egressTargets": null}}),
            0.8,
            15,
        );
        let texts: Vec<&str> = f.findings.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "The gateway reports no egress targets",
                "The scaler has not reported a pass"
            ]
        );
        assert_eq!(
            state(&json!({"at": at(), "fabricOps": {"lastPassAt": null, "egressTargets": null}})),
            Level::Ok,
            "no gateway answering: the gateway finding says it"
        );
        assert_eq!(
            state(&json!({"at": at(), "hosts": live, "fabricOps": null, "errors": {"fabricOps": "timeout"}})),
            Level::Blind
        );
        assert_eq!(
            state(&json!({"at": at(), "replicas": [{"pod": "hermit-a", "storeError": "ENOTCONN"}]})),
            Level::Problem
        );
        assert_eq!(
            state(
                &json!({"at": at(), "replicas": [{"pod": "hermit-a", "deleteRefusedAt": ago(60), "deleteRefusedPath": "ws/x"}]})
            ),
            Level::Watch
        );
        assert_eq!(
            health_of(
                &json!({"at": at(), "replicas": [{"pod": "hermit-a", "reporting": false, "seenAt": ago(120)}]}),
                0.8,
                15
            )
            .findings[0]
                .text,
            "hermit hermit-a stopped reporting (last seen 2 min ago): its workspace mount may hang"
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
        assert_eq!(levels, vec![Level::Watch, Level::Watch, Level::Warming]);
        assert_eq!(mixed.findings[0].text, "2 host or move errors in the last 15 min");
    }

    /// Deyao, 2026-11-17: red (`problem`) is a service lost right now. A replica down beside a healthy one, an error
    /// that is over and a thing near its limit are cautions (`watch`); a section that could not be read is `blind`,
    /// its own level between the two.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn problem_is_a_service_lost_now() {
        let dep = |state: &str, ready: u64| json!({"available": true, "deployments": [{"name": "pigeon", "state": state, "desired": 2, "ready": ready, "updated": 2}]});
        // one of two pods ready: the other serves
        let short = health_of(&json!({"at": at(), "service": dep("watch", 1)}), 0.8, 15);
        assert_eq!(short.state, Level::Watch);
        assert_eq!(short.findings[0].text, "pigeon: 1 of 2 pods ready");
        // none ready: the service is down
        assert_eq!(
            state(&json!({"at": at(), "service": dep("problem", 0)})),
            Level::Problem
        );

        // a host that stopped answering: red with someone's sandbox on it, a caution when empty
        let host = |n: u64| json!({"name": "aws-1", "status": "active", "reachable": false, "sandboxes": n, "launchedAt": ago(3600)});
        let busy = health_of(&json!({"at": at(), "hosts": {"list": [host(2)]}}), 0.8, 15);
        assert_eq!(busy.state, Level::Problem);
        assert_eq!(busy.findings[0].text, "Host aws-1 not answering (2 sandboxes on it)");
        let empty = health_of(&json!({"at": at(), "hosts": {"list": [host(0)]}}), 0.8, 15);
        assert_eq!(empty.state, Level::Watch);
        assert_eq!(empty.findings[0].text, "Host aws-1 not answering (no sandbox on it)");
        // one person stopped right now is a service lost: a sandbox stuck starting stays red
        assert_eq!(
            state(&json!({"at": at(), "sandboxes": {"list": [{"status": "starting", "ageS": 200, "owner": "a@b"}]}})),
            Level::Problem
        );

        // errors that are over
        for k in ["failedStarts", "modelErrors", "hostErrors"] {
            assert_eq!(state(&json!({"at": at(), "recent": {k: 3}})), Level::Watch, "{k}");
        }
        // the scaler never reported: running sandboxes go on
        let gw = json!({"name": "gw", "gateway": true, "reachable": true, "status": "active", "launchedAt": at(), "lastSeenAt": at()});
        assert_eq!(
            state(&json!({"at": at(), "hosts": {"list": [gw]}, "fabricOps": {"lastPassAt": null, "egressTargets": 2}})),
            Level::Watch
        );
        // no egress targets: no sandbox reaches a model
        assert_eq!(
            state(&json!({"at": at(), "fabricOps": {"lastPassAt": ago(5), "egressTargets": 0}})),
            Level::Problem
        );

        // hermit's replicas: one down or without its store beside a healthy one is a caution; with none healthy, red
        let ok = json!({"pod": "hermit-b"});
        let silent = json!({"pod": "hermit-a", "reporting": false, "seenAt": ago(120)});
        let unmounted = json!({"pod": "hermit-a", "storeError": "ENOTCONN"});
        assert_eq!(state(&json!({"at": at(), "replicas": [silent, ok]})), Level::Watch);
        assert_eq!(state(&json!({"at": at(), "replicas": [unmounted, ok]})), Level::Watch);
        assert_eq!(state(&json!({"at": at(), "replicas": [silent]})), Level::Problem);
        assert_eq!(state(&json!({"at": at(), "replicas": [unmounted]})), Level::Problem);
        assert_eq!(
            state(&json!({"at": at(), "replicas": [silent, unmounted]})),
            Level::Problem
        );

        // blind: its own level, worse than a caution, never a problem; it serialises as "blind"
        let blind = health_of(
            &json!({"at": at(), "errors": {"hosts": "db"}, "recent": {"failedStarts": 1}, "service": dep("warming", 2)}),
            0.8,
            15,
        );
        let levels: Vec<Level> = blind.findings.iter().map(|f| f.level).collect();
        assert_eq!(levels, vec![Level::Blind, Level::Watch, Level::Warming]);
        assert_eq!(blind.state, Level::Blind);
        assert_eq!(serde_json::to_value(&blind).unwrap()["state"], "blind");
        assert!(Level::Watch < Level::Blind && Level::Blind < Level::Problem);
        // a real loss still outranks it
        assert_eq!(
            state(&json!({"at": at(), "errors": {"hosts": "db"}, "gatewayMissing": true})),
            Level::Problem
        );
    }
}
