//! Exact replay refinement for pool-disjoint paths.
//!
//! Uniswap V2 paths use their closed-form CPMM curves. Other simulated paths,
//! including Uniswap V3, use pairwise coordinate ascent over exact simulator
//! replays, so concentrated-liquidity tick crossings are never approximated as
//! constant-product reserves.

use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};
use rustc_hash::FxHashSet;
use tycho_simulation::evm::protocol::uniswap_v2::state::UniswapV2State;

use super::{
    split_primitives::{
        golden_section_search, simulate_path, split_amount, HopDescriptor, MarketOverrides,
        PathAllocation,
    },
    AlgorithmError,
};
use crate::feed::market_data::MarketState;

#[derive(Clone)]
struct ContinuousCpmm {
    // Continuous no-floor envelope q(x) = a*x / (b + c*x).
    a: BigUint,
    b: BigUint,
    c: BigUint,
}

/// Refines pool-disjoint allocations. Pure V2 sets retain the closed-form
/// allocator; mixed or V3 sets are refined with exact simulated replay.
pub(super) fn refine_disjoint_allocations(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
) -> Result<Option<Vec<PathAllocation>>, AlgorithmError> {
    if current.len() < 2 || !paths_are_pool_disjoint(current) {
        return Ok(None);
    }

    if let Some(refined) = allocate_uniswap_v2_paths(current, total, market)? {
        return Ok(Some(refined));
    }
    refine_simulated_paths(current, total, market)
}

fn paths_are_pool_disjoint(paths: &[PathAllocation]) -> bool {
    let mut components = FxHashSet::default();
    paths
        .iter()
        .flat_map(|path| &path.hops)
        .all(|hop| components.insert(hop.descriptor.component_id.clone()))
}

fn replay_path(
    path: &PathAllocation,
    amount_in: BigUint,
    total: &BigUint,
    market: &MarketState,
) -> Result<PathAllocation, AlgorithmError> {
    let descriptors = path
        .hops
        .iter()
        .map(|hop| hop.descriptor.clone())
        .collect::<Vec<HopDescriptor>>();
    let sim = simulate_path(&descriptors, &amount_in, market, &MarketOverrides::empty())?;
    let mut replayed = path.clone();
    replayed.flow_fraction = amount_in.to_f64().unwrap_or(0.0) / total.to_f64().unwrap_or(1.0);
    replayed.amount_in = amount_in;
    replayed.amount_out = sim.amount_out;
    replayed.marginal_price_product = sim.marginal_price_product;
    for (hop, (amount_out, gas)) in replayed
        .hops
        .iter_mut()
        .zip(sim.hop_results)
    {
        hop.amount_out = amount_out;
        hop.gas = gas;
    }
    Ok(replayed)
}

fn output(paths: &[PathAllocation]) -> BigUint {
    paths
        .iter()
        .fold(BigUint::zero(), |sum, path| sum + &path.amount_out)
}

