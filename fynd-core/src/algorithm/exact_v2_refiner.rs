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

const CERTIFIED_INTERVAL_DEPTH: usize = 6;

#[derive(Clone)]
struct ContinuousCpmm {
    a: BigUint,
    b: BigUint,
    c: BigUint,
}

#[derive(Clone)]
struct PositiveFraction {
    numerator: BigUint,
    denominator: BigUint,
}

impl PositiveFraction {
    fn new(numerator: BigUint, denominator: BigUint) -> Option<Self> {
        if denominator.is_zero() { None } else { Some(Self { numerator, denominator }) }
    }

    fn add(&self, other: &Self) -> Self {
        Self {
            numerator: &self.numerator * &other.denominator + &other.numerator * &self.denominator,
            denominator: &self.denominator * &other.denominator,
        }
    }

    fn mul_uint(&self, value: &BigUint) -> Self {
        Self { numerator: &self.numerator * value, denominator: self.denominator.clone() }
    }

    fn ceil(&self) -> BigUint {
        if self.numerator.is_zero() { return BigUint::zero(); }
        (&self.numerator + &self.denominator - BigUint::from(1u8)) / &self.denominator
    }
}

pub(super) fn refine_disjoint_allocations(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
) -> Result<Option<Vec<PathAllocation>>, AlgorithmError> {
    if current.len() < 2 || !paths_are_pool_disjoint(current) { return Ok(None); }

    if std::env::var_os("FYND_V2_CERTIFIED_INTERVAL_CENSUS").is_some() && current.len() == 2 {
        emit_v2_certified_interval_census(current, total, market);
    }

    if std::env::var_os("FYND_V2_CERTIFIED_PRUNE").is_some() && current.len() == 2 {
        if let Some(upper) = certified_v2_pair_upper(current, total, market) {
            let incumbent_index = if current[1].amount_out > current[0].amount_out { 1 } else { 0 };
            let incumbent = &current[incumbent_index].amount_out;
            if upper <= *incumbent {
                if std::env::var_os("FYND_V2_CERTIFIED_PRUNE_TRACE").is_some() {
                    eprintln!("v2-certified-prune: upper={} incumbent={} left_hops={} right_hops={}", upper, incumbent, current[0].hops.len(), current[1].hops.len());
                }
                return Ok(Some(vec![current[incumbent_index].clone()]));
            }
        }
    }

    if let Some(refined) = allocate_uniswap_v2_paths(current, total, market)? { return Ok(Some(refined)); }
    refine_simulated_paths(current, total, market)
}

fn paths_are_pool_disjoint(paths: &[PathAllocation]) -> bool {
    let mut components = FxHashSet::default();
    paths.iter().flat_map(|path| &path.hops).all(|hop| components.insert(hop.descriptor.component_id.clone()))
}

fn replay_path(path: &PathAllocation, amount_in: BigUint, total: &BigUint, market: &MarketState) -> Result<PathAllocation, AlgorithmError> {
    let descriptors = path.hops.iter().map(|hop| hop.descriptor.clone()).collect::<Vec<HopDescriptor>>();
    let sim = simulate_path(&descriptors, &amount_in, market, &MarketOverrides::empty())?;
    let mut replayed = path.clone();
    replayed.flow_fraction = amount_in.to_f64().unwrap_or(0.0) / total.to_f64().unwrap_or(1.0);
    replayed.amount_in = amount_in;
    replayed.amount_out = sim.amount_out;
    replayed.marginal_price_product = sim.marginal_price_product;
    for (hop, (amount_out, gas)) in replayed.hops.iter_mut().zip(sim.hop_results) {
        hop.amount_out = amount_out;
        hop.gas = gas;
    }
    Ok(replayed)
}

fn zero_path(path: &PathAllocation) -> PathAllocation {
    let mut zeroed = path.clone();
    zeroed.flow_fraction = 0.0;
    zeroed.amount_in = BigUint::zero();
    zeroed.amount_out = BigUint::zero();
    zeroed.marginal_price_product = 0.0;
    for hop in &mut zeroed.hops { hop.amount_out = BigUint::zero(); hop.gas = BigUint::zero(); }
    zeroed
}

