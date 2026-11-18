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
//!
//! Frozen sandboxes (paused in place; a deployment that freezes, docs/CONTRACTS.md) take no capacity: packing and
//! placement run over the running sandboxes only, and a host left with frozen sandboxes alone is empty. What is
//! evicted (recorded into the object store, its slot freed) is not chosen by a rule of its own: it is what the plan
//! leaves over a host's slots ([`evictions`]), and every frozen sandbox of a host that goes. A frozen sandbox is
//! evicted the more readily the more time it has left before its `until` ([`eviction_order`]).

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

/// A frozen sandbox as packing sees it: paused on its host, taking its memory there but no capacity in the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackFrozen {
    pub id: String,
    pub host: String,
    pub size: u32,
    /// When it is due back (unix ms); `None`: no time set, the most time left of all.
    pub until: Option<i64>,
    /// When it froze (unix ms): between equal `until`s the older freeze goes first.
    pub frozen_at: i64,
}

/// The frozen sandboxes in the order they are evicted: the most time left first (no `until` before any), then the
/// oldest freeze.
#[must_use]
pub fn eviction_order(frozen: &[PackFrozen]) -> Vec<&PackFrozen> {
    let mut v: Vec<&PackFrozen> = frozen.iter().collect();
    v.sort_by(|a, b| {
        let left = |f: &PackFrozen| f.until.map_or((1, 0), |u| (0, u));
        left(b)
            .cmp(&left(a))
            .then_with(|| a.frozen_at.cmp(&b.frozen_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    v
}

/// The frozen sandboxes a target forces out: on each host, `load` (the target's running units there, a placement's
/// included) plus the frozen sandboxes it keeps fit its slots; the ones over that are evicted, in [`eviction_order`].
#[must_use]
pub fn evictions(hosts: &[PackHost], load: &BTreeMap<String, u32>, frozen: &[PackFrozen]) -> Vec<String> {
    let mut room: BTreeMap<&str, i64> = hosts
        .iter()
        .map(|h| {
            let used = load.get(&h.name).copied().unwrap_or(0)
                + frozen.iter().filter(|f| f.host == h.name).map(|f| f.size).sum::<u32>();
            (h.name.as_str(), i64::from(used) - i64::from(h.slots))
        })
        .collect();
    let mut out = Vec::new();
    for f in eviction_order(frozen) {
        if let Some(over) = room.get_mut(f.host.as_str())
            && *over > 0
        {
            *over -= i64::from(f.size);
            out.push(f.id.clone());
        }
    }
    out
}

/// Where a new sandbox of `need` units goes, given each open host's running `load`: a host with room counting its
/// frozen sandboxes as taken (the fullest, as `hosts::pick`), else the host whose eviction of the frozen sandbox with
/// the most time left makes room, else none (the scaler launches a host). The evictions it costs are [`evictions`] of
/// the load with it placed.
#[must_use]
pub fn place(hosts: &[PackHost], load: &BTreeMap<String, u32>, frozen: &[PackFrozen], need: u32) -> Option<String> {
    let used = |h: &PackHost, with_frozen: bool| {
        load.get(&h.name).copied().unwrap_or(0)
            + if with_frozen {
                frozen.iter().filter(|f| f.host == h.name).map(|f| f.size).sum()
            } else {
                0
            }
    };
    let fits = |h: &&PackHost, with_frozen: bool| h.open && used(h, with_frozen) + need <= h.slots;
    if let Some(h) = hosts.iter().filter(|h| fits(h, true)).min_by(|a, b| {
        (a.slots - used(a, true))
            .cmp(&(b.slots - used(b, true)))
            .then_with(|| a.name.cmp(&b.name))
    }) {
        return Some(h.name.clone());
    }
    eviction_order(frozen)
        .into_iter()
        .find(|f| hosts.iter().any(|h| h.name == f.host && fits(&h, false)))
        .map(|f| f.host.clone())
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

    fn frozen(id: &str, h: &str, until: Option<i64>, at: i64) -> PackFrozen {
        PackFrozen {
            id: id.into(),
            host: h.into(),
            size: 1,
            until,
            frozen_at: at,
        }
    }

    fn loads(v: &[(&str, u32)]) -> BTreeMap<String, u32> {
        v.iter().map(|(h, n)| ((*h).to_owned(), *n)).collect()
    }

    #[test]
    fn eviction_order_most_time_left_first() {
        let f = [
            frozen("soon", "a", Some(100), 1),
            frozen("forever", "a", None, 5),
            frozen("late-new", "b", Some(900), 9),
            frozen("late-old", "b", Some(900), 2),
        ];
        let ids: Vec<&str> = eviction_order(&f).iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["forever", "late-old", "late-new", "soon"]);
    }

    #[test]
    fn a_frozen_sandbox_stays_while_its_slot_is_not_needed() {
        let hosts = [host("a", 4, true)];
        let f = [frozen("x", "a", None, 0), frozen("y", "a", Some(5), 0)];
        assert_eq!(evictions(&hosts, &loads(&[("a", 2)]), &f), Vec::<String>::new());
        // one more running sandbox there: the one with the most time left goes
        assert_eq!(evictions(&hosts, &loads(&[("a", 3)]), &f), ["x"]);
        assert_eq!(evictions(&hosts, &loads(&[("a", 4)]), &f), ["x", "y"]);
    }

    #[test]
    fn a_new_sandbox_takes_a_free_slot_then_a_frozen_ones_then_none() {
        let hosts = [host("a", 4, true), host("b", 4, true)];
        // b has a free slot: it goes there and nothing is evicted
        let f = [frozen("x", "a", None, 0), frozen("y", "b", Some(1), 0)];
        assert_eq!(
            place(&hosts, &loads(&[("a", 3), ("b", 2)]), &f, 1).as_deref(),
            Some("b")
        );
        // no free slot: the host of the frozen sandbox with the most time left, which is then evicted
        let l = loads(&[("a", 3), ("b", 3)]);
        assert_eq!(place(&hosts, &l, &f, 1).as_deref(), Some("a"));
        assert_eq!(evictions(&hosts, &loads(&[("a", 4), ("b", 3)]), &f), ["x"]);
        // no frozen sandbox either: nowhere (a host is launched)
        assert_eq!(place(&hosts, &loads(&[("a", 4), ("b", 4)]), &[], 1), None);
        // a host short of memory takes nothing
        assert_eq!(
            place(
                &[host("a", 4, false)],
                &loads(&[("a", 3)]),
                &[frozen("x", "a", None, 0)],
                1
            ),
            None
        );
    }

    #[test]
    fn frozen_sandboxes_never_keep_a_host() {
        // b holds one running sandbox and three frozen ones: the running one fits on a, so b goes (its frozen ones
        // are evicted with it); a keeps its own frozen one as long as it fits
        let hosts = [host("a", 4, true), host("b", 4, true)];
        let sbx = [on("a", 2, 1), on("b", 1, 1)].concat();
        let moves = plan_packing(&hosts, &sbx);
        assert_eq!(hosts_used(&after(&sbx, &moves)), BTreeSet::from(["a".to_owned()]));
        let f = [
            frozen("fa", "a", Some(9), 0),
            frozen("fb1", "b", None, 0),
            frozen("fb2", "b", Some(1), 0),
        ];
        assert_eq!(evictions(&hosts, &loads(&[("a", 3)]), &f), Vec::<String>::new());
        // with a fuller a, its frozen one makes the room
        assert_eq!(evictions(&hosts, &loads(&[("a", 4)]), &f), ["fa"]);
    }
}
