//! The order of the commits of a plan, given which commits each has to come after.

use std::collections::BTreeSet;

/// The commits grouped into the cycles of `must_follow`, with a group number for each commit.
struct Cycles {
    /// Each group's commits, in ascending order, and the groups by their first commit.
    members: Vec<Vec<usize>>,
    group_of: Vec<usize>,
}

/// The commits to make, as groups of commit numbers: a group holds commits that need each
/// other, and comes after the groups it needs. Groups are taken in the order of the model's
/// plan by their first commit, and what a group needs is put right before it, if it isn't
/// there yet.
pub(super) fn in_order(must_follow: &[BTreeSet<usize>]) -> Vec<Vec<usize>> {
    let cycles = cycles(must_follow);
    let mut order = Vec::new();
    for group in 0..cycles.members.len() {
        place(group, &cycles, must_follow, &mut order);
    }
    order
        .into_iter()
        .map(|group| cycles.members[group].clone())
        .collect()
}

fn cycles(must_follow: &[BTreeSet<usize>]) -> Cycles {
    let reach: Vec<BTreeSet<usize>> = (0..must_follow.len())
        .map(|commit| reachable(commit, must_follow))
        .collect();
    let mut cycles = Cycles {
        members: Vec::new(),
        group_of: Vec::with_capacity(must_follow.len()),
    };
    for commit in 0..must_follow.len() {
        let first = first_of_cycle(commit, &reach);
        if first == commit {
            cycles.group_of.push(cycles.members.len());
            cycles.members.push(vec![commit]);
        } else {
            let group = cycles.group_of[first];
            cycles.group_of.push(group);
            cycles.members[group].push(commit);
        }
    }
    cycles
}

/// Puts `group` in `order`, after the groups it needs.
fn place(group: usize, cycles: &Cycles, must_follow: &[BTreeSet<usize>], order: &mut Vec<usize>) {
    if order.contains(&group) {
        return;
    }
    let needed: BTreeSet<usize> = cycles.members[group]
        .iter()
        .flat_map(|&member| &must_follow[member])
        .map(|&commit| cycles.group_of[commit])
        .filter(|&needed| needed != group)
        .collect();
    for needed in needed {
        place(needed, cycles, must_follow, order);
    }
    order.push(group);
}

/// The lowest number in the cycle `commit` is in: the commits it needs, in any number of
/// steps, that need it. That's its own number if it is the lowest, or in no cycle.
fn first_of_cycle(commit: usize, reach: &[BTreeSet<usize>]) -> usize {
    (0..commit)
        .find(|&earlier| reach[commit].contains(&earlier) && reach[earlier].contains(&commit))
        .unwrap_or(commit)
}

