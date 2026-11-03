//! Running sets (cutover 6.3): which old conversations are running, read inside the old ui pods. The importer runs
//! [`SCRIPT`] (`holds.js`) in every `app=ui` pod's container `ui` (Terminating pods included) as `node -e`; the script
//! asks every `CloudCLI` child of the old router for its running list and prints one JSON line per child. A schema
//! with a child that did not answer is unknown, and every conversation of it counts as running (fail-closed); a
//! pod whose read failed as a whole makes the read unusable ([`RunningSet::complete`] false).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The script, a constant of the importer (its route and the child environment come from the pinned ui image).
pub const SCRIPT: &str = include_str!("holds.js");

/// One child's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    /// `cc_u…`.
    pub schema: String,
    /// Its port in the pod.
    pub port: u16,
    /// It answered 200 with a list.
    pub ok: bool,
    /// The app session ids (conversation ids) it runs.
    pub sessions: Vec<String>,
}

/// Parses `holds.js`'s output.
///
/// # Errors
/// [`Error::Source`] for a line that is not a child's answer.
pub fn parse(out: &str) -> Result<Vec<Child>> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| Error::Source(format!("holds.js printed {l:?}: {e}"))))
        .collect()
}

/// One schema in a running-set read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Schema {
    /// Every child of it answered.
    pub ok: bool,
    /// The sessions its children run.
    pub sessions: BTreeSet<String>,
}

/// One running-set read over every ui pod.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RunningSet {
    /// By schema.
    pub schemas: BTreeMap<String, Schema>,
    /// Every pod's read succeeded.
    pub complete: bool,
    /// Pods read.
    pub pods: usize,
}

impl RunningSet {
    /// A read of `pods` pods, each `Ok(children)` or a failure.
    #[must_use]
    pub fn of(pods: Vec<Result<Vec<Child>>>) -> Self {
        let mut set = RunningSet {
            schemas: BTreeMap::new(),
            complete: true,
            pods: pods.len(),
        };
        for p in pods {
            let Ok(children) = p else {
                set.complete = false;
                continue;
            };
            for c in children {
                let s = set.schemas.entry(c.schema).or_insert(Schema {
                    ok: true,
                    sessions: BTreeSet::new(),
                });
                s.ok &= c.ok;
                s.sessions.extend(c.sessions);
            }
        }
        set
    }

    /// Whether conversation `id` of `schema` may be running by this read: listed, or its schema unknown (a child that
    /// did not answer), or the read incomplete. A schema with no child at all runs nothing.
    #[must_use]
    pub fn may_run(&self, schema: &str, id: &str) -> bool {
        if !self.complete {
            return true;
        }
        self.schemas
            .get(schema)
            .is_some_and(|s| !s.ok || s.sessions.contains(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(schema: &str, ok: bool, sessions: &[&str]) -> Child {
        Child {
            schema: schema.into(),
            port: 3001,
            ok,
            sessions: sessions.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn a_schema_with_a_silent_child_runs_everything_and_a_failed_pod_spoils_the_read() {
        let set = RunningSet::of(vec![
            Ok(vec![child("cc_a", true, &["x"]), child("cc_b", false, &[])]),
            Ok(vec![child("cc_a", true, &["y"])]),
        ]);
        assert!(set.may_run("cc_a", "x") && set.may_run("cc_a", "y"));
        assert!(!set.may_run("cc_a", "z"));
        assert!(set.may_run("cc_b", "anything"));
        assert!(!set.may_run("cc_c", "z"), "no child: nothing runs");
        let spoiled = RunningSet::of(vec![Ok(vec![]), Err(Error::Source("exec failed".into()))]);
        assert!(spoiled.may_run("cc_c", "z"));
    }

    #[test]
    fn output_lines_parse() {
        let out = "{\"schema\":\"cc_a\",\"port\":3001,\"ok\":true,\"sessions\":[\"x\"]}\n\n";
        assert_eq!(parse(out).unwrap(), vec![child("cc_a", true, &["x"])]);
        assert!(parse("oops\n").is_err());
    }
}