fn output(paths: &[PathAllocation]) -> BigUint { paths.iter().fold(BigUint::zero(), |sum, path| sum + &path.amount_out) }
fn input(paths: &[PathAllocation]) -> BigUint { paths.iter().fold(BigUint::zero(), |sum, path| sum + &path.amount_in) }

fn feasible_seed(current: &[PathAllocation], total: &BigUint, market: &MarketState) -> Result<Vec<PathAllocation>, AlgorithmError> {
    if input(current) == *total { return Ok(current.to_vec()); }
    let count = BigUint::from(current.len() as u64);
    let base = total / &count;
    let remainder = (total % &count).to_usize().unwrap_or(0);
    current.iter().enumerate().map(|(index, path)| {
        let mut amount = base.clone();
        if index < remainder { amount += BigUint::from(1u8); }
        replay_path(path, amount, total, market)
    }).collect()
}

fn refine_simulated_paths(current: &[PathAllocation], total: &BigUint, market: &MarketState) -> Result<Option<Vec<PathAllocation>>, AlgorithmError> {
    let mut best = feasible_seed(current, total, market)?;
    for _ in 0..4 {
        let mut changed = false;
        for left in 0..best.len() {
            for right in left + 1..best.len() {
                let pair_total = &best[left].amount_in + &best[right].amount_in;
                if pair_total.is_zero() { continue; }
                let split = golden_section_search(|fraction| {
                    let (left_amount, right_amount) = split_amount(&pair_total, fraction);
                    let left_out = replay_path(&best[left], left_amount, total, market).map(|path| path.amount_out).unwrap_or_default();
                    let right_out = replay_path(&best[right], right_amount, total, market).map(|path| path.amount_out).unwrap_or_default();
                    (&left_out + &right_out).to_f64().unwrap_or(f64::INFINITY)
                }, 0.0, 1.0, 16);
                let (interior_left_amount, interior_right_amount) = split_amount(&pair_total, split);
                let interior_left = replay_path(&best[left], interior_left_amount, total, market)?;
                let interior_right = replay_path(&best[right], interior_right_amount, total, market)?;
                let interior_output = &interior_left.amount_out + &interior_right.amount_out;
                let left_boundary_left = replay_path(&best[left], pair_total.clone(), total, market)?;
                let left_boundary_right = zero_path(&best[right]);
                let left_boundary_output = left_boundary_left.amount_out.clone();
                let right_boundary_left = zero_path(&best[left]);
                let right_boundary_right = replay_path(&best[right], pair_total.clone(), total, market)?;
                let right_boundary_output = right_boundary_right.amount_out.clone();
                let (left_path, right_path, new_pair) = if left_boundary_output > interior_output && left_boundary_output >= right_boundary_output {
                    (left_boundary_left, left_boundary_right, left_boundary_output)
                } else if right_boundary_output > interior_output {
                    (right_boundary_left, right_boundary_right, right_boundary_output)
                } else { (interior_left, interior_right, interior_output) };
                let old_pair = &best[left].amount_out + &best[right].amount_out;
                if new_pair > old_pair { best[left] = left_path; best[right] = right_path; changed = true; }
            }
        }
        if !changed { break; }
    }
    best.retain(|path| !path.amount_in.is_zero());
    Ok(Some(best))
}

