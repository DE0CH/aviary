//! Packing (hermit 13.11): where the scaler would like every sandbox to be, as a pure function: all sandboxes on the
//! fewest hosts. Ported as is from jarvis3 `web/packing.ts`. Host age plays no part; a host the moves leave empty
//! goes by the release rule (13.6) on its own.
//!
//! ```text
//! target = current placement
//! for each host h in use, least loaded first (ties: the one whose sandboxes moved least recently):
//!   place h's sandboxes, biggest first, on the other open hosts still in use, each on the fullest one it fits
//!   all fit -> they go there in the target (h is empty in it); else h keeps them
//! moves = sandboxes whose target host differs, only if the target uses strictly fewer hosts
//! ```

use std::collections::{BTreeMap, BTreeSet};

/// A host as packing sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackHost {
    pub name: String,
    /// Slots it has (memory and CPU by the configured sandbox size, the operator's cap).
    pub slots: u32,
    /// May take sandboxes (active and answering, not short of memory, its `JuiceFS` mounted).
    pub open: bool,
}

/// A sandbox as packing sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackSandbox {
    pub id: String,
    pub host: String,
    /// Slots it takes (a sandbox from before a smaller `sandbox.memory` takes more).
    pub size: u32,
    /// When it was last moved (unix ms), 0 = never: the tie-breaker between equally good plans.
    pub moved_at: i64,
}

/// One move of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    pub id: String,
    pub from: String,
    pub to: String,
}

/// The moves that put the sandboxes on strictly fewer hosts, or none.
#[must_use]
pub fn plan_packing(hosts: &[PackHost], sandboxes: &[PackSandbox]) -> Vec<Move> {
    let mut target: BTreeMap<&str, &str> = sandboxes.iter().map(|s| (s.id.as_str(), s.host.as_str())).collect();
    let load = |t: &BTreeMap<&str, &str>, h: &str| -> u32 {
        sandboxes
            .iter()
            .filter(|s| t.get(s.id.as_str()).copied() == Some(h))
            .map(|s| s.size)
            .sum()
    };
    let in_use = |t: &BTreeMap<&str, &str>, h: &str| t.values().any(|x| *x == h);
    let last_move = |h: &str| -> i64 {
        sandboxes
            .iter()
            .filter(|s| s.host == h)
            .map(|s| s.moved_at)
            .max()
            .unwrap_or(0)
            .max(0)
    };

    // the candidate order is fixed from the current placement, as in the original (a stable sort)
    let mut candidates: Vec<&PackHost> = hosts.iter().filter(|h| in_use(&target, &h.name)).collect();
    candidates.sort_by(|a, b| {
        load(&target, &a.name)
            .cmp(&load(&target, &b.name))
            .then_with(|| last_move(&a.name).cmp(&last_move(&b.name)))
    });
    for h in candidates {
        let mut leaving: Vec<&PackSandbox> = sandboxes
            .iter()
            .filter(|s| target.get(s.id.as_str()).copied() == Some(h.name.as_str()))
            .collect();
        leaving.sort_by_key(|s| std::cmp::Reverse(s.size));
        // free room on the other open hosts still in use, in the hosts' order (the tie order of the original)
        let mut free: Vec<(&str, i64)> = hosts
            .iter()
            .filter(|x| x.open && x.name != h.name && in_use(&target, &x.name))
            .map(|x| (x.name.as_str(), i64::from(x.slots) - i64::from(load(&target, &x.name))))
            .collect();
        let mut trial = target.clone();
        let fits = leaving.iter().all(|s| {
            let size = i64::from(s.size);
            let best = free
                .iter()
                .enumerate()
                .filter(|(_, (_, f))| *f >= size)
                .min_by_key(|(_, (_, f))| *f)
                .map(|(i, _)| i);
            let Some(i) = best else {
                return false;
            };
            free[i].1 -= size;
            trial.insert(s.id.as_str(), free[i].0);
            true
        });
        if fits {
            target = trial;
        }
    }

    let used = |t: &BTreeMap<&str, &str>| t.values().collect::<BTreeSet<_>>().len();
    let now: BTreeMap<&str, &str> = sandboxes.iter().map(|s| (s.id.as_str(), s.host.as_str())).collect();
    if used(&target) >= used(&now) {
        return Vec::new();
    }
    sandboxes
        .iter()
        .filter_map(|s| {
            let to = target.get(s.id.as_str()).copied()?;
            (to != s.host).then(|| Move {
                id: s.id.clone(),
                from: s.host.clone(),
                to: to.to_owned(),
            })
        })
        .collect()
}

