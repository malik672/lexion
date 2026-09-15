use crate::router::{Path, PathSet};

/// Generates each pool-disjoint portfolio once in ascending path-index order.
pub fn maximal_portfolios(
    paths: &PathSet,
    candidates: &[usize],
    max_paths: usize,
) -> Vec<Vec<usize>> {
    let mut all = Vec::new();
    enumerate(paths, candidates, max_paths, 0, &mut Vec::new(), &mut all);
    all.into_iter()
        .filter(|portfolio| {
            portfolio.len() == max_paths
                || !candidates.iter().copied().any(|candidate| {
                    !portfolio.contains(&candidate)
                        && portfolio.iter().all(|&member| {
                            compatible(&paths.paths[member], &paths.paths[candidate])
                        })
                })
        })
        .collect()
}

fn enumerate(
    paths: &PathSet,
    candidates: &[usize],
    max_paths: usize,
    next: usize,
    current: &mut Vec<usize>,
    out: &mut Vec<Vec<usize>>,
) {
    if !current.is_empty() {
        out.push(current.clone());
    }
    if current.len() == max_paths {
        return;
    }
    for position in next..candidates.len() {
        let candidate = candidates[position];
        if current
            .iter()
            .all(|&member| compatible(&paths.paths[member], &paths.paths[candidate]))
        {
            current.push(candidate);
            enumerate(paths, candidates, max_paths, position + 1, current, out);
            current.pop();
        }
    }
}

fn compatible(a: &Path, b: &Path) -> bool {
    !a.hops
        .iter()
        .any(|left| b.hops.iter().any(|right| left.pool == right.pool))
}