impl ContinuousCpmm {
    fn one_hop(state: &UniswapV2State, zero_to_one: bool) -> Self {
        let (reserve_in, reserve_out) = if zero_to_one { (state.reserve0, state.reserve1) } else { (state.reserve1, state.reserve0) };
        let fee_num = BigUint::from(9_970u32);
        let fee_den = BigUint::from(10_000u32);
        Self {
            a: BigUint::from_bytes_be(&reserve_out.to_be_bytes::<32>()) * &fee_num,
            b: BigUint::from_bytes_be(&reserve_in.to_be_bytes::<32>()) * fee_den,
            c: fee_num,
        }
    }
    fn compose(first: &Self, second: &Self) -> Self {
        Self { a: &first.a * &second.a, b: &first.b * &second.b, c: &second.b * &first.c + &second.c * &first.a }
    }
    fn value(&self, amount: &BigUint) -> Option<PositiveFraction> {
        PositiveFraction::new(&self.a * amount, &self.b + &self.c * amount)
    }
    fn derivative_fraction(&self, amount: &BigUint) -> Option<PositiveFraction> {
        let base = &self.b + &self.c * amount;
        PositiveFraction::new(&self.a * &self.b, &base * &base)
    }
    fn allocation_at_marginal(&self, marginal: f64) -> f64 {
        let (Some(a), Some(b), Some(c)) = (self.a.to_f64(), self.b.to_f64(), self.c.to_f64()) else { return 0.0; };
        if marginal <= 0.0 || b == 0.0 || c == 0.0 { return 0.0; }
        ((a * b / marginal).sqrt() - b).max(0.0) / c
    }
    fn initial_slope(&self) -> f64 {
        let (Some(a), Some(b)) = (self.a.to_f64(), self.b.to_f64()) else { return f64::INFINITY; };
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
        let state = market.get_simulation_state(&descriptor.component_id)?.as_any().downcast_ref::<UniswapV2State>()?;
        let hop_curve = ContinuousCpmm::one_hop(state, descriptor.token_in.address < descriptor.token_out.address);
        curve = Some(match curve { Some(previous) => ContinuousCpmm::compose(&previous, &hop_curve), None => hop_curve });
    }
    curve
}

fn certified_v2_interval_upper_from_curves(left: &ContinuousCpmm, right: &ContinuousCpmm, total: &BigUint, lo: &BigUint, hi: &BigUint) -> Option<BigUint> {
    if lo > hi || hi > total { return None; }
    let midpoint = (lo + hi) / BigUint::from(2u8);
    let left_amount = total - &midpoint;
    let value_at_mid = left.value(&left_amount)?.add(&right.value(&midpoint)?);
    let left_derivative = left.derivative_fraction(&left_amount)?;
    let right_derivative = right.derivative_fraction(&midpoint)?;
    let right_ge_left = &right_derivative.numerator * &left_derivative.denominator >= &left_derivative.numerator * &right_derivative.denominator;
    let slope_magnitude = if right_ge_left {
        PositiveFraction::new(&right_derivative.numerator * &left_derivative.denominator - &left_derivative.numerator * &right_derivative.denominator, &right_derivative.denominator * &left_derivative.denominator)?
    } else {
        PositiveFraction::new(&left_derivative.numerator * &right_derivative.denominator - &right_derivative.numerator * &left_derivative.denominator, &right_derivative.denominator * &left_derivative.denominator)?
    };
    let endpoint_distance = if right_ge_left { hi - &midpoint } else { &midpoint - lo };
    Some(value_at_mid.add(&slope_magnitude.mul_uint(&endpoint_distance)).ceil())
}

fn certified_v2_pair_upper(current: &[PathAllocation], total: &BigUint, market: &MarketState) -> Option<BigUint> {
    if current.len() != 2 || total.is_zero() { return None; }
    let left = path_curve(&current[0], market)?;
    let right = path_curve(&current[1], market)?;
    certified_v2_interval_upper_from_curves(&left, &right, total, &BigUint::zero(), total)
}

#[derive(Default)]
struct V2CertifiedIntervalStats {
    intervals: usize,
    certified_dead: usize,
    unresolved_leaves: usize,
    dead_width: BigUint,
    dead_by_depth: [usize; CERTIFIED_INTERVAL_DEPTH + 1],
    falsifier_probes: usize,
    falsifier_unknown: usize,
    falsifier_violations: usize,
}

fn exact_pair_output(current: &[PathAllocation], total: &BigUint, market: &MarketState, x: &BigUint) -> Option<BigUint> {
    if current.len() != 2 || x > total { return None; }
    let left = replay_path(&current[0], total - x, total, market).ok()?;
    let right = replay_path(&current[1], x.clone(), total, market).ok()?;
    Some(left.amount_out + right.amount_out)
}

