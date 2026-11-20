//! Which commit of main deploys now: the one rule of the deploy run (docs/DEPLOY.md). The deploy is the pipeline's
//! last stage, and a newer commit still in the earlier stages never holds back one that has passed them (Deyao,
//! 2026-11-20: with main moving every 10-20 min and its ci taking 2 h, waiting for main's head deployed nothing for
//! hours). So the newest commit main has pointed at whose every part's newest run is over and green deploys, as long as
//! it is newer than what runs (the caller leaves older ones out): a rollout only points the clusters at that commit's
//! images. A part whose workflow did not run on the commit (its paths unchanged) has nothing to wait for.

/// one workflow run on the commit (GitHub's `actions/runs`)
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

/// one commit main has pointed at, with its runs
#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub runs: Vec<Run>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ready {
    /// these workflows' runs are not over
    Wait(Vec<String>),
    /// these workflows' runs ended red
    Fail(Vec<String>),
    Go,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    /// this commit deploys
    Go(String),
    /// no commit newer than what runs is ready: how the newest one stands (`None`: main has nothing newer)
    Nothing(Option<(String, Ready)>),
}

/// how one commit stands. `workflows`: the parts' workflow names (the deploy run's own is not among them)
pub fn ready(workflows: &[String], runs: &[Run]) -> Ready {
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

/// `commits`: the commits main has pointed at that are newer than what runs, newest first
pub fn pick(workflows: &[String], commits: &[Commit]) -> Pick {
    for c in commits {
        if ready(workflows, &c.runs) == Ready::Go {
            return Pick::Go(c.sha.clone());
        }
    }
    Pick::Nothing(commits.first().map(|c| (c.sha.clone(), ready(workflows, &c.runs))))
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
    fn all_green_goes_and_a_part_that_did_not_run_waits_for_nothing() {
        let runs = [
            run(1, "ci", "completed", Some("success")),
            run(2, "app", "completed", Some("skipped")),
        ];
        assert_eq!(ready(&ws(), &runs), Ready::Go);
    }

    #[test]
    fn a_run_not_over_waits() {
        let runs = [
            run(1, "ci", "completed", Some("success")),
            run(2, "yyc", "in_progress", None),
        ];
        assert_eq!(ready(&ws(), &runs), Ready::Wait(vec!["yyc".into()]));
    }

    #[test]
    fn red_fails_even_while_another_runs() {
        let runs = [
            run(1, "ci", "completed", Some("failure")),
            run(2, "yyc", "queued", None),
        ];
        assert_eq!(ready(&ws(), &runs), Ready::Fail(vec!["ci: failure".into()]));
    }

    #[test]
    fn the_newest_run_of_a_workflow_counts_a_rerun_replaces_a_failure() {
        let runs = [
            run(1, "ci", "completed", Some("failure")),
            run(5, "ci", "completed", Some("success")),
        ];
        assert_eq!(ready(&ws(), &runs), Ready::Go);
    }

    #[test]
    fn a_pull_requests_run_on_the_same_commit_is_not_mains() {
        // 2026-11-19: a branch's pull_request run of ci (red) on the commit main had just been fast-forwarded to stopped
        // the deploy while main's own ci was still running
        let mut pr = run(9, "ci", "completed", Some("failure"));
        pr.on_main = false;
        let runs = [run(1, "ci", "in_progress", None), pr];
        assert_eq!(ready(&ws(), &runs), Ready::Wait(vec!["ci".into()]));
    }

    #[test]
    fn other_workflows_do_not_count() {
        let runs = [
            run(1, "deploy", "in_progress", None),
            run(2, "branch", "completed", Some("failure")),
        ];
        assert_eq!(ready(&ws(), &runs), Ready::Go);
    }

    fn commit(sha: &str, runs: Vec<Run>) -> Commit {
        Commit { sha: sha.into(), runs }
    }

    #[test]
    fn an_older_green_commit_deploys_while_a_newer_one_is_still_in_ci() {
        // 2026-11-20: main's head was always a commit whose 2 h ci had not finished, and nothing deployed for 7 h
        let commits = [
            commit("new", vec![run(9, "ci", "in_progress", None)]),
            commit(
                "old",
                vec![
                    run(5, "ci", "completed", Some("success")),
                    run(6, "app", "completed", Some("success")),
                ],
            ),
        ];
        assert_eq!(pick(&ws(), &commits), Pick::Go("old".into()));
    }

    #[test]
    fn the_newest_green_commit_deploys() {
        let commits = [
            commit("c", vec![run(9, "ci", "completed", Some("success"))]),
            commit("b", vec![run(5, "ci", "completed", Some("success"))]),
        ];
        assert_eq!(pick(&ws(), &commits), Pick::Go("c".into()));
    }

    #[test]
    fn a_red_newer_commit_does_not_hold_back_an_older_green_one() {
        let commits = [
            commit("red", vec![run(9, "ci", "completed", Some("failure"))]),
            commit("green", vec![run(5, "ci", "completed", Some("success"))]),
        ];
        assert_eq!(pick(&ws(), &commits), Pick::Go("green".into()));
    }

    #[test]
    fn nothing_ready_says_how_the_newest_stands() {
        let commits = [
            commit("b", vec![run(9, "ci", "completed", Some("failure"))]),
            commit("a", vec![run(5, "app", "queued", None)]),
        ];
        assert_eq!(
            pick(&ws(), &commits),
            Pick::Nothing(Some(("b".into(), Ready::Fail(vec!["ci: failure".into()]))))
        );
        assert_eq!(pick(&ws(), &[]), Pick::Nothing(None));
    }
}
