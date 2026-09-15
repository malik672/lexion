use crate::{
    Amount,
    router::{PathSet, Router},
};

#[derive(Clone, Copy, Debug)]
pub struct FrontierConfig {
    pub paths_per_scale: usize,
}

impl Default for FrontierConfig {
    fn default() -> Self {
        Self { paths_per_scale: 2 }
    }
}

/// Path indices retained by exact replay at several possible allocation sizes.
pub fn multiscale_frontier(
    router: &Router<'_>,
    paths: &PathSet,
    total: Amount,
    config: FrontierConfig,
) -> Box<[usize]> {
    assert!(
        config.paths_per_scale > 0,
        "paths_per_scale must be positive"
    );
    const FULL_ORDER_PATHS: usize = 6;
    const MAX_PATHS: usize = 10;
    let scales = [(1_u32, 20_u32), (1, 10), (1, 4), (1, 2)];
    let mut retained = Vec::new();

    let mut full_order = rank_paths(router, paths, total);
    retained.extend(
        full_order
            .drain(..full_order.len().min(FULL_ORDER_PATHS))
            .map(|(index, _)| index),
    );

    for (numerator, denominator) in scales {
        if retained.len() == MAX_PATHS {
            break;
        }
        let mut amount = total * Amount::from(numerator) / Amount::from(denominator);
        if amount.is_zero() && !total.is_zero() {
            amount = Amount::from(1);
        }
        let ranked = rank_paths(router, paths, amount);
        for (index, _) in ranked.into_iter().take(config.paths_per_scale) {
            if !retained.contains(&index) {
                retained.push(index);
                if retained.len() == MAX_PATHS {
                    break;
                }
            }
        }
    }
    retained.sort_unstable();
    retained.into_boxed_slice()
}

fn rank_paths(router: &Router<'_>, paths: &PathSet, amount: Amount) -> Vec<(usize, Amount)> {
    let mut ranked = paths
        .paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            router
                .replay_path(path, amount)
                .map(|output| (index, output))
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}
