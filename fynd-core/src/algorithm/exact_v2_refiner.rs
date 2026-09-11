//! Exact replay refinement for pool-disjoint paths.
//!
//! Uniswap V2 paths use their closed-form CPMM curves. Other simulated paths,
//! including Uniswap V3, use pairwise coordinate ascent over exact simulator
//! replays, so concentrated-liquidity tick crossings are never approximated as
//! constant-product reserves.

use std::time::{Duration, Instant};

use alloy::primitives::Address;
use num_bigint::{BigInt, BigUint};
use num_traits::{ToPrimitive, Zero};
use rustc_hash::FxHashSet;
use tycho_simulation::evm::protocol::{
    uniswap_v2::state::UniswapV2State,
    uniswap_v3::{enums::FeeAmount, state::UniswapV3State},
    uniswap_v4::state::UniswapV4State,
};

use super::{
    split_primitives::{
        golden_section_search, simulate_path, split_amount, HopDescriptor, MarketOverrides,
        PathAllocation,
    },
    AlgorithmError,
};
use crate::feed::market_data::MarketState;

#[path = "v2_certified_interval_allocator.rs"]
mod v2_certified_interval_allocator;

const CERTIFIED_INTERVAL_DEPTH: usize = 6;
const CERTIFIED_PAIR_INTERVAL_BUDGET: usize = 16_384;
const SACRED_TIMELINE_CUTOFFS_CENTI_BPS: [u32; 5] = [0, 1, 10, 50, 100];

pub(super) enum CertifiedPairSolve {
    Complete(Vec<PathAllocation>),
    Unsupported,
    BudgetExceeded,
}

/// Globally optimizes one supported two-path allocation face.
pub(super) fn certified_pair_allocation(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
    gas_valuation: Option<&ExactGasValuation>,
    cutoff_centi_bps: u32,
) -> Result<CertifiedPairSolve, AlgorithmError> {
    let seed = feasible_seed(current, total, market)?;
    certified_pair_branch_and_bound(current, &seed, total, market, gas_valuation, cutoff_centi_bps)
}

fn sacred_timeline_allocation_census_enabled() -> bool {
    std::env::var_os("FYND_SACRED_TIMELINE_ALLOCATION_CENSUS").is_some()
}

#[derive(Default)]
struct SacredTimelineAllocationStats {
    intervals: usize,
    bound_pruned: usize,
    singleton_intervals: usize,
    incumbent_improvements: usize,
    envelope_replays: usize,
    exact_leaf_replays: usize,
    unsupported: bool,
    budget_exceeded: bool,
    elapsed: Duration,
    cutoff_scenarios: Option<[AllocationCutoffScenario; 5]>,
}

#[derive(Clone)]
struct AllocationCutoffScenario {
    centi_bps: u32,
    incumbent: BigInt,
    intervals_supported: usize,
    intervals_pruned: usize,
    intervals_avoided: usize,
    envelope_replays_avoided: usize,
    leaf_replays_avoided: usize,
    improvements: usize,
}

fn cutoff_allows_exploration(upper: &BigInt, incumbent: &BigInt, centi_bps: u32) -> bool {
    if centi_bps == 0 || incumbent <= &BigInt::from(0u8) {
        return upper > incumbent;
    }
    let scale = BigInt::from(1_000_000u32);
    upper * &scale > incumbent * (scale + BigInt::from(centi_bps))
}

impl AllocationCutoffScenario {
    fn new(centi_bps: u32, incumbent: &BigInt) -> Self {
        Self {
            centi_bps,
            incumbent: incumbent.clone(),
            intervals_supported: 0,
            intervals_pruned: 0,
            intervals_avoided: 0,
            envelope_replays_avoided: 0,
            leaf_replays_avoided: 0,
            improvements: 0,
        }
    }

    fn should_explore(&mut self, upper: &BigInt) -> bool {
        self.intervals_supported = self
            .intervals_supported
            .saturating_add(1);
        let prune = !cutoff_allows_exploration(upper, &self.incumbent, self.centi_bps);
        if prune {
            self.intervals_pruned = self.intervals_pruned.saturating_add(1);
        }
        !prune
    }
}

impl SacredTimelineAllocationStats {
    fn emit(&self) {
        let pruning = if self.intervals == 0 {
            0.0
        } else {
            self.bound_pruned as f64 / self.intervals as f64 * 100.0
        };
        eprintln!("\n=== SacredTimelineAllocationCensusV1 ===");
        eprintln!("intervals examined:          {}", self.intervals);
        eprintln!("certified-bound pruned:      {}", self.bound_pruned);
        eprintln!("interval pruning fraction:   {pruning:.2}%");
        eprintln!("singleton intervals replayed: {}", self.singleton_intervals);
        eprintln!("incumbent improvements:      {}", self.incumbent_improvements);
        eprintln!("envelope simulator replays:  {}", self.envelope_replays);
        eprintln!("exact-leaf simulator replays: {}", self.exact_leaf_replays);
        eprintln!("unsupported:                 {}", self.unsupported);
        eprintln!("budget exceeded:             {}", self.budget_exceeded);
        eprintln!("branch-and-bound time us:    {}", self.elapsed.as_micros());
        if let Some(scenarios) = &self.cutoff_scenarios {
            let actual = scenarios[0].incumbent.clone();
            for scenario in scenarios {
                let loss = (&actual - &scenario.incumbent).max(BigInt::from(0u8));
                let loss_bps = match (loss.to_f64(), actual.to_f64()) {
                    (Some(loss), Some(actual)) if actual > 0.0 => loss / actual * 10_000.0,
                    _ => 0.0,
                };
                eprintln!(
                    "allocation cutoff result centi_bps={} supported={} pruned={} intervals_avoided={} envelope_replays_avoided={} leaf_replays_avoided={} improvements={} changed={} loss_bps={loss_bps:.9}",
                    scenario.centi_bps,
                    scenario.intervals_supported,
                    scenario.intervals_pruned,
                    scenario.intervals_avoided,
                    scenario.envelope_replays_avoided,
                    scenario.leaf_replays_avoided,
                    scenario.improvements,
                    scenario.incumbent != actual,
                );
            }
        }
        eprintln!("new pruning enabled:         no");
        eprintln!("routing output changed:      no");
        eprintln!("=== end SacredTimelineAllocationCensusV1 ===\n");
    }
}

/// Exact conversion from gas units to output-token base units.
///
/// The coordinate search still uses `f64` as a ranking heuristic. Certificates must instead use
/// this ratio so comparisons at one raw output unit remain meaningful.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExactGasValuation {
    numerator: BigUint,
    denominator: BigUint,
}

impl ExactGasValuation {
    pub(super) fn new(numerator: BigUint, denominator: BigUint) -> Option<Self> {
        (!denominator.is_zero()).then_some(Self { numerator, denominator })
    }

