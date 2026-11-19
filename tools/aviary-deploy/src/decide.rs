//! Whether main's head may deploy now: the one rule of the deploy run (docs/DEPLOY.md). Only main's head deploys, so an
//! older commit's late run never rolls back a newer one; it deploys once every part's newest run on it is over and
//! green. A part whose workflow did not run on the commit (its paths unchanged) has nothing to wait for.

/// one workflow run on the commit (GitHub's `actions/runs?head_sha=`)
#[derive(Debug, Clone)]
pub struct Run {
    pub id: u64,
    pub workflow: String,
    pub status: String,
    pub conclusion: Option<String>,
    /// a run of main itself (a push, dispatch or schedule on main), not another branch's or a pull request's run that
    /// happens to have the same head commit (a branch the gate fast-forwarded main to has them)
    pub on_main: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ready {
    /// main has moved on: its own head deploys
    Stale {
        head: String,
    },
    /// these workflows' runs are not over
    Wait(Vec<String>),
    /// these workflows' runs ended red
    Fail(Vec<String>),
    Go,
}

/// `workflows`: the parts' workflow names (the deploy run's own is not among them)
pub fn ready(sha: &str, main_head: &str, workflows: &[String], runs: &[Run]) -> Ready {
    if main_head != sha {
        return Ready::Stale {
            head: main_head.to_string(),
        };
    }
    let (mut wait, mut fail) = (Vec::new(), Vec::new());
    for w in workflows {
        let Some(r) = runs
            .iter()
            .filter(|r| r.on_main && &r.workflow == w)
            .max_by_key(|r| r.id)
        else {
            continue;
        };
        if r.status != "completed" {
            wait.push(w.clone());
        } else if !matches!(r.conclusion.as_deref(), Some("success" | "skipped")) {
            fail.push(format!("{w}: {}", r.conclusion.as_deref().unwrap_or("no conclusion")));
        }
    }
    if !fail.is_empty() {
        Ready::Fail(fail)
    } else if !wait.is_empty() {
        Ready::Wait(wait)
    } else {
        Ready::Go
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64, w: &str, status: &str, conclusion: Option<&str>) -> Run {
        Run {
            id,
            workflow: w.into(),
            status: status.into(),
            conclusion: conclusion.map(Into::into),
            on_main: true,
        }
    }
    fn ws() -> Vec<String> {
        vec!["ci".into(), "app".into(), "yyc".into()]
    }

    #[test]
    fn only_mains_head_deploys() {
        let runs = [run(1, "ci", "completed", Some("success"))];
        assert_eq!(ready("a", "b", &ws(), &runs), Ready::Stale { head: "b".into() });
    }

    #[test]
    fn all_green_goes_and_a_part_that_did_not_run_waits_for_nothing() {
        let runs = [
            run(1, "ci", "completed", Some("success")),
            run(2, "app", "completed", Some("skipped")),
        ];
        assert_eq!(ready("a", "a", &ws(), &runs), Ready::Go);
    }

    #[test]
    fn a_run_not_over_waits() {
        let runs = [
            run(1, "ci", "completed", Some("success")),
            run(2, "yyc", "in_progress", None),
        ];
        assert_eq!(ready("a", "a", &ws(), &runs), Ready::Wait(vec!["yyc".into()]));
    }

    #[test]
    fn red_fails_even_while_another_runs() {
        let runs = [
            run(1, "ci", "completed", Some("failure")),
            run(2, "yyc", "queued", None),
        ];
        assert_eq!(ready("a", "a", &ws(), &runs), Ready::Fail(vec!["ci: failure".into()]));
    }

    #[test]
    fn the_newest_run_of_a_workflow_counts_a_rerun_replaces_a_failure() {
        let runs = [
            run(1, "ci", "completed", Some("failure")),
            run(5, "ci", "completed", Some("success")),
        ];
        assert_eq!(ready("a", "a", &ws(), &runs), Ready::Go);
    }

    #[test]
    fn a_pull_requests_run_on_the_same_commit_is_not_mains() {
        // 2026-11-19: a branch's pull_request run of ci (red) on the commit main had just been fast-forwarded to stopped
        // the deploy while main's own ci was still running
        let mut pr = run(9, "ci", "completed", Some("failure"));
        pr.on_main = false;
        let runs = [run(1, "ci", "in_progress", None), pr];
        assert_eq!(ready("a", "a", &ws(), &runs), Ready::Wait(vec!["ci".into()]));
    }

    #[test]
    fn other_workflows_do_not_count() {
        let runs = [
            run(1, "deploy", "in_progress", None),
            run(2, "branch", "completed", Some("failure")),
        ];
        assert_eq!(ready("a", "a", &ws(), &runs), Ready::Go);
    }
}