fn analyze_v2_certified_interval(current: &[PathAllocation], left: &ContinuousCpmm, right: &ContinuousCpmm, total: &BigUint, market: &MarketState, incumbent: &BigUint, lo: BigUint, hi: BigUint, depth: usize, stats: &mut V2CertifiedIntervalStats) {
    stats.intervals += 1;
    let Some(upper) = certified_v2_interval_upper_from_curves(left, right, total, &lo, &hi) else { stats.unresolved_leaves += 1; return; };
    let midpoint = (&lo + &hi) / BigUint::from(2u8);
    if upper <= *incumbent {
        stats.certified_dead += 1;
        stats.dead_by_depth[depth] += 1;
        stats.dead_width += &hi - &lo;
        for x in [&lo, &midpoint, &hi] {
            match exact_pair_output(current, total, market, x) {
                Some(exact) => { stats.falsifier_probes += 1; if exact > *incumbent { stats.falsifier_violations += 1; } }
                None => stats.falsifier_unknown += 1,
            }
        }
        return;
    }
    if depth == CERTIFIED_INTERVAL_DEPTH || lo == hi { stats.unresolved_leaves += 1; return; }
    analyze_v2_certified_interval(current, left, right, total, market, incumbent, lo.clone(), midpoint.clone(), depth + 1, stats);
    analyze_v2_certified_interval(current, left, right, total, market, incumbent, midpoint, hi, depth + 1, stats);
}

fn emit_v2_certified_interval_census(current: &[PathAllocation], total: &BigUint, market: &MarketState) {
    if current.len() != 2 || total.is_zero() { return; }
    let (Some(left), Some(right)) = (path_curve(&current[0], market), path_curve(&current[1], market)) else { return; };
    let incumbent = current[0].amount_out.clone().max(current[1].amount_out.clone());
    let mut stats = V2CertifiedIntervalStats::default();
    analyze_v2_certified_interval(current, &left, &right, total, market, &incumbent, BigUint::zero(), total.clone(), 0, &mut stats);
    let dead_bps = ((&stats.dead_width * BigUint::from(10_000u64)) / total).to_u64().unwrap_or(10_000).min(10_000);
    eprintln!("\n=== V2CertifiedIntervalCensusV1 ===");
    eprintln!("left hops:                    {}", current[0].hops.len());
    eprintln!("right hops:                   {}", current[1].hops.len());
    eprintln!("incumbent gross raw:          {}", incumbent);
    eprintln!("interval nodes examined:      {}", stats.intervals);
    eprintln!("certified-dead intervals:     {}", stats.certified_dead);
    eprintln!("unresolved depth-6 leaves:    {}", stats.unresolved_leaves);
    eprintln!("allocation width dead:        {}.{:02}%", dead_bps / 100, dead_bps % 100);
    eprintln!("exact falsifier probes:       {}", stats.falsifier_probes);
    eprintln!("falsifier unknown probes:     {}", stats.falsifier_unknown);
    eprintln!("certificate violations:       {}", stats.falsifier_violations);
    eprintln!("dead intervals by depth:");
    for (depth, count) in stats.dead_by_depth.iter().enumerate() { eprintln!("  depth {}: {}", depth, count); }
    eprintln!("=== end V2CertifiedIntervalCensusV1 ===\n");
}

fn allocations(curves: &[ContinuousCpmm], total: &BigUint) -> Option<Vec<BigUint>> {
    let total_f = total.to_f64()?;
    let mut low = 0.0;
    let mut high = curves.iter().map(ContinuousCpmm::initial_slope).fold(0.0, f64::max);
    if !high.is_finite() { return None; }
    for _ in 0..96 {
        let mid = (low + high) / 2.0;
        let assigned = curves.iter().map(|curve| curve.allocation_at_marginal(mid).min(total_f)).sum::<f64>();
        if assigned > total_f { low = mid; } else { high = mid; }
    }
    let mut result = curves.iter().map(|curve| BigUint::from(curve.allocation_at_marginal(high).clamp(0.0, total_f) as u128)).collect::<Vec<_>>();
    let assigned = result.iter().fold(BigUint::zero(), |sum, value| sum + value);
    if assigned <= *total {
        let remainder = total - assigned;
        let best = result.iter().enumerate().max_by(|(lhs, _), (rhs, _)| {
            let (lhs_num, lhs_den) = curves[*lhs].derivative(&result[*lhs]);
            let (rhs_num, rhs_den) = curves[*rhs].derivative(&result[*rhs]);
            (lhs_num * rhs_den).cmp(&(rhs_num * lhs_den))
        })?.0;
        result[best] += remainder;
    } else {
        let mut excess = assigned - total;
        for value in result.iter_mut().rev() {
            let removed = value.clone().min(excess.clone());
            *value -= &removed;
            excess -= removed;
            if excess.is_zero() { break; }
        }
    }
    Some(result)
}

