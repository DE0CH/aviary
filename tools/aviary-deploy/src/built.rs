//! Whether a branch's check can read its commit's images yet (yyc.yml; Deyao, 2026-11-26: CI is deterministic, no
//! step depends on how long another one takes). An image a commit names (tags.rs) and GHCR lacks is built by its part's
//! own workflow, the `.github/workflows/<file>` among the part's paths in deploy/images.json, in a run on a commit from
//! the part's last commit up to the one checked (a later commit that starts the workflow builds the same tag). So the
//! check waits for those runs, not for a length of time: while one runs it waits; once the newest has ended, the image
//! is either there or it never will be, and the run's own result says why.

/// one run of a part's workflow on one of the commits (GitHub's `actions/runs`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: u64,
    /// .github/workflows/<file>
    pub path: String,
    pub status: String,
    pub conclusion: Option<String>,
}

/// a part with an image GHCR lacks: what builds it, and that workflow's runs on the commits that can build it
#[derive(Debug, Clone)]
pub struct Absent {
    pub part: String,
    /// the part's workflow (None: its paths name none)
    pub workflow: Option<String>,
    pub runs: Vec<Run>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// every image is there
    Ready,
    /// these parts' builds are still running
    Wait(Vec<String>),
    /// these parts' images will never come from this branch's runs, with why
    Fail(Vec<String>),
}

/// The one rule: an absent part waits while the newest run of its workflow is not over; once it is over, the image
/// that is still absent is a failure, named by that run's result (a red build never published, a green one that
/// should have). No run at all is a failure too: nothing is building the image.
pub fn verdict(absent: &[Absent]) -> Verdict {
    let (mut wait, mut fail) = (Vec::new(), Vec::new());
    for a in absent {
        let Some(w) = &a.workflow else {
            fail.push(format!("{}: no workflow among its paths builds it", a.part));
            continue;
        };
        let newest = a.runs.iter().filter(|r| &r.path == w).max_by_key(|r| r.id);
        match newest {
            None => fail.push(format!("{}: no {w} run on its commits builds it", a.part)),
            Some(r) if r.status != "completed" => wait.push(a.part.clone()),
            Some(r) => match r.conclusion.as_deref() {
                Some("success") => fail.push(format!(
                    "{}: {w} run {} passed and its image is still not in GHCR",
                    a.part, r.id
                )),
                c => fail.push(format!(
                    "{}: {w} run {} ended {}, so it published nothing",
                    a.part,
                    r.id,
                    c.unwrap_or("without a conclusion")
                )),
            },
        }
    }
    if !fail.is_empty() {
        Verdict::Fail(fail)
    } else if !wait.is_empty() {
        Verdict::Wait(wait)
    } else {
        Verdict::Ready
    }
}

/// the part's workflow: the one `.github/workflows/*.yml` among its paths
pub fn workflow_of(paths: &[String]) -> Option<String> {
    let mut w = paths.iter().filter(|p| {
        p.starts_with(".github/workflows/")
            && std::path::Path::new(p.as_str())
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("yml") || e.eq_ignore_ascii_case("yaml"))
    });
    match (w.next(), w.next()) {
        (Some(one), None) => Some(one.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64, status: &str, conclusion: Option<&str>) -> Run {
        Run {
            id,
            path: ".github/workflows/ci.yml".into(),
            status: status.into(),
            conclusion: conclusion.map(String::from),
        }
    }

    fn absent(runs: Vec<Run>) -> Absent {
        Absent {
            part: "aviary".into(),
            workflow: Some(".github/workflows/ci.yml".into()),
            runs,
        }
    }

    #[test]
    fn nothing_absent_is_ready() {
        assert_eq!(verdict(&[]), Verdict::Ready);
    }

    #[test]
    fn a_build_still_running_is_waited_for_however_long_it_takes() {
        assert_eq!(
            verdict(&[absent(vec![run(1, "in_progress", None)])]),
            Verdict::Wait(vec!["aviary".into()])
        );
        assert_eq!(
            verdict(&[absent(vec![run(1, "queued", None)])]),
            Verdict::Wait(vec!["aviary".into()])
        );
    }

    #[test]
    fn a_red_build_fails_at_once_naming_its_run() {
        let Verdict::Fail(f) = verdict(&[absent(vec![run(7, "completed", Some("failure"))])]) else {
            panic!()
        };
        assert_eq!(
            f,
            vec!["aviary: .github/workflows/ci.yml run 7 ended failure, so it published nothing"]
        );
    }

    #[test]
    fn a_green_build_without_its_image_is_a_failure() {
        let Verdict::Fail(f) = verdict(&[absent(vec![run(7, "completed", Some("success"))])]) else {
            panic!()
        };
        assert!(f[0].contains("passed and its image is still not in GHCR"), "{f:?}");
    }

    #[test]
    fn the_newest_run_decides_a_rerun_included() {
        // a cancelled run, then a newer one still going: wait for the newer
        let a = absent(vec![
            run(5, "completed", Some("cancelled")),
            run(9, "in_progress", None),
        ]);
        assert_eq!(verdict(&[a]), Verdict::Wait(vec!["aviary".into()]));
        // a re-run keeps its id and is in progress again
        let a = absent(vec![run(5, "in_progress", None)]);
        assert_eq!(verdict(&[a]), Verdict::Wait(vec!["aviary".into()]));
    }

    #[test]
    fn runs_of_other_workflows_do_not_count() {
        let mut other = run(9, "in_progress", None);
        other.path = ".github/workflows/app.yml".into();
        let Verdict::Fail(f) = verdict(&[absent(vec![other])]) else {
            panic!()
        };
        assert_eq!(
            f,
            vec!["aviary: no .github/workflows/ci.yml run on its commits builds it"]
        );
    }

    #[test]
    fn no_run_at_all_fails_rather_than_waiting() {
        assert!(matches!(verdict(&[absent(vec![])]), Verdict::Fail(_)));
    }

    #[test]
    fn a_failure_anywhere_is_reported_even_while_another_part_builds() {
        let red = Absent {
            part: "egress".into(),
            workflow: Some(".github/workflows/egress.yml".into()),
            runs: vec![Run {
                id: 3,
                path: ".github/workflows/egress.yml".into(),
                status: "completed".into(),
                conclusion: Some("failure".into()),
            }],
        };
        let Verdict::Fail(f) = verdict(&[absent(vec![run(1, "in_progress", None)]), red]) else {
            panic!()
        };
        assert_eq!(f.len(), 1);
        assert!(f[0].starts_with("egress:"));
    }

    #[test]
    fn the_workflow_is_the_one_among_the_paths() {
        let p = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            workflow_of(&p(&["crates", ".github/ci", ".github/workflows/ci.yml", "flake.nix"])),
            Some(".github/workflows/ci.yml".into())
        );
        assert_eq!(workflow_of(&p(&["crates"])), None);
        assert_eq!(
            workflow_of(&p(&[".github/workflows/a.yml", ".github/workflows/b.yml"])),
            None
        );
    }

    #[test]
    fn every_part_of_the_repository_names_its_workflow() {
        let cfg: crate::tags::Config = serde_json::from_str(include_str!("../../../deploy/images.json")).unwrap();
        for (part, spec) in &cfg {
            assert!(workflow_of(&spec.paths).is_some(), "{part} names no single workflow");
        }
    }
}