    fn cost(&self, gas: &BigUint) -> BigUint {
        gas * &self.numerator / &self.denominator
    }
}

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
        if denominator.is_zero() {
            None
        } else {
            Some(Self { numerator, denominator })
        }
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

    fn mul(&self, other: &Self) -> Self {
        Self {
            numerator: &self.numerator * &other.numerator,
            denominator: &self.denominator * &other.denominator,
        }
    }

    fn ceil(&self) -> BigUint {
        if self.numerator.is_zero() {
            return BigUint::zero();
        }
        (&self.numerator + &self.denominator - BigUint::from(1u8)) / &self.denominator
    }
}

/// Returns an initial marginal-rate envelope for a standard V3 path.
///
/// An exact-input V3 swap moves its execution price against the trader. The initial marginal
/// price therefore upper-bounds output throughout the swap, including across initialized ticks.
/// The exact fee factor and marginal envelopes compose across a multi-hop path.
fn v3_marginal_envelope<'a>(
    path: &PathAllocation,
    states: impl IntoIterator<
        Item = &'a dyn tycho_simulation::tycho_common::simulation::protocol_sim::ProtocolSim,
    >,
) -> Option<PositiveFraction> {
    const Q96_BITS: usize = 96;
    let q192 = BigUint::from(1u8) << (2 * Q96_BITS);
    let mut envelope = PositiveFraction::new(BigUint::from(1u8), BigUint::from(1u8))?;

    for (hop, state) in path.hops.iter().zip(states) {
        let state = state
            .as_any()
            .downcast_ref::<UniswapV3State>()?;
        // Upstream currently exposes no immutable price accessor. Serialization is used only once
        // per certified solve to recover the exact U256, never in the interval loop.
        let encoded = serde_json::to_value(state).ok()?;
        let sqrt_price =
            serde_json::from_value::<alloy::primitives::U256>(encoded.get("sqrt_price")?.clone())
                .ok()?;
        let fee = serde_json::from_value::<FeeAmount>(encoded.get("fee")?.clone()).ok()? as u32;
        let sqrt_price = BigUint::from_bytes_be(&sqrt_price.to_be_bytes::<32>());
        let squared = &sqrt_price * &sqrt_price;
        let price_envelope = if hop.descriptor.token_in < hop.descriptor.token_out {
            PositiveFraction::new(squared, q192.clone())?
        } else {
            PositiveFraction::new(q192.clone(), squared)?
        };
        let fee_envelope =
            PositiveFraction::new(BigUint::from(1_000_000u32 - fee), BigUint::from(1_000_000u32))?;
        envelope = envelope
            .mul(&price_envelope)
            .mul(&fee_envelope);
    }
    Some(envelope)
}

fn replay_path_with_terminal_envelope(
    path: &PathAllocation,
    amount_in: BigUint,
    total: &BigUint,
    market: &MarketState,
) -> Result<(PathAllocation, Option<PositiveFraction>), AlgorithmError> {
    if amount_in.is_zero() {
        let envelope = path_curve(path, market)
            .and_then(|curve| curve.derivative_fraction(&amount_in))
            .or_else(|| {
                let states = path
                    .hops
                    .iter()
                    .map(|hop| market.get_simulation_state(&hop.descriptor.component_id))
                    .collect::<Option<Vec<_>>>()?;
                v3_marginal_envelope(path, states)
            });
        return Ok((zero_path(path), envelope));
    }
    let descriptors = path
        .hops
        .iter()
        .map(|hop| hop.descriptor.clone())
        .collect::<Vec<_>>();
    let sim = simulate_path(&descriptors, &amount_in, market, &MarketOverrides::empty())?;
    let envelope = path_curve(path, market)
        .and_then(|curve| curve.derivative_fraction(&amount_in))
        .or_else(|| {
            v3_marginal_envelope(
                path,
                sim.post_swap_states
                    .iter()
                    .map(|(_, state)| state.as_ref()),
            )
        });
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
    Ok((replayed, envelope))
}

fn pair_local_marginal_upper(
    current: &[PathAllocation],
    total: &BigUint,
    lo: &BigUint,
    hi: &BigUint,
    market: &MarketState,
    gas_valuation: Option<&ExactGasValuation>,
) -> Result<Option<BigInt>, AlgorithmError> {
    let (left_base, left_rate) =
        replay_path_with_terminal_envelope(&current[0], total - hi, total, market)?;
    let (right_base, right_rate) =
        replay_path_with_terminal_envelope(&current[1], lo.clone(), total, market)?;
    let (Some(left_rate), Some(right_rate)) = (left_rate, right_rate) else {
        if !certified_pair_supported(current, market) {
            return Ok(None);
        }
        // Passive V4 currently exposes no exact immutable price accessor. Monotonicity still gives
        // a sound, though weaker, box bound: across right-flow [lo, hi], the left path receives at
        // most total-lo and the right path at most hi. Ignoring gas can only raise the net bound.
        let left_max = replay_path(&current[0], total - lo, total, market)?;
        let right_max = replay_path(&current[1], hi.clone(), total, market)?;
        return Ok(Some(BigInt::from(left_max.amount_out + right_max.amount_out)));
    };
    let variable_at = |right_flow: &BigUint| {
        left_rate
            .mul_uint(&(hi - right_flow))
            .add(&right_rate.mul_uint(&(right_flow - lo)))
    };
    let at_lo = variable_at(lo);
    let at_hi = variable_at(hi);
    let variable_upper =
        if &at_lo.numerator * &at_hi.denominator >= &at_hi.numerator * &at_lo.denominator {
            at_lo
        } else {
            at_hi
        };
    let gross_upper =
        left_base.amount_out.clone() + right_base.amount_out.clone() + variable_upper.ceil();
    let minimum_gas = gas(&[left_base, right_base]);
    let gas_lower = gas_valuation
        .map(|valuation| valuation.cost(&minimum_gas))
        .unwrap_or_default();
    Ok(Some(BigInt::from(gross_upper) - BigInt::from(gas_lower)))
}

pub(super) fn shadow_compare_structural_pair(
    left: &[HopDescriptor],
    right: &[HopDescriptor],
    total: &BigUint,
    market: &MarketState,
) -> Result<(), AlgorithmError> {
    v2_certified_interval_allocator::shadow_compare_descriptors(left, right, total, market)
}