pub(super) fn allocate_uniswap_v2_paths(current: &[PathAllocation], total: &BigUint, market: &MarketState) -> Result<Option<Vec<PathAllocation>>, AlgorithmError> {
    if current.len() < 2 { return Ok(None); }
    let mut components = FxHashSet::default();
    for path in current {
        for hop in &path.hops {
            if !components.insert(hop.descriptor.component_id.clone()) { return Ok(None); }
        }
    }
    let Some(curves) = current.iter().map(|path| path_curve(path, market)).collect::<Option<Vec<_>>>() else { return Ok(None); };
    let Some(amounts) = allocations(&curves, total) else { return Ok(None); };
    let mut refined = current.to_vec();
    for (path, amount_in) in refined.iter_mut().zip(amounts) {
        let descriptors = path.hops.iter().map(|hop| hop.descriptor.clone()).collect::<Vec<HopDescriptor>>();
        let sim = simulate_path(&descriptors, &amount_in, market, &MarketOverrides::empty())?;
        path.flow_fraction = amount_in.to_f64().unwrap_or(0.0) / total.to_f64().unwrap_or(1.0);
        path.amount_in = amount_in;
        path.amount_out = sim.amount_out;
        path.marginal_price_product = sim.marginal_price_product;
        for (hop, (amount_out, gas)) in path.hops.iter_mut().zip(sim.hop_results) { hop.amount_out = amount_out; hop.gas = gas; }
    }
    refined.retain(|path| !path.amount_in.is_zero());
    Ok(Some(refined))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_splits_equal_curves_exactly() {
        let curve = ContinuousCpmm { a: BigUint::from(997_000u32), b: BigUint::from(1_000_000u32), c: BigUint::from(997u32) };
        let result = allocations(&[curve.clone(), curve], &BigUint::from(50u32)).unwrap();
        assert_eq!(result, [BigUint::from(25u32), BigUint::from(25u32)]);
    }

    #[test]
    fn tangent_upper_bounds_equal_curves_at_endpoints() {
        let curve = ContinuousCpmm { a: BigUint::from(997_000u32), b: BigUint::from(1_000_000u32), c: BigUint::from(997u32) };
        let total = BigUint::from(50u32);
        let midpoint = &total / BigUint::from(2u8);
        let left_amount = &total - &midpoint;
        let value = curve.value(&left_amount).unwrap().add(&curve.value(&midpoint).unwrap());
        let upper = value.ceil();
        let endpoint = curve.value(&total).unwrap().ceil();
        assert!(upper >= endpoint);
    }

    #[test]
    fn child_interval_tangent_need_not_be_monotone() {
        let curve = ContinuousCpmm { a: BigUint::from(997_000u32), b: BigUint::from(1_000_000u32), c: BigUint::from(997u32) };
        let total = BigUint::from(100u32);
        let root = certified_v2_interval_upper_from_curves(&curve, &curve, &total, &BigUint::zero(), &total).unwrap();
        let half = certified_v2_interval_upper_from_curves(&curve, &curve, &total, &BigUint::zero(), &BigUint::from(50u32)).unwrap();
        assert!(half >= root);

        let x0 = BigUint::zero();
        let x1 = BigUint::from(25u32);
        let x2 = BigUint::from(50u32);
        for x in [&x0, &x1, &x2] {
            let exact_continuous = curve.value(&(&total - x)).unwrap().add(&curve.value(x).unwrap()).ceil();
            assert!(half >= exact_continuous);
        }
    }
}