/// Ported from jarvis3 `test/packing.test.ts`.
#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    fn host(name: &str, slots: u32, open: bool) -> PackHost {
        PackHost {
            name: name.into(),
            slots,
            open,
        }
    }

    fn on(h: &str, n: usize, size: u32) -> Vec<PackSandbox> {
        (0..n)
            .map(|i| PackSandbox {
                id: format!("{h}-{i}"),
                host: h.into(),
                size,
                moved_at: 0,
            })
            .collect()
    }

    fn after(sbx: &[PackSandbox], moves: &[Move]) -> BTreeMap<String, String> {
        let mut w: BTreeMap<String, String> = sbx.iter().map(|s| (s.id.clone(), s.host.clone())).collect();
        for m in moves {
            w.insert(m.id.clone(), m.to.clone());
        }
        w
    }

    fn hosts_used(w: &BTreeMap<String, String>) -> BTreeSet<String> {
        w.values().cloned().collect()
    }

    #[test]
    fn three_hosts_partly_full_everything_lands_on_the_fullest() {
        let hosts = [host("a", 14, true), host("b", 14, true), host("c", 14, true)];
        let sbx = [on("a", 5, 1), on("b", 3, 1), on("c", 2, 1)].concat();
        let moves = plan_packing(&hosts, &sbx);
        assert_eq!(moves.len(), 5);
        assert_eq!(hosts_used(&after(&sbx, &moves)), BTreeSet::from(["a".to_owned()]));
    }

    #[test]
    fn more_than_one_hosts_worth_the_fewest_hosts_that_hold_them() {
        let hosts = [host("a", 4, true), host("b", 4, true), host("c", 4, true)];
        let sbx = [on("a", 3, 1), on("b", 2, 1), on("c", 2, 1)].concat();
        assert_eq!(hosts_used(&after(&sbx, &plan_packing(&hosts, &sbx))).len(), 2);
    }

    #[test]
    fn no_move_without_strictly_fewer_hosts() {
        // full hosts
        let hosts = [host("a", 4, true), host("b", 4, true)];
        assert_eq!(plan_packing(&hosts, &[on("a", 3, 1), on("b", 3, 1)].concat()), vec![]);
        // a 2-slot sandbox (made before a smaller sandbox.memory) needs 2 free slots
        assert_eq!(plan_packing(&hosts, &[on("a", 3, 1), on("b", 1, 2)].concat()), vec![]);
        // an empty host is never a target (it goes by the release rule)
        assert_eq!(
            plan_packing(&[host("a", 14, true), host("empty", 14, true)], &on("a", 3, 1)),
            vec![]
        );
        // a host short of memory takes nothing
        assert_eq!(
            plan_packing(
                &[host("a", 14, false), host("b", 14, true)],
                &[on("a", 1, 1), on("b", 1, 1)].concat()
            ),
            vec![Move {
                id: "a-0".into(),
                from: "a".into(),
                to: "b".into()
            }]
        );
    }

    #[test]
    fn tie_breaker_moves_the_sandboxes_moved_least_recently() {
        let sbx = [
            PackSandbox {
                id: "x".into(),
                host: "a".into(),
                size: 1,
                moved_at: 1000,
            },
            PackSandbox {
                id: "y".into(),
                host: "b".into(),
                size: 1,
                moved_at: 0,
            },
        ];
        assert_eq!(
            plan_packing(&[host("a", 14, true), host("b", 14, true)], &sbx),
            vec![Move {
                id: "y".into(),
                from: "b".into(),
                to: "a".into()
            }]
        );
    }
}