pub(super) fn refine_disjoint_allocations(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
    gas_cost_per_unit: f64,
    exact_gas_valuation: Option<&ExactGasValuation>,
    cutoff_centi_bps: u32,
) -> Result<Option<Vec<PathAllocation>>, AlgorithmError> {
    if current.len() < 2 || !paths_are_pool_disjoint(current) {
        return Ok(None);
    }

    if std::env::var_os("FYND_V2_CERTIFIED_INTERVAL_CENSUS").is_some() && current.len() == 2 {
        emit_v2_certified_interval_census(current, total, market);
    }

    if std::env::var_os("FYND_V2_CERTIFIED_PRUNE").is_some() && current.len() == 2 {
        if let Some(upper) = certified_v2_pair_upper(current, total, market) {
            let incumbent_index = if current[1].amount_out > current[0].amount_out { 1 } else { 0 };
            let incumbent = &current[incumbent_index].amount_out;
            if upper <= *incumbent {
                if std::env::var_os("FYND_V2_CERTIFIED_PRUNE_TRACE").is_some() {
                    eprintln!(
                        "v2-certified-prune: upper={} incumbent={} left_hops={} right_hops={}",
                        upper,
                        incumbent,
                        current[0].hops.len(),
                        current[1].hops.len()
                    );
                }
                return Ok(Some(vec![current[incumbent_index].clone()]));
            }
        }
    }

    let certified_v2_supported = current.len() == 2 &&
        current.iter().all(|path| {
            !path.hops.is_empty() &&
                path.hops.iter().all(|hop| {
                    market
                        .get_simulation_state(&hop.descriptor.component_id)
                        .is_some_and(|state| {
                            state
                                .as_any()
                                .downcast_ref::<UniswapV2State>()
                                .is_some()
                        })
                })
        });
    if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR").is_some() && certified_v2_supported {
        match v2_certified_interval_allocator::certified_allocate(current, total, market) {
            Ok(Some(refined)) => {
                if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
                    eprintln!(
                        "v2-certified-allocator: used paths={} total={}",
                        current.len(),
                        total
                    );
                }
                return Ok(Some(refined));
            }
            Ok(None) => {
                if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
                    eprintln!(
                        "v2-certified-allocator: fallback unsupported paths={}",
                        current.len()
                    );
                }
            }
            Err(error) => {
                if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
                    eprintln!("v2-certified-allocator: fallback error={error}");
                }
            }
        }
    }

    if let Some(refined) = allocate_uniswap_v2_paths(current, total, market)? {
        if std::env::var_os("FYND_V2_CERTIFIED_INTERVAL_SHADOW").is_some() {
            v2_certified_interval_allocator::shadow_compare(current, total, market, &refined)?;
        }
        return Ok(Some(refined));
    }
    refine_simulated_paths(
        current,
        total,
        market,
        gas_cost_per_unit,
        exact_gas_valuation,
        cutoff_centi_bps,
    )
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
    if amount_in.is_zero() {
        return Ok(zero_path(path));
    }
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

fn zero_path(path: &PathAllocation) -> PathAllocation {
    // Theorem 2, assumption 2 (zero-flow closure): extending a portfolio with an unused path must
    // add neither output nor gas. Keep this representation aligned with `gas`, which excludes
    // zero-input paths, and with final allocation cleanup, which removes them.
    let mut zeroed = path.clone();
    zeroed.flow_fraction = 0.0;
    zeroed.amount_in = BigUint::zero();
    zeroed.amount_out = BigUint::zero();
    zeroed.marginal_price_product = 0.0;
    for hop in &mut zeroed.hops {
        hop.amount_out = BigUint::zero();
        hop.gas = BigUint::zero();
    }
    zeroed
}

fn output(paths: &[PathAllocation]) -> BigUint {
    paths
        .iter()
        .fold(BigUint::zero(), |sum, path| sum + &path.amount_out)
}
fn input(paths: &[PathAllocation]) -> BigUint {
    paths
        .iter()
        .fold(BigUint::zero(), |sum, path| sum + &path.amount_in)
}

fn gas(paths: &[PathAllocation]) -> BigUint {
    // Theorem 2, assumption 2: zero-flow paths have zero activation cost in the proved objective.
    paths
        .iter()
        .filter(|path| !path.amount_in.is_zero())
        .flat_map(|path| &path.hops)
        .fold(BigUint::zero(), |sum, hop| sum + &hop.gas)
}

fn score(paths: &[PathAllocation], gas_cost_per_unit: f64) -> f64 {
    output(paths)
        .to_f64()
        .unwrap_or(f64::INFINITY) -
        gas(paths)
            .to_f64()
            .unwrap_or(f64::INFINITY) *
            gas_cost_per_unit
}

fn exact_score(paths: &[PathAllocation], gas_valuation: Option<&ExactGasValuation>) -> BigInt {
    let output = BigInt::from(output(paths));
    let Some(gas_valuation) = gas_valuation else { return output };
    output - BigInt::from(gas_valuation.cost(&gas(paths)))
}

fn certified_pair_supported(paths: &[PathAllocation], market: &MarketState) -> bool {
    // The passive-V4 monotonicity fallback is sound but presently too weak to be a production
    // certificate: live audits mostly exhaust its interval budget. Keep it analysis-only until a
    // tighter V4 marginal envelope is available.
    let passive_v4_enabled =
        cfg!(test) || std::env::var_os("FYND_PASSIVE_V4_PAIR_CERTIFICATE").is_some();
    paths.len() == 2 &&
        paths.iter().all(|path| {
            !path.hops.is_empty() &&
                (path_curve(path, market).is_some() ||
                    path.hops.iter().all(|hop| {
                        market
                            .get_simulation_state(&hop.descriptor.component_id)
                            .is_some_and(|state| {
                                let state = state.as_any();
                                state
                                    .downcast_ref::<UniswapV3State>()
                                    .is_some() ||
                                    passive_v4_enabled &&
                                        state
                                            .downcast_ref::<UniswapV4State>()
                                            .is_some_and(|state| {
                                                state.hook.as_ref().is_none_or(|hook| {
                                                    hook.address() == Address::ZERO
                                                })
                                            })
                            })
                    }))
        })
}