/// Improves an existing V2/V3 split using exact simulator replays. Each
/// coordinate step preserves the pair's total input, then accepts only an
/// exact integer-output improvement. This is deliberately conservative: the
/// caller still compares the resulting route's post-gas net output with the
/// native route before selecting it.
fn refine_simulated_paths(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
) -> Result<Option<Vec<PathAllocation>>, AlgorithmError> {
    let mut best = current.to_vec();
    let baseline = output(&best);

    // A small fixed number of coordinate passes keeps V3 replay bounded inside
    // the worker timeout while handling splits with more than two paths.
    for _ in 0..3 {
        let mut changed = false;
        for left in 0..best.len() {
            for right in left + 1..best.len() {
                let pair_total = &best[left].amount_in + &best[right].amount_in;
                if pair_total.is_zero() {
                    continue;
                }

                let split = golden_section_search(
                    |fraction| {
                        let (left_amount, right_amount) = split_amount(&pair_total, fraction);
                        let left_out = replay_path(&best[left], left_amount, total, market)
                            .map(|path| path.amount_out)
                            .unwrap_or_default();
                        let right_out = replay_path(&best[right], right_amount, total, market)
                            .map(|path| path.amount_out)
                            .unwrap_or_default();
                        (&left_out + &right_out)
                            .to_f64()
                            .unwrap_or(f64::INFINITY)
                    },
                    0.0,
                    1.0,
                    16,
                );
                let (left_amount, right_amount) = split_amount(&pair_total, split);
                let left_path = replay_path(&best[left], left_amount, total, market)?;
                let right_path = replay_path(&best[right], right_amount, total, market)?;

                let old_pair = &best[left].amount_out + &best[right].amount_out;
                let new_pair = &left_path.amount_out + &right_path.amount_out;
                if new_pair > old_pair {
                    best[left] = left_path;
                    best[right] = right_path;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    Ok((output(&best) > baseline).then_some(best))
}

impl ContinuousCpmm {
    fn one_hop(state: &UniswapV2State, zero_to_one: bool) -> Self {
        let (reserve_in, reserve_out) = if zero_to_one {
            (state.reserve0, state.reserve1)
        } else {
            (state.reserve1, state.reserve0)
        };
        let fee_num = BigUint::from(9_970u32);
        let fee_den = BigUint::from(10_000u32);
        Self {
            a: BigUint::from_bytes_be(&reserve_out.to_be_bytes::<32>()) * &fee_num,
            b: BigUint::from_bytes_be(&reserve_in.to_be_bytes::<32>()) * fee_den,
            c: fee_num,
        }
    }

    fn compose(first: &Self, second: &Self) -> Self {
        Self {
            a: &first.a * &second.a,
            b: &first.b * &second.b,
            c: &second.b * &first.c + &second.c * &first.a,
        }
    }

    fn allocation_at_marginal(&self, marginal: f64) -> f64 {
        let (Some(a), Some(b), Some(c)) = (self.a.to_f64(), self.b.to_f64(), self.c.to_f64())
        else {
            return 0.0;
        };
        if marginal <= 0.0 || b == 0.0 || c == 0.0 {
            return 0.0;
        }
        ((a * b / marginal).sqrt() - b).max(0.0) / c
    }

    fn initial_slope(&self) -> f64 {
        let (Some(a), Some(b)) = (self.a.to_f64(), self.b.to_f64()) else {
            return f64::INFINITY;
        };
        a / b
    }

    fn derivative(&self, amount: &BigUint) -> (BigUint, BigUint) {
        let base = &self.b + &self.c * amount;
        (&self.a * &self.b, &base * &base)
    }
}

fn path_curve(path: &PathAllocation, market: &MarketState) -> Option<ContinuousCpmm> {
    let mut curve = None;
    for hop in &path.hops {
        let descriptor = &hop.descriptor;
        let state = market
            .get_simulation_state(&descriptor.component_id)?
            .as_any()
            .downcast_ref::<UniswapV2State>()?;
        let hop_curve = ContinuousCpmm::one_hop(
            state,
            descriptor.token_in.address < descriptor.token_out.address,
        );
        curve = Some(match curve {
            Some(previous) => ContinuousCpmm::compose(&previous, &hop_curve),
            None => hop_curve,
        });
    }
    curve
}

fn allocations(curves: &[ContinuousCpmm], total: &BigUint) -> Option<Vec<BigUint>> {
    let total_f = total.to_f64()?;
    let mut low = 0.0;
    let mut high = curves
        .iter()
        .map(ContinuousCpmm::initial_slope)
        .fold(0.0, f64::max);
    if !high.is_finite() {
        return None;
    }
    for _ in 0..96 {
        let mid = (low + high) / 2.0;
        let assigned = curves
            .iter()
            .map(|curve| {
                curve
                    .allocation_at_marginal(mid)
                    .min(total_f)
            })
            .sum::<f64>();
        if assigned > total_f {
            low = mid;
        } else {
            high = mid;
        }
    }

    let mut result = curves
        .iter()
        .map(|curve| {
            BigUint::from(
                curve
                    .allocation_at_marginal(high)
                    .clamp(0.0, total_f) as u128,
            )
        })
        .collect::<Vec<_>>();
    let assigned = result
        .iter()
        .fold(BigUint::zero(), |sum, value| sum + value);
    if assigned <= *total {
        let remainder = total - assigned;
        let best = result
            .iter()
            .enumerate()
            .max_by(|(lhs, _), (rhs, _)| {
                let (lhs_num, lhs_den) = curves[*lhs].derivative(&result[*lhs]);
                let (rhs_num, rhs_den) = curves[*rhs].derivative(&result[*rhs]);
                (lhs_num * rhs_den).cmp(&(rhs_num * lhs_den))
            })?
            .0;
        result[best] += remainder;
    } else {
        let mut excess = assigned - total;
        for value in result.iter_mut().rev() {
            let removed = value.clone().min(excess.clone());
            *value -= &removed;
            excess -= removed;
            if excess.is_zero() {
                break;
            }
        }
    }
    Some(result)
}

pub(super) fn allocate_uniswap_v2_paths(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
) -> Result<Option<Vec<PathAllocation>>, AlgorithmError> {
    if current.len() < 2 {
        return Ok(None);
    }

    let mut components = FxHashSet::default();
    for path in current {
        for hop in &path.hops {
            if !components.insert(hop.descriptor.component_id.clone()) {
                return Ok(None);
            }
        }
    }
    let Some(curves) = current
        .iter()
        .map(|path| path_curve(path, market))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    let Some(amounts) = allocations(&curves, total) else { return Ok(None) };

    let mut refined = current.to_vec();
    for (path, amount_in) in refined.iter_mut().zip(amounts) {
        let descriptors = path
            .hops
            .iter()
            .map(|hop| hop.descriptor.clone())
            .collect::<Vec<HopDescriptor>>();
        let sim = simulate_path(&descriptors, &amount_in, market, &MarketOverrides::empty())?;
        path.flow_fraction = amount_in.to_f64().unwrap_or(0.0) / total.to_f64().unwrap_or(1.0);
        path.amount_in = amount_in;
        path.amount_out = sim.amount_out;
        path.marginal_price_product = sim.marginal_price_product;
        for (hop, (amount_out, gas)) in path
            .hops
            .iter_mut()
            .zip(sim.hop_results)
        {
            hop.amount_out = amount_out;
            hop.gas = gas;
        }
    }
    refined.retain(|path| !path.amount_in.is_zero());

    Ok(Some(refined))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_splits_equal_curves_exactly() {
        let curve = ContinuousCpmm {
            a: BigUint::from(997_000u32),
            b: BigUint::from(1_000_000u32),
            c: BigUint::from(997u32),
        };
        let result = allocations(&[curve.clone(), curve], &BigUint::from(50u32)).unwrap();
        assert_eq!(result, [BigUint::from(25u32), BigUint::from(25u32)]);
    }
}
