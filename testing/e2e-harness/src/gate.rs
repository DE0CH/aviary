//! claude-code-gate (supervisor 22, magpie 20; nightly.yml): one side of the gate. Runs the harness's `cc`, `e21`
//! and `e22` scenarios with the sandbox image `<image>` and writes `<out>/gate.json`:
//! `{"claudeCode": "<version>", "unclassified": [top-level names], "unanswered": [opcodes], "counts": <the cc
//! scenario's cc-counts: magpie's events by kind and the rows by kind>}` from the sandbox Pods' supervisor logs
//! (`unclassified path`, `unanswered opcode`) and the driver's output. The harness is left up for the artifacts step;
//! `down` ends it. `GATE_HARNESS_UP=1`: it is already up with `<image>`.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::sh::cmd;
use crate::{Env, knob};

/// The distinct values of `field` in the supervisor's JSON log lines whose `msg` is `msg`.
fn field_of(log: &str, msg: &str, field: &str) -> Value {
    let set: BTreeSet<String> = log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["msg"] == msg)
        .filter_map(|v| match &v[field] {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        })
        .collect();
    json!(set.into_iter().collect::<Vec<_>>())
}

pub fn run(e: &Env, sha12: &str, image: &str, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let version = cmd(
        "docker",
        &[
            "run",
            "--rm",
            "--entrypoint",
            "/opt/claude-code/bin/claude",
            image,
            "--version",
        ],
    )
    .run(300)?
    .split_whitespace()
    .next()
    .unwrap_or_default()
    .to_owned();
    eprintln!("claude-code-gate: Claude Code {version} in {image}");
    if knob("GATE_HARNESS_UP").as_deref() != Some("1") {
        let names = [
            format!("pigeon:{sha12}"),
            format!("magpie:{sha12}"),
            format!("hermit:{sha12}"),
            format!("aviary-testing:{sha12}"),
            image.to_owned(),
        ];
        crate::up(e, [&names[0], &names[1], &names[2], &names[3], &names[4]])?;
    }
    // the driver's output (what `test` prints) into driver.log
    let exe = std::env::current_exe().context("the harness's own path")?;
    let driver = cmd(&exe.to_string_lossy(), &["test", "cc", "e21", "e22"]).output_within(2600)?;
    let log = [driver.stdout.clone(), driver.stderr.clone()].concat();
    std::fs::write(out.join("driver.log"), &log)?;
    let text = String::from_utf8_lossy(&log).into_owned();
    let shown: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with("in ") && !l.starts_with("out "))
        .collect();
    for l in &shown[shown.len().saturating_sub(40)..] {
        eprintln!("{l}");
    }
    // every sandbox Pod's log, right after the scenarios (before an idle release removes them)
    let mut sandboxes = String::new();
    let pods = e
        .k(&["get", "pods", "-l", "app=sandbox", "-o", "name"])
        .run(60)
        .unwrap_or_default();
    for p in pods.lines() {
        if let Ok(o) = e.k(&["logs", p, "--all-containers", "--tail=-1"]).output_within(120) {
            sandboxes.push_str(&String::from_utf8_lossy(&o.stdout));
            sandboxes.push_str(&String::from_utf8_lossy(&o.stderr));
        }
    }
    std::fs::write(out.join("sandboxes.log"), &sandboxes)?;
    let counts = text
        .lines()
        .filter_map(|l| l.strip_prefix("cc-counts "))
        .next_back()
        .and_then(|c| serde_json::from_str::<Value>(c).ok())
        .unwrap_or(Value::Null);
    let gate = json!({
        "claudeCode": version,
        "unclassified": field_of(&sandboxes, "unclassified path", "path"),
        "unanswered": field_of(&sandboxes, "unanswered opcode", "opcode"),
        "counts": counts,
    });
    let pretty = serde_json::to_string_pretty(&gate)?;
    std::fs::write(out.join("gate.json"), format!("{pretty}\n"))?;
    eprintln!("{pretty}");
    if !driver.status.success() {
        bail!("claude-code-gate: the scenarios failed with Claude Code {version}");
    }
    if gate["counts"].is_null() {
        bail!("claude-code-gate: no cc-counts in the driver's output");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::field_of;

    #[test]
    fn the_distinct_values_of_a_logged_field() {
        let log = r#"{"msg":"unclassified path","path":"claude/a"}
{"msg":"unclassified path","path":"claude/b"}
{"msg":"unclassified path","path":"claude/a"}
{"msg":"unanswered opcode","opcode":"UNKNOWN"}
not json
"#;
        assert_eq!(
            field_of(log, "unclassified path", "path"),
            serde_json::json!(["claude/a", "claude/b"])
        );
        assert_eq!(
            field_of(log, "unanswered opcode", "opcode"),
            serde_json::json!(["UNKNOWN"])
        );
        assert_eq!(field_of(log, "nothing", "path"), serde_json::json!([]));
    }
}