/// Exactly optimizes a standard two-path V3 portfolio using only monotonicity.
///
/// For right-path flow `x in [lo, hi]`, monotonicity gives the gross upper bound
/// `Q_left(total - lo) + Q_right(hi)`. Gross output also upper-bounds net output because gas cost
/// is non-negative. An interval is discarded only when this bound cannot beat the exact incumbent.
fn certified_pair_branch_and_bound(
    current: &[PathAllocation],
    seed: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
    gas_valuation: Option<&ExactGasValuation>,
    cutoff_centi_bps: u32,
) -> Result<CertifiedPairSolve, AlgorithmError> {
    // Theorem 2, assumption 1 is established only when this function returns `Complete`: every
    // integer allocation point in the admitted two-path family was either replayed or removed by
    // a sound upper bound. `Unsupported` and `BudgetExceeded` deliberately carry no certificate.
    // A positive `cutoff_centi_bps` additionally changes the result from exact to the bounded-
    // quality guarantee of Theorem 4.
    let census_enabled = sacred_timeline_allocation_census_enabled();
    let started = Instant::now();
    let mut census = SacredTimelineAllocationStats::default();
    if !certified_pair_supported(current, market) {
        if census_enabled {
            census.unsupported = true;
            census.elapsed = started.elapsed();
            census.emit();
        }
        return Ok(CertifiedPairSolve::Unsupported);
    }

    let mut best = seed.to_vec();
    let mut best_score = exact_score(&best, gas_valuation);
    if census_enabled {
        census.cutoff_scenarios = Some(
            SACRED_TIMELINE_CUTOFFS_CENTI_BPS
                .map(|centi_bps| AllocationCutoffScenario::new(centi_bps, &best_score)),
        );
    }
    let mut intervals = vec![(BigUint::zero(), total.clone(), true, [true; 5])];
    let mut examined = 0usize;

    while let Some((lo, hi, actual_active, active_scenarios)) = intervals.pop() {
        if actual_active {
            examined += 1;
        }
        if actual_active {
            census.intervals += 1;
        }
        if examined > CERTIFIED_PAIR_INTERVAL_BUDGET {
            if census_enabled {
                census.budget_exceeded = true;
                census.elapsed = started.elapsed();
                census.emit();
            }
            return Ok(CertifiedPairSolve::BudgetExceeded);
        }

        if actual_active {
            census.envelope_replays += 2;
        }
        let net_upper =
            match pair_local_marginal_upper(current, total, &lo, &hi, market, gas_valuation) {
                Ok(Some(net_upper)) => net_upper,
                Ok(None) if actual_active => {
                    if census_enabled {
                        census.unsupported = true;
                        census.elapsed = started.elapsed();
                        census.emit();
                    }
                    return Ok(CertifiedPairSolve::Unsupported);
                }
                Err(error) if actual_active => return Err(error),
                Ok(None) | Err(_) => continue,
            };
        let mut child_scenarios = [false; 5];
        if let Some(scenarios) = &mut census.cutoff_scenarios {
            for (index, (scenario, active)) in scenarios
                .iter_mut()
                .zip(active_scenarios)
                .enumerate()
            {
                if active {
                    child_scenarios[index] = scenario.should_explore(&net_upper);
                } else {
                    scenario.intervals_avoided = scenario
                        .intervals_avoided
                        .saturating_add(1);
                    scenario.envelope_replays_avoided = scenario
                        .envelope_replays_avoided
                        .saturating_add(2);
                }
            }
        }
        let actual_explores =
            actual_active && cutoff_allows_exploration(&net_upper, &best_score, cutoff_centi_bps);
        if actual_active && !actual_explores {
            census.bound_pruned = census.bound_pruned.saturating_add(1);
        }
        if !actual_explores &&
            !child_scenarios
                .iter()
                .any(|active| *active)
        {
            continue;
        }

        if lo == hi {
            if actual_active {
                census.singleton_intervals += 1;
                census.exact_leaf_replays += 2;
            }
            let candidate = match (
                replay_path(&current[0], total - &lo, total, market),
                replay_path(&current[1], lo, total, market),
            ) {
                (Ok(left), Ok(right)) => vec![left, right],
                (Err(error), _) | (_, Err(error)) if actual_active => return Err(error),
                _ => continue,
            };
            let candidate_score = exact_score(&candidate, gas_valuation);
            if let Some(scenarios) = &mut census.cutoff_scenarios {
                for (scenario, active) in scenarios
                    .iter_mut()
                    .zip(child_scenarios)
                {
                    if active {
                        if candidate_score > scenario.incumbent {
                            scenario.incumbent = candidate_score.clone();
                            scenario.improvements = scenario.improvements.saturating_add(1);
                        }
                    } else {
                        scenario.leaf_replays_avoided = scenario
                            .leaf_replays_avoided
                            .saturating_add(2);
                    }
                }
            }
            if actual_explores && candidate_score > best_score {
                census.incumbent_improvements += 1;
                best = candidate;
                best_score = candidate_score;
            }
            continue;
        }

        let mid = (&lo + &hi) / BigUint::from(2u8);
        intervals.push((lo, mid.clone(), actual_explores, child_scenarios));
        intervals.push((mid + BigUint::from(1u8), hi, actual_explores, child_scenarios));
    }

    best.retain(|path| !path.amount_in.is_zero());
    if census_enabled {
        census.elapsed = started.elapsed();
        census.emit();
    }
    Ok(CertifiedPairSolve::Complete(best))
}

fn feasible_seed(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
) -> Result<Vec<PathAllocation>, AlgorithmError> {
    if input(current) == *total {
        return Ok(current.to_vec());
    }
    let count = BigUint::from(current.len() as u64);
    let base = total / &count;
    let remainder = (total % &count).to_usize().unwrap_or(0);
    current
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let mut amount = base.clone();
            if index < remainder {
                amount += BigUint::from(1u8);
            }
            replay_path(path, amount, total, market)
        })
        .collect()
}

