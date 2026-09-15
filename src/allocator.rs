use crate::{
    Amount,
    router::{PathSet, Router},
    solver::{Allocation, GasValuation},
};

/// Generic simulator-backed marginal allocator used when no certified family applies.
pub fn allocate_by_pieces(
    router: &Router<'_>,
    paths: &PathSet,
    portfolio: &[usize],
    total: Amount,
    steps: u32,
    gas_valuation: GasValuation,
) -> Option<Vec<Allocation>> {
    let mut amounts = vec![Amount::ZERO; portfolio.len()];
    let mut outputs = vec![Amount::ZERO; portfolio.len()];
    let divisor = Amount::from(steps);
    let piece = total / divisor;
    let mut pieces = Vec::with_capacity(steps as usize + 1);
    if piece.is_zero() {
        pieces.push(total);
    } else {
        let remainder = total % divisor;
        if !remainder.is_zero() {
            pieces.push(remainder);
        }
        pieces.extend(std::iter::repeat_n(piece, steps as usize));
    }

    for piece in pieces {
        if piece.is_zero() {
            continue;
        }
        let mut best = None;
        for (slot, &path_index) in portfolio.iter().enumerate() {
            let proposed = amounts[slot].checked_add(piece)?;
            let Some(proposed_output) = router.replay_path(&paths.paths[path_index], proposed)
            else {
                continue;
            };
            // Seed every portfolio using gross marginal output. Charging activation gas here can
            // starve a path before coordinate refinement gets a chance to evaluate a profitable
            // split. Gas belongs in refinement and final selection, matching the original.
            let marginal = proposed_output.saturating_sub(outputs[slot]);
            if best
                .as_ref()
                .is_none_or(|&(_, _, best_marginal)| marginal > best_marginal)
            {
                best = Some((slot, proposed_output, marginal));
            }
        }
        let (slot, output, _) = best?;
        amounts[slot] += piece;
        outputs[slot] = output;
    }

    let coarse = portfolio
        .iter()
        .enumerate()
        .filter(|(slot, _)| !amounts[*slot].is_zero())
        .map(|(slot, &path_index)| Allocation {
            path_index,
            amount_in: amounts[slot],
            amount_out: outputs[slot],
        })
        .collect::<Vec<_>>();
    refine_simulated_allocations(router, paths, coarse, total, gas_valuation)
}

/// Ports the original router's simulator-backed pairwise coordinate refinement.
fn refine_simulated_allocations(
    router: &Router<'_>,
    paths: &PathSet,
    mut best: Vec<Allocation>,
    total: Amount,
    gas_valuation: GasValuation,
) -> Option<Vec<Allocation>> {
    for _ in 0..4 {
        let mut changed = false;
        for left in 0..best.len() {
            for right in left + 1..best.len() {
                let pair_total = best[left].amount_in.checked_add(best[right].amount_in)?;
                if pair_total.is_zero() {
                    continue;
                }
                let left_path = best[left].path_index;
                let right_path = best[right].path_index;
                let split = golden_section_search(
                    |fraction| {
                        let (left_amount, right_amount) = split_amount(pair_total, fraction);
                        pair_score(
                            router,
                            paths,
                            left_path,
                            right_path,
                            left_amount,
                            right_amount,
                            gas_valuation,
                        )
                        .map_or(f64::NEG_INFINITY, amount_as_f64)
                    },
                    16,
                );
                let (interior_left, interior_right) = split_amount(pair_total, split);
                let candidates = [
                    (interior_left, interior_right),
                    (pair_total, Amount::ZERO),
                    (Amount::ZERO, pair_total),
                ];
                let old_score = pair_score(
                    router,
                    paths,
                    left_path,
                    right_path,
                    best[left].amount_in,
                    best[right].amount_in,
                    gas_valuation,
                )?;
                let replacement = candidates
                    .into_iter()
                    .filter_map(|(left_amount, right_amount)| {
                        let (left_output, left_gas) =
                            replay_or_zero_with_gas(router, paths, left_path, left_amount)?;
                        let (right_output, right_gas) =
                            replay_or_zero_with_gas(router, paths, right_path, right_amount)?;
                        let output = left_output.checked_add(right_output)?;
                        let gas = left_gas.checked_add(right_gas)?;
                        let score = gas_valuation.net(output, gas);
                        Some((score, left_amount, left_output, right_amount, right_output))
                    })
                    .max_by_key(|candidate| candidate.0);
                if let Some((score, left_amount, left_output, right_amount, right_output)) =
                    replacement
                    && score > old_score
                {
                    best[left].amount_in = left_amount;
                    best[left].amount_out = left_output;
                    best[right].amount_in = right_amount;
                    best[right].amount_out = right_output;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    best.retain(|allocation| !allocation.amount_in.is_zero());
    debug_assert_eq!(
        best.iter()
            .map(|allocation| allocation.amount_in)
            .sum::<Amount>(),
        total
    );
    Some(best)
}

fn pair_score(
    router: &Router<'_>,
    paths: &PathSet,
    left_path: usize,
    right_path: usize,
    left_amount: Amount,
    right_amount: Amount,
    gas_valuation: GasValuation,
) -> Option<Amount> {
    let (left_output, left_gas) = replay_or_zero_with_gas(router, paths, left_path, left_amount)?;
    let (right_output, right_gas) =
        replay_or_zero_with_gas(router, paths, right_path, right_amount)?;
    gas_valuation
        .net(
            left_output.checked_add(right_output)?,
            left_gas.checked_add(right_gas)?,
        )
        .into()
}

fn replay_or_zero_with_gas(
    router: &Router<'_>,
    paths: &PathSet,
    path: usize,
    amount: Amount,
) -> Option<(Amount, Amount)> {
    if amount.is_zero() {
        Some((Amount::ZERO, Amount::ZERO))
    } else {
        router.replay_path_with_gas(&paths.paths[path], amount)
    }
}

fn golden_section_search(mut evaluate: impl FnMut(f64) -> f64, max_evals: usize) -> f64 {
    let inverse_phi = (5_f64.sqrt() - 1.0) / 2.0;
    let (mut low, mut high) = (0.0, 1.0);
    let mut left = high - inverse_phi * (high - low);
    let mut right = low + inverse_phi * (high - low);
    let mut left_score = evaluate(left);
    let mut right_score = evaluate(right);
    for _ in 0..max_evals.saturating_sub(2) {
        if left_score < right_score {
            low = left;
            left = right;
            left_score = right_score;
            right = low + inverse_phi * (high - low);
            right_score = evaluate(right);
        } else {
            high = right;
            right = left;
            right_score = left_score;
            left = high - inverse_phi * (high - low);
            left_score = evaluate(left);
        }
    }
    if left_score >= right_score {
        left
    } else {
        right
    }
}

fn split_amount(total: Amount, fraction: f64) -> (Amount, Amount) {
    let scale = 1_000_000_000_000_000_000_u64;
    let numerator = (fraction.clamp(0.0, 1.0) * scale as f64) as u64;
    let part = total * Amount::from(numerator) / Amount::from(scale);
    (part, total - part)
}

fn amount_as_f64(amount: Amount) -> f64 {
    amount.to_string().parse().unwrap_or(f64::INFINITY)
}