/// The commits that `from` needs, directly or through others.
fn reachable(from: usize, must_follow: &[BTreeSet<usize>]) -> BTreeSet<usize> {
    let mut reached = BTreeSet::new();
    let mut pending = vec![from];
    while let Some(commit) = pending.pop() {
        for &needed in &must_follow[commit] {
            if reached.insert(needed) {
                pending.push(needed);
            }
        }
    }
    reached
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pair (user, adder) of different commits among `count`, which is a possible need.
    fn possible_needs(count: usize) -> Vec<(usize, usize)> {
        (0..count)
            .flat_map(|user| {
                (0..count)
                    .filter(move |&adder| adder != user)
                    .map(move |adder| (user, adder))
            })
            .collect()
    }

    /// The needs that the bits set in `graph` stand for, by the order of `pairs`.
    fn graph_of(graph: usize, count: usize, pairs: &[(usize, usize)]) -> Vec<BTreeSet<usize>> {
        let mut must_follow = vec![BTreeSet::new(); count];
        let chosen = pairs
            .iter()
            .enumerate()
            .filter(|&(bit, _)| graph >> bit & 1 == 1);
        for (_, &(user, adder)) in chosen {
            must_follow[user].insert(adder);
        }
        must_follow
    }

    /// Who needs whom, in any number of steps: the needs, joined with themselves until none
    /// is added.
    fn needs_in_any_steps(must_follow: &[BTreeSet<usize>]) -> Vec<Vec<bool>> {
        let count = must_follow.len();
        let direct: Vec<Vec<bool>> = (0..count)
            .map(|user| {
                (0..count)
                    .map(|adder| must_follow[user].contains(&adder))
                    .collect()
            })
            .collect();
        (0..count).fold(direct, |needs, _| joined_once_more(&needs))
    }

    /// `needs`, and each need that two of them make together.
    fn joined_once_more(needs: &[Vec<bool>]) -> Vec<Vec<bool>> {
        let count = needs.len();
        let through_one =
            |from: usize, to: usize| (0..count).any(|via| needs[from][via] && needs[via][to]);
        (0..count)
            .map(|from| {
                (0..count)
                    .map(|to| needs[from][to] || through_one(from, to))
                    .collect()
            })
            .collect()
    }

    fn position_of(groups: &[Vec<usize>], commit: usize) -> usize {
        groups
            .iter()
            .position(|group| group.contains(&commit))
            .unwrap()
    }

    /// The groups split the commits, and two commits share a group exactly when each needs
    /// the other.
    fn assert_the_groups_are_the_cycles(groups: &[Vec<usize>], needs: &[Vec<bool>], case: &str) {
        let mut commits: Vec<usize> = groups.iter().flatten().copied().collect();
        commits.sort_unstable();
        assert_eq!(commits, (0..needs.len()).collect::<Vec<_>>(), "{case}");
        for (a, row) in needs.iter().enumerate() {
            for (b, &a_needs_b) in row.iter().enumerate() {
                let in_a_cycle = a == b || (a_needs_b && needs[b][a]);
                let together = position_of(groups, a) == position_of(groups, b);
                assert_eq!(together, in_a_cycle, "{case}");
            }
        }
        for group in groups {
            assert!(group.windows(2).all(|pair| pair[0] < pair[1]), "{case}");
        }
    }

    fn assert_every_need_points_backwards(
        groups: &[Vec<usize>],
        must_follow: &[BTreeSet<usize>],
        case: &str,
    ) {
        for (user, adders) in must_follow.iter().enumerate() {
            for &adder in adders {
                assert!(
                    position_of(groups, adder) <= position_of(groups, user),
                    "{case}"
                );
            }
        }
    }

    /// A group's turn is the first commit of any group that holds it or needs it, in any
    /// number of steps: the turn of the model's plan at which it is put in place.
    fn turn_of(group: &[usize], groups: &[Vec<usize>], needs: &[Vec<bool>]) -> usize {
        (0..needs.len())
            .filter(|&other| {
                group.contains(&other) || group.iter().any(|&member| needs[other][member])
            })
            .map(|other| groups[position_of(groups, other)][0])
            .min()
            .unwrap()
    }

    fn assert_a_valid_order(must_follow: &[BTreeSet<usize>]) {
        let groups = in_order(must_follow);
        let needs = needs_in_any_steps(must_follow);
        let case = format!("{must_follow:?} gave {groups:?}");
        assert_the_groups_are_the_cycles(&groups, &needs, &case);
        assert_every_need_points_backwards(&groups, must_follow, &case);
        let turns: Vec<usize> = groups
            .iter()
            .map(|group| turn_of(group, &groups, &needs))
            .collect();
        assert!(turns.windows(2).all(|pair| pair[0] <= pair[1]), "{case}");
    }

    #[test]
    fn orders_all_4096_graphs_of_four_commits() {
        let pairs = possible_needs(4);
        assert_eq!(1usize << pairs.len(), 4096);
        for graph in 0..1usize << pairs.len() {
            assert_a_valid_order(&graph_of(graph, 4, &pairs));
        }
    }

    #[test]
    fn keeps_a_plan_without_needs_as_it_is() {
        assert_eq!(in_order(&graph_of(0, 4, &[])), [[0], [1], [2], [3]]);
    }
}