fn refine_simulated_paths(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
    gas_cost_per_unit: f64,
    exact_gas_valuation: Option<&ExactGasValuation>,
    cutoff_centi_bps: u32,
) -> Result<Option<Vec<PathAllocation>>, AlgorithmError> {
    // The coordinate-ascent phase below is candidate generation, not a global certificate. If the
    // certified pair solver cannot return `Complete`, this function returns the heuristic
    // candidate. Therefore this fallback does not establish Theorem 2, assumption 1 and must
    // never inherit the maximal-frontier optimality claim.
    let mut best = feasible_seed(current, total, market)?;
    if std::env::var_os("FYND_REFINER_GRID").is_some() && current.len() == 2 {
        let routes = current
            .iter()
            .map(|path| {
                path.hops
                    .iter()
                    .map(|hop| hop.descriptor.component_id.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect::<Vec<_>>();
        eprintln!("refiner-grid: left={} right={}", routes[0], routes[1]);
        for right_percent in (0u32..=100).step_by(5) {
            let right_amount = total * BigUint::from(right_percent) / BigUint::from(100u32);
            let left_amount = total - &right_amount;
            let left = replay_path(&current[0], left_amount.clone(), total, market)?;
            let right = replay_path(&current[1], right_amount.clone(), total, market)?;
            eprintln!(
                "  right_percent={} left_in={} right_in={} left_out={} right_out={} total_out={}",
                right_percent,
                left_amount,
                right_amount,
                left.amount_out,
                right.amount_out,
                &left.amount_out + &right.amount_out
            );
        }
    }
    for _ in 0..4 {
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
                        let left = replay_path(&best[left], left_amount, total, market);
                        let right = replay_path(&best[right], right_amount, total, market);
                        match (left, right) {
                            (Ok(left), Ok(right)) => score(&[left, right], gas_cost_per_unit),
                            _ => f64::NEG_INFINITY,
                        }
                    },
                    0.0,
                    1.0,
                    16,
                );
                let (interior_left_amount, interior_right_amount) =
                    split_amount(&pair_total, split);
                let interior_left = replay_path(&best[left], interior_left_amount, total, market)?;
                let interior_right =
                    replay_path(&best[right], interior_right_amount, total, market)?;
                let interior_score =
                    score(&[interior_left.clone(), interior_right.clone()], gas_cost_per_unit);
                let left_boundary_left =
                    replay_path(&best[left], pair_total.clone(), total, market)?;
                let left_boundary_right = zero_path(&best[right]);
                let left_boundary_score = score(
                    &[left_boundary_left.clone(), left_boundary_right.clone()],
                    gas_cost_per_unit,
                );
                let right_boundary_left = zero_path(&best[left]);
                let right_boundary_right =
                    replay_path(&best[right], pair_total.clone(), total, market)?;
                let right_boundary_score = score(
                    &[right_boundary_left.clone(), right_boundary_right.clone()],
                    gas_cost_per_unit,
                );
                let (left_path, right_path, new_pair_score) = if left_boundary_score >
                    interior_score &&
                    left_boundary_score >= right_boundary_score
                {
                    (left_boundary_left, left_boundary_right, left_boundary_score)
                } else if right_boundary_score > interior_score {
                    (right_boundary_left, right_boundary_right, right_boundary_score)
                } else {
                    (interior_left, interior_right, interior_score)
                };
                let old_pair_score =
                    score(&[best[left].clone(), best[right].clone()], gas_cost_per_unit);
                if new_pair_score > old_pair_score {
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
    match certified_pair_branch_and_bound(
        current,
        &best,
        total,
        market,
        exact_gas_valuation,
        cutoff_centi_bps,
    )? {
        CertifiedPairSolve::Complete(certified) => {
            if std::env::var_os("FYND_REFINER_DETAIL").is_some() {
                eprintln!("certified-pair: complete");
            }
            return Ok(Some(certified));
        }
        CertifiedPairSolve::Unsupported => {
            if std::env::var_os("FYND_REFINER_DETAIL").is_some() {
                eprintln!("certified-pair: unsupported");
            }
        }
        CertifiedPairSolve::BudgetExceeded => {
            if std::env::var_os("FYND_REFINER_DETAIL").is_some() {
                eprintln!("certified-pair: budget-exceeded");
            }
        }
    }
    best.retain(|path| !path.amount_in.is_zero());
    if std::env::var_os("FYND_REFINER_DETAIL").is_some() {
        let source = current
            .iter()
            .map(|path| {
                path.hops
                    .iter()
                    .map(|hop| hop.descriptor.component_id.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect::<Vec<_>>()
            .join(" + ");
        let allocation = best
            .iter()
            .map(|path| {
                format!(
                    "{}:{}",
                    path.amount_in,
                    path.hops
                        .iter()
                        .map(|hop| hop.descriptor.component_id.as_str())
                        .collect::<Vec<_>>()
                        .join("|")
                )
            })
            .collect::<Vec<_>>()
            .join(" + ");
        eprintln!(
            "refiner-detail: source=[{source}] allocation=[{allocation}] output={}",
            output(&best)
        );
        eprintln!("refiner-detail: exact-net={}", exact_score(&best, exact_gas_valuation));
    }
    Ok(Some(best))
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
    fn value(&self, amount: &BigUint) -> Option<PositiveFraction> {
        PositiveFraction::new(&self.a * amount, &self.b + &self.c * amount)
    }
    fn derivative_fraction(&self, amount: &BigUint) -> Option<PositiveFraction> {
        let base = &self.b + &self.c * amount;
        PositiveFraction::new(&self.a * &self.b, &base * &base)
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

fn descriptor_path_curve(path: &[HopDescriptor], market: &MarketState) -> Option<ContinuousCpmm> {
    let mut curve = None;
    for descriptor in path {
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

fn path_curve(path: &PathAllocation, market: &MarketState) -> Option<ContinuousCpmm> {
    let descriptors = path
        .hops
        .iter()
        .map(|hop| hop.descriptor.clone())
        .collect::<Vec<_>>();
    descriptor_path_curve(&descriptors, market)
}

fn certified_v2_interval_upper_from_curves(
    left: &ContinuousCpmm,
    right: &ContinuousCpmm,
    total: &BigUint,
    lo: &BigUint,
    hi: &BigUint,
) -> Option<BigUint> {
    if lo > hi || hi > total {
        return None;
    }
    let midpoint = (lo + hi) / BigUint::from(2u8);
    let left_amount = total - &midpoint;
    let value_at_mid = left
        .value(&left_amount)?
        .add(&right.value(&midpoint)?);
    let left_derivative = left.derivative_fraction(&left_amount)?;
    let right_derivative = right.derivative_fraction(&midpoint)?;
    let right_ge_left = &right_derivative.numerator * &left_derivative.denominator >=
        &left_derivative.numerator * &right_derivative.denominator;
    let slope_magnitude = if right_ge_left {
        PositiveFraction::new(
            &right_derivative.numerator * &left_derivative.denominator -
                &left_derivative.numerator * &right_derivative.denominator,
            &right_derivative.denominator * &left_derivative.denominator,
        )?
    } else {
        PositiveFraction::new(
            &left_derivative.numerator * &right_derivative.denominator -
                &right_derivative.numerator * &left_derivative.denominator,
            &right_derivative.denominator * &left_derivative.denominator,
        )?
    };
    let endpoint_distance = if right_ge_left { hi - &midpoint } else { &midpoint - lo };
    Some(
        value_at_mid
            .add(&slope_magnitude.mul_uint(&endpoint_distance))
            .ceil(),
    )
}

/// Sound upper bound on the exact output of a two-path V2 split over x in [0,T].
pub(super) fn certified_v2_interval_upper_for_paths(
    left: &[HopDescriptor],
    right: &[HopDescriptor],
    total: &BigUint,
    lo: &BigUint,
    hi: &BigUint,
    market: &MarketState,
) -> Option<BigUint> {
    let left_curve = descriptor_path_curve(left, market)?;
    let right_curve = descriptor_path_curve(right, market)?;
    certified_v2_interval_upper_from_curves(&left_curve, &right_curve, total, lo, hi)
}

fn certified_v2_pair_upper(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
) -> Option<BigUint> {
    if current.len() != 2 || total.is_zero() {
        return None;
    }
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

fn exact_pair_output(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
    x: &BigUint,
) -> Option<BigUint> {
    if current.len() != 2 || x > total {
        return None;
    }
    let left = replay_path(&current[0], total - x, total, market).ok()?;
    let right = replay_path(&current[1], x.clone(), total, market).ok()?;
    Some(left.amount_out + right.amount_out)
}

fn analyze_v2_certified_interval(
    current: &[PathAllocation],
    left: &ContinuousCpmm,
    right: &ContinuousCpmm,
    total: &BigUint,
    market: &MarketState,
    incumbent: &BigUint,
    lo: BigUint,
    hi: BigUint,
    depth: usize,
    stats: &mut V2CertifiedIntervalStats,
) {
    stats.intervals += 1;
    let Some(upper) = certified_v2_interval_upper_from_curves(left, right, total, &lo, &hi) else {
        stats.unresolved_leaves += 1;
        return;
    };
    let midpoint = (&lo + &hi) / BigUint::from(2u8);
    if upper <= *incumbent {
        stats.certified_dead += 1;
        stats.dead_by_depth[depth] += 1;
        stats.dead_width += &hi - &lo;
        for x in [&lo, &midpoint, &hi] {
            match exact_pair_output(current, total, market, x) {
                Some(exact) => {
                    stats.falsifier_probes += 1;
                    if exact > *incumbent {
                        stats.falsifier_violations += 1;
                    }
                }
                None => stats.falsifier_unknown += 1,
            }
        }
        return;
    }
    if depth == CERTIFIED_INTERVAL_DEPTH || lo == hi {
        stats.unresolved_leaves += 1;
        return;
    }
    analyze_v2_certified_interval(
        current,
        left,
        right,
        total,
        market,
        incumbent,
        lo.clone(),
        midpoint.clone(),
        depth + 1,
        stats,
    );
    analyze_v2_certified_interval(
        current,
        left,
        right,
        total,
        market,
        incumbent,
        midpoint,
        hi,
        depth + 1,
        stats,
    );
}

fn emit_v2_certified_interval_census(
    current: &[PathAllocation],
    total: &BigUint,
    market: &MarketState,
) {
    if current.len() != 2 || total.is_zero() {
        return;
    }
    let (Some(left), Some(right)) =
        (path_curve(&current[0], market), path_curve(&current[1], market))
    else {
        return;
    };
    let incumbent = current[0]
        .amount_out
        .clone()
        .max(current[1].amount_out.clone());
    let mut stats = V2CertifiedIntervalStats::default();
    analyze_v2_certified_interval(
        current,
        &left,
        &right,
        total,
        market,
        &incumbent,
        BigUint::zero(),
        total.clone(),
        0,
        &mut stats,
    );
    let dead_bps = ((&stats.dead_width * BigUint::from(10_000u64)) / total)
        .to_u64()
        .unwrap_or(10_000)
        .min(10_000);
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
    for (depth, count) in stats.dead_by_depth.iter().enumerate() {
        eprintln!("  depth {}: {}", depth, count);
    }
    eprintln!("=== end V2CertifiedIntervalCensusV1 ===\n");
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
    let Some(amounts) = allocations(&curves, total) else {
        return Ok(None);
    };
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
    use alloy::primitives::U256;
    use tycho_simulation::{
        evm::protocol::{uniswap_v4::state::UniswapV4Fees, utils::uniswap::tick_list::TickInfo},
        tycho_common::{
            models::{token::Token, Chain},
            simulation::protocol_sim::ProtocolSim,
            Bytes,
        },
    };

    use super::*;
    use crate::algorithm::split_primitives::SimulatedHop;

    fn v3_test_token(address: u8) -> Token {
        Token::new(
            &Bytes::from(vec![address; 20]),
            &format!("T{address}"),
            18,
            0,
            &[Some(10_000)],
            Chain::Ethereum,
            100,
        )
    }

    fn v3_test_pool(liquidity: u128, fee: FeeAmount) -> UniswapV3State {
        let spacing = match fee {
            FeeAmount::Lowest => 1,
            FeeAmount::Lowest2 => 2,
            FeeAmount::Lowest3 => 3,
            FeeAmount::Lowest4 => 4,
            FeeAmount::Low => 10,
            FeeAmount::MediumLow => 50,
            FeeAmount::Medium => 60,
            FeeAmount::MediumHigh => 100,
            FeeAmount::High => 200,
        };
        UniswapV3State::new(
            liquidity,
            U256::from(1u8) << 96,
            fee,
            0,
            (-200..=200)
                .filter(|tick| *tick != 0)
                .map(|tick| TickInfo::new(tick * spacing, 0).unwrap())
                .collect(),
        )
        .unwrap()
    }

    fn v4_test_pool(liquidity: u128, lp_fee: u32) -> UniswapV4State {
        UniswapV4State::new(
            liquidity,
            U256::from(1u8) << 96,
            UniswapV4Fees::new(0, 0, lp_fee),
            0,
            10,
            (-200..=200)
                .filter(|tick| *tick != 0)
                .map(|tick| TickInfo::new(tick * 10, 0).unwrap())
                .collect(),
        )
        .unwrap()
    }

    fn v3_test_path(component_id: &str, token_in: &Token, token_out: &Token) -> PathAllocation {
        v3_test_path_from_hops(&[(component_id, token_in, token_out)])
    }

    fn v3_test_path_from_hops(hops: &[(&str, &Token, &Token)]) -> PathAllocation {
        PathAllocation {
            hops: hops
                .iter()
                .map(|(component_id, token_in, token_out)| SimulatedHop {
                    descriptor: HopDescriptor::new(
                        (*component_id).to_string(),
                        (*token_in).clone(),
                        (*token_out).clone(),
                    ),
                    amount_out: BigUint::zero(),
                    gas: BigUint::zero(),
                })
                .collect(),
            flow_fraction: 0.0,
            amount_in: BigUint::zero(),
            amount_out: BigUint::zero(),
            marginal_price_product: 0.0,
        }
    }

    #[test]
    fn exact_gas_valuation_preserves_integer_units() {
        let valuation = ExactGasValuation::new(BigUint::from(3u8), BigUint::from(2u8)).unwrap();
        assert_eq!(valuation.cost(&BigUint::from(5u8)), BigUint::from(7u8));
    }

    #[test]
    fn passive_v4_monotone_certificate_matches_exhaustive_small_domain() {
        let token_a = v3_test_token(1);
        let token_b = v3_test_token(2);
        let total = BigUint::from(32u8);
        let mut market = MarketState::new();
        market.update_states([
            ("left".to_string(), Box::new(v4_test_pool(2_000, 500)) as Box<dyn ProtocolSim>),
            ("right".to_string(), Box::new(v4_test_pool(4_000, 3_000)) as Box<dyn ProtocolSim>),
        ]);
        let paths = vec![
            v3_test_path("left", &token_a, &token_b),
            v3_test_path("right", &token_a, &token_b),
        ];
        let exact = (0u8..=32)
            .map(|x| {
                let x = BigUint::from(x);
                exact_score(
                    &[
                        replay_path(&paths[0], &total - &x, &total, &market).unwrap(),
                        replay_path(&paths[1], x, &total, &market).unwrap(),
                    ],
                    None,
                )
            })
            .collect::<Vec<_>>();

        for lo in 0usize..=32 {
            for hi in lo..=32 {
                let upper = pair_local_marginal_upper(
                    &paths,
                    &total,
                    &BigUint::from(lo),
                    &BigUint::from(hi),
                    &market,
                    None,
                )
                .unwrap()
                .unwrap();
                assert!(upper >= *exact[lo..=hi].iter().max().unwrap());
            }
        }
        assert!(matches!(
            certified_pair_allocation(&paths, &total, &market, None, 0).unwrap(),
            CertifiedPairSolve::Complete(_)
        ));
    }

    #[test]
    fn v3_tangent_certificate_matches_exhaustive_small_domains() {
        let token_a = v3_test_token(1);
        let token_b = v3_test_token(2);
        let total = BigUint::from(32u8);

        for (case, (left_liquidity, right_liquidity, left_fee, right_fee, reverse)) in [
            (1_000, 1_500, FeeAmount::Low, FeeAmount::Low, false),
            (1_000, 10_000, FeeAmount::Low, FeeAmount::Medium, false),
            (10_000, 1_000, FeeAmount::High, FeeAmount::Lowest, false),
            (10_000, 10_000, FeeAmount::Medium, FeeAmount::Medium, false),
            (1_000, 1_500, FeeAmount::Low, FeeAmount::Low, true),
            (1_000, 10_000, FeeAmount::Low, FeeAmount::Medium, true),
            (10_000, 1_000, FeeAmount::High, FeeAmount::Lowest, true),
            (10_000, 10_000, FeeAmount::Medium, FeeAmount::Medium, true),
        ]
        .into_iter()
        .enumerate()
        {
            let (token_in, token_out) =
                if reverse { (&token_b, &token_a) } else { (&token_a, &token_b) };
            let mut market = MarketState::new();
            market.update_states([
                (
                    "left".to_string(),
                    Box::new(v3_test_pool(left_liquidity, left_fee)) as Box<dyn ProtocolSim>,
                ),
                (
                    "right".to_string(),
                    Box::new(v3_test_pool(right_liquidity, right_fee)) as Box<dyn ProtocolSim>,
                ),
            ]);
            let paths = vec![
                v3_test_path("left", &token_in, &token_out),
                v3_test_path("right", &token_in, &token_out),
            ];
            let valuation =
                ExactGasValuation::new(BigUint::from(1u8), BigUint::from(10_000u32)).unwrap();
            for gas_valuation in [None, Some(&valuation)] {
                let exact = (0u8..=32)
                    .map(|x| {
                        let x = BigUint::from(x);
                        let allocation = vec![
                            replay_path(&paths[0], &total - &x, &total, &market).unwrap(),
                            replay_path(&paths[1], x, &total, &market).unwrap(),
                        ];
                        exact_score(&allocation, gas_valuation)
                    })
                    .collect::<Vec<_>>();

                for lo in 0usize..=32 {
                    for hi in lo..=32 {
                        let upper = pair_local_marginal_upper(
                            &paths,
                            &total,
                            &BigUint::from(lo),
                            &BigUint::from(hi),
                            &market,
                            gas_valuation,
                        )
                        .unwrap()
                        .unwrap();
                        let actual = exact[lo..=hi].iter().max().unwrap();
                        assert!(
                            upper >= *actual,
                            "case {case}, interval [{lo}, {hi}]: upper={upper}, actual={actual}",
                        );
                    }
                }

                let seed = feasible_seed(&paths, &total, &market).unwrap();
                let CertifiedPairSolve::Complete(solution) = certified_pair_branch_and_bound(
                    &paths,
                    &seed,
                    &total,
                    &market,
                    gas_valuation,
                    0,
                )
                .unwrap() else {
                    panic!("case {case} did not complete");
                };
                assert_eq!(exact_score(&solution, gas_valuation), *exact.iter().max().unwrap(),);
            }
        }
    }

    #[test]
    fn v3_tangent_certificate_composes_over_multihop_paths() {
        let token_a = v3_test_token(1);
        let token_b = v3_test_token(2);
        let token_c = v3_test_token(3);
        let token_d = v3_test_token(4);
        let total = BigUint::from(16u8);
        let mut market = MarketState::new();
        market.update_states([
            (
                "ac".to_string(),
                Box::new(v3_test_pool(2_000, FeeAmount::Low)) as Box<dyn ProtocolSim>,
            ),
            (
                "cb".to_string(),
                Box::new(v3_test_pool(3_000, FeeAmount::Medium)) as Box<dyn ProtocolSim>,
            ),
            (
                "ad".to_string(),
                Box::new(v3_test_pool(4_000, FeeAmount::High)) as Box<dyn ProtocolSim>,
            ),
            (
                "db".to_string(),
                Box::new(v3_test_pool(2_500, FeeAmount::Lowest)) as Box<dyn ProtocolSim>,
            ),
        ]);
        let paths = vec![
            v3_test_path_from_hops(&[("ac", &token_a, &token_c), ("cb", &token_c, &token_b)]),
            v3_test_path_from_hops(&[("ad", &token_a, &token_d), ("db", &token_d, &token_b)]),
        ];
        let valuation =
            ExactGasValuation::new(BigUint::from(1u8), BigUint::from(20_000u32)).unwrap();

        for gas_valuation in [None, Some(&valuation)] {
            let exact = (0u8..=16)
                .map(|x| {
                    let x = BigUint::from(x);
                    exact_score(
                        &[
                            replay_path(&paths[0], &total - &x, &total, &market).unwrap(),
                            replay_path(&paths[1], x, &total, &market).unwrap(),
                        ],
                        gas_valuation,
                    )
                })
                .collect::<Vec<_>>();
            for lo in 0usize..=16 {
                for hi in lo..=16 {
                    let upper = pair_local_marginal_upper(
                        &paths,
                        &total,
                        &BigUint::from(lo),
                        &BigUint::from(hi),
                        &market,
                        gas_valuation,
                    )
                    .unwrap()
                    .unwrap();
                    assert!(
                        upper >= *exact[lo..=hi].iter().max().unwrap(),
                        "multihop interval [{lo}, {hi}] violated its certificate",
                    );
                }
            }
            let seed = feasible_seed(&paths, &total, &market).unwrap();
            let CertifiedPairSolve::Complete(solution) =
                certified_pair_branch_and_bound(&paths, &seed, &total, &market, gas_valuation, 0)
                    .unwrap()
            else {
                panic!("multihop certificate did not complete");
            };
            assert_eq!(exact_score(&solution, gas_valuation), *exact.iter().max().unwrap());
        }
    }

    #[test]
    fn v3_tangent_certificate_deterministic_falsifier_matrix() {
        let token_a = v3_test_token(1);
        let token_b = v3_test_token(2);
        let total = BigUint::from(12u8);
        let fees = [
            FeeAmount::Lowest,
            FeeAmount::Low,
            FeeAmount::MediumLow,
            FeeAmount::Medium,
            FeeAmount::High,
        ];
        let mut random = 0x9e37_79b9_7f4a_7c15u64;

        for case in 0..24 {
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let left_liquidity = 800 + random as u128 % 20_000;
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let right_liquidity = 800 + random as u128 % 20_000;
            let left_fee = fees[(random as usize >> 8) % fees.len()];
            let right_fee = fees[(random as usize >> 16) % fees.len()];
            let (token_in, token_out) =
                if random & 1 == 0 { (&token_a, &token_b) } else { (&token_b, &token_a) };
            let mut market = MarketState::new();
            market.update_states([
                (
                    "left".to_string(),
                    Box::new(v3_test_pool(left_liquidity, left_fee)) as Box<dyn ProtocolSim>,
                ),
                (
                    "right".to_string(),
                    Box::new(v3_test_pool(right_liquidity, right_fee)) as Box<dyn ProtocolSim>,
                ),
            ]);
            let paths = vec![
                v3_test_path("left", token_in, token_out),
                v3_test_path("right", token_in, token_out),
            ];
            let valuation = ExactGasValuation::new(
                BigUint::from(1 + (random >> 24) % 7),
                BigUint::from(50_000u32),
            )
            .unwrap();
            let exact = (0u8..=12)
                .map(|x| {
                    let x = BigUint::from(x);
                    exact_score(
                        &[
                            replay_path(&paths[0], &total - &x, &total, &market).unwrap(),
                            replay_path(&paths[1], x, &total, &market).unwrap(),
                        ],
                        Some(&valuation),
                    )
                })
                .collect::<Vec<_>>();

            for lo in 0usize..=12 {
                for hi in lo..=12 {
                    let upper = pair_local_marginal_upper(
                        &paths,
                        &total,
                        &BigUint::from(lo),
                        &BigUint::from(hi),
                        &market,
                        Some(&valuation),
                    )
                    .unwrap()
                    .unwrap();
                    assert!(
                        upper >= *exact[lo..=hi].iter().max().unwrap(),
                        "random case {case}, interval [{lo}, {hi}] violated its certificate",
                    );
                }
            }
        }
    }

    #[test]
    fn mixed_v2_v3_certificate_matches_exhaustive_small_domains() {
        let token_a = v3_test_token(1);
        let token_b = v3_test_token(2);
        let total = BigUint::from(32u8);
        let valuation =
            ExactGasValuation::new(BigUint::from(1u8), BigUint::from(20_000u32)).unwrap();

        for (case, reverse, v2_on_left) in
            [(0, false, true), (1, false, false), (2, true, true), (3, true, false)]
        {
            let (token_in, token_out) =
                if reverse { (&token_b, &token_a) } else { (&token_a, &token_b) };
            let mut market = MarketState::new();
            let v2 = Box::new(UniswapV2State::new(U256::from(20_000u32), U256::from(24_000u32)))
                as Box<dyn ProtocolSim>;
            let v3 = Box::new(v3_test_pool(12_000, FeeAmount::Medium)) as Box<dyn ProtocolSim>;
            if v2_on_left {
                market.update_states([("left".to_string(), v2), ("right".to_string(), v3)]);
            } else {
                market.update_states([("left".to_string(), v3), ("right".to_string(), v2)]);
            }
            let paths = vec![
                v3_test_path("left", token_in, token_out),
                v3_test_path("right", token_in, token_out),
            ];

            for gas_valuation in [None, Some(&valuation)] {
                let exact = (0u8..=32)
                    .map(|x| {
                        let x = BigUint::from(x);
                        exact_score(
                            &[
                                replay_path(&paths[0], &total - &x, &total, &market).unwrap(),
                                replay_path(&paths[1], x, &total, &market).unwrap(),
                            ],
                            gas_valuation,
                        )
                    })
                    .collect::<Vec<_>>();
                for lo in 0usize..=32 {
                    for hi in lo..=32 {
                        let upper = pair_local_marginal_upper(
                            &paths,
                            &total,
                            &BigUint::from(lo),
                            &BigUint::from(hi),
                            &market,
                            gas_valuation,
                        )
                        .unwrap()
                        .unwrap();
                        assert!(
                            upper >= *exact[lo..=hi].iter().max().unwrap(),
                            "mixed case {case}, interval [{lo}, {hi}] violated its certificate",
                        );
                    }
                }
                let seed = feasible_seed(&paths, &total, &market).unwrap();
                let CertifiedPairSolve::Complete(solution) = certified_pair_branch_and_bound(
                    &paths,
                    &seed,
                    &total,
                    &market,
                    gas_valuation,
                    0,
                )
                .unwrap() else {
                    panic!("mixed case {case} did not complete");
                };
                assert_eq!(exact_score(&solution, gas_valuation), *exact.iter().max().unwrap(),);
            }
        }
    }

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

    #[test]
    fn tangent_upper_bounds_equal_curves_at_endpoints() {
        let curve = ContinuousCpmm {
            a: BigUint::from(997_000u32),
            b: BigUint::from(1_000_000u32),
            c: BigUint::from(997u32),
        };
        let total = BigUint::from(50u32);
        let midpoint = &total / BigUint::from(2u8);
        let left_amount = &total - &midpoint;
        let value = curve
            .value(&left_amount)
            .unwrap()
            .add(&curve.value(&midpoint).unwrap());
        let upper = value.ceil();
        let endpoint = curve.value(&total).unwrap().ceil();
        assert!(upper >= endpoint);
    }

    #[test]
    fn child_interval_tangent_need_not_be_monotone() {
        let curve = ContinuousCpmm {
            a: BigUint::from(997_000u32),
            b: BigUint::from(1_000_000u32),
            c: BigUint::from(997u32),
        };
        let total = BigUint::from(100u32);
        let root = certified_v2_interval_upper_from_curves(
            &curve,
            &curve,
            &total,
            &BigUint::zero(),
            &total,
        )
        .unwrap();
        let half = certified_v2_interval_upper_from_curves(
            &curve,
            &curve,
            &total,
            &BigUint::zero(),
            &BigUint::from(50u32),
        )
        .unwrap();
        assert!(half >= root);

        let x0 = BigUint::zero();
        let x1 = BigUint::from(25u32);
        let x2 = BigUint::from(50u32);
        for x in [&x0, &x1, &x2] {
            let exact_continuous = curve
                .value(&(&total - x))
                .unwrap()
                .add(&curve.value(x).unwrap())
                .ceil();
            assert!(half >= exact_continuous);
        }
    }
}
