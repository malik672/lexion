//! Independent pool-disjoint path discovery for the exact replay refinement layer.

use std::time::Instant;

use alloy::primitives::Address;
use num_bigint::{BigInt, BigUint};
use num_traits::{ToPrimitive, Zero};
use petgraph::graph::NodeIndex;
use rustc_hash::{FxHashMap, FxHashSet};
use tycho_simulation::evm::protocol::{
    uniswap_v2::state::UniswapV2State, uniswap_v3::state::UniswapV3State,
    uniswap_v4::state::UniswapV4State,
};

use super::{
    bellman_ford::BellmanFordContext,
    exact_v2_refiner::{
        certified_pair_allocation, certified_v2_interval_upper_for_paths,
        refine_disjoint_allocations, CertifiedPairSolve,
    },
    split_primitives::{
        build_split_route, simulate_path, HopDescriptor, MarketOverrides, PathAllocation,
        SimulatedHop,
    },
    AlgorithmError, PathFrankWolfeAlgorithm,
};
use crate::types::Order;

/// A bounded V2/V3/V4 frontier. The exact replay allocator evaluates path
/// portfolios, so raising this has combinatorial cost; six candidates retain
/// the strongest alternatives while keeping user-order latency bounded.
const MAX_CANDIDATE_PATHS: usize = 6;

/// V4 extends rather than replaces the established V2/V3 frontier. Keeping a
/// small separate frontier preserves the old candidate set and bounds the
/// additional portfolio combinations.
const MAX_V4_CANDIDATE_PATHS: usize = 2;

/// Experimental cap for the union of the ordinary full-order frontier and
/// paths that are competitive only at smaller flow amounts.
const MAX_MULTI_SCALE_CANDIDATE_PATHS: usize = 10;

/// Smaller-flow probes used by the experimental production frontier. The
/// full-order frontier is retained separately.
const MULTI_SCALE_FRONTIER_SCALES: &[(u64, u64)] = &[(1, 2), (1, 4), (1, 10), (1, 20)];

/// Only the strongest two paths at each smaller-flow probe are admitted. This
/// is sufficient to expose complementary marginal paths without turning the
/// downstream portfolio enumeration into an unbounded search.
const MULTI_SCALE_PATHS_PER_SCALE: usize = 2;

/// Resolution of the reusable monotone output envelope for certified V4 subset fallback.
const CERTIFIED_V4_BOUND_GRID: usize = 16;

/// Analysis-only probe scales for the routing-language census. These deliberately
/// include the 25% and 10% regimes seen in live Uniswap counterexamples.
const CENSUS_SCALES: &[(u64, u64)] = &[(1, 1), (1, 2), (1, 4), (1, 10), (1, 20)];

/// Maximum dyadic refinement depth for `SymbolicAllocationCutCensusV1`.
/// Depth six gives 64 allocation cells. It is intentionally an analysis budget,
/// not a production routing parameter.
const SYMBOLIC_CUT_DEPTH: usize = 6;

fn census_enabled() -> bool {
    std::env::var_os("FYND_ROUTING_LANGUAGE_CENSUS").is_some()
}

fn symbolic_cut_census_enabled() -> bool {
    std::env::var_os("FYND_SYMBOLIC_ALLOCATION_CENSUS").is_some()
}

fn concavity_audit_enabled() -> bool {
    std::env::var_os("FYND_CONCAVITY_AUDIT").is_some()
}

fn certified_interval_census_enabled() -> bool {
    std::env::var_os("FYND_V2_CERTIFIED_INTERVAL_CENSUS").is_some()
}

fn canonical_portfolio_census_enabled() -> bool {
    std::env::var_os("FYND_CANONICAL_PORTFOLIO_CENSUS").is_some()
}

fn sacred_timeline_census_enabled() -> bool {
    std::env::var_os("FYND_SACRED_TIMELINE_CENSUS").is_some()
}

fn allocation_support_audit_enabled() -> bool {
    std::env::var_os("FYND_ALLOCATION_SUPPORT_AUDIT").is_some()
}

fn emit_allocation_support_audit(
    frontier: &[PathAllocation],
    allocation: &[PathAllocation],
    total: &BigUint,
    order: &Order,
    ctx: &BellmanFordContext,
    gas_valuation: Option<&super::exact_v2_refiner::ExactGasValuation>,
) -> Result<(), AlgorithmError> {
    if !allocation_support_audit_enabled() || !(3..=4).contains(&frontier.len()) {
        return Ok(());
    }

    let heuristic_net = allocation_net_output(allocation, order, ctx);
    let upper = monotone_full_input_upper_bound(frontier, ctx);
    let gap = match (&upper, &heuristic_net) {
        (Some(upper), Some(net)) => Some(upper - net),
        _ => None,
    };
    let mut pair_complete = 0usize;
    let mut pair_unsupported = 0usize;
    let mut pair_budget = 0usize;
    let mut best_pair_net = None::<BigInt>;
    let started = Instant::now();
    for left in 0..frontier.len() {
        for right in left + 1..frontier.len() {
            let pair = [frontier[left].clone(), frontier[right].clone()];
            match certified_pair_allocation(&pair, total, &ctx.market_data, gas_valuation, 0)? {
                CertifiedPairSolve::Complete(allocation) => {
                    pair_complete += 1;
                    if let Some(net) = allocation_net_output(&allocation, order, ctx) {
                        best_pair_net =
                            Some(best_pair_net.map_or(net.clone(), |best| best.max(net)));
                    }
                }
                CertifiedPairSolve::Unsupported => pair_unsupported += 1,
                CertifiedPairSolve::BudgetExceeded => pair_budget += 1,
            }
        }
    }
    let pair_delta = match (&best_pair_net, &heuristic_net) {
        (Some(pair), Some(heuristic)) => Some(pair - heuristic),
        _ => None,
    };
    let allocations = allocation
        .iter()
        .map(|path| {
            let gas = path
                .hops
                .iter()
                .fold(BigUint::from(0u8), |sum, hop| sum + &hop.gas);
            format!("{}:{}:{}", path.amount_in, path.amount_out, gas)
        })
        .collect::<Vec<_>>()
        .join(",");
    eprintln!(
        "allocation-support-audit: frontier={} support={} upper={} heuristic={} gap={} pair_best={} pair_delta={} pair_complete={} pair_unsupported={} pair_budget={} pair_ms={:.3} allocations=[{}]",
        frontier.len(),
        allocation.iter().filter(|path| !path.amount_in.is_zero()).count(),
        upper.as_ref().map_or_else(|| "unsupported".to_string(), ToString::to_string),
        heuristic_net.as_ref().map_or_else(|| "unsupported".to_string(), ToString::to_string),
        gap.as_ref().map_or_else(|| "unsupported".to_string(), ToString::to_string),
        best_pair_net.as_ref().map_or_else(|| "unsupported".to_string(), ToString::to_string),
        pair_delta.as_ref().map_or_else(|| "unsupported".to_string(), ToString::to_string),
        pair_complete,
        pair_unsupported,
        pair_budget,
        started.elapsed().as_secs_f64() * 1_000.0,
        allocations,
    );
    Ok(())
}

const SACRED_TIMELINE_CUTOFFS_CENTI_BPS: [u32; 5] = [0, 1, 10, 50, 100];

struct SacredTimelineCutoffScenario {
    centi_bps: u32,
    incumbent: BigInt,
    supported_frontiers: usize,
    frontiers_pruned: usize,
    refinements_avoided: usize,
    refinement_time_avoided_us: u128,
    incumbent_improvements: usize,
}

impl SacredTimelineCutoffScenario {
    fn new(centi_bps: u32, incumbent: &BigInt) -> Self {
        Self {
            centi_bps,
            incumbent: incumbent.clone(),
            supported_frontiers: 0,
            frontiers_pruned: 0,
            refinements_avoided: 0,
            refinement_time_avoided_us: 0,
            incumbent_improvements: 0,
        }
    }

    fn should_explore(&mut self, upper: &BigInt) -> bool {
        self.supported_frontiers = self
            .supported_frontiers
            .saturating_add(1);
        let prune = if self.centi_bps == 0 || self.incumbent <= BigInt::from(0u8) {
            upper <= &self.incumbent
        } else {
            // One basis point is 100 centi-bps and 1/10_000 of the incumbent.
            // Cross multiplication keeps the counterfactual decision exact.
            let scale = BigInt::from(1_000_000u32);
            upper * &scale <= &self.incumbent * (scale + BigInt::from(self.centi_bps))
        };
        if prune {
            self.frontiers_pruned = self.frontiers_pruned.saturating_add(1);
        }
        !prune
    }

    fn record_refinement(&mut self, explored: bool, elapsed_us: u128) {
        if !explored {
            self.refinements_avoided = self
                .refinements_avoided
                .saturating_add(1);
            self.refinement_time_avoided_us = self
                .refinement_time_avoided_us
                .saturating_add(elapsed_us);
        }
    }

    fn record_result(&mut self, explored: bool, net: &BigInt) {
        if explored && net > &self.incumbent {
            self.incumbent = net.clone();
            self.incumbent_improvements = self
                .incumbent_improvements
                .saturating_add(1);
        }
    }
}

#[cfg(test)]
mod sacred_timeline_cutoff_tests {
    use num_bigint::BigInt;

    use super::SacredTimelineCutoffScenario;

    #[test]
    fn test_exact_cutoff() {
        let incumbent = BigInt::from(1_000_000u32);
        let mut scenario = SacredTimelineCutoffScenario::new(0, &incumbent);

        assert!(!scenario.should_explore(&incumbent));
        assert!(scenario.should_explore(&(incumbent + 1u8)));
    }

    #[test]
    fn test_fractional_bps_cutoff() {
        let incumbent = BigInt::from(1_000_000u32);
        let mut scenario = SacredTimelineCutoffScenario::new(10, &incumbent);

        assert!(!scenario.should_explore(&BigInt::from(1_000_010u32)));
        assert!(scenario.should_explore(&BigInt::from(1_000_011u32)));
    }
}

/// Analysis-only accounting for histories that existing exact laws already prove irrelevant.
///
/// This observes production decisions rather than making new ones. `gross_bound_dead` is counted
/// only where the existing monotone gross-output bound rejects a complete portfolio, and resource
/// conflicts are counted only where pool disjointness makes an extension illegal.
struct SacredTimelineStats {
    prefix_states: usize,
    extension_attempts: usize,
    resource_conflicts: usize,
    complete_portfolios: usize,
    gross_bound_dead: usize,
    refinement_attempts: usize,
    refinement_successes: usize,
    refinement_failures: usize,
    canonical_histories: usize,
    ordered_histories: usize,
    v4_frontiers: usize,
    v4_subsets_represented: usize,
    v4_refinement_attempts: usize,
    v4_refinement_successes: usize,
    v4_incumbent_improvements: usize,
    v4_losing_frontiers: usize,
    v4_refinement_time_us: u128,
    v4_bound_supported: usize,
    v4_bound_unsupported: usize,
    v4_bound_pruned: usize,
    v4_finite_input_supported: usize,
    v4_finite_input_would_prune: usize,
    v4_finite_input_time_avoided_us: u128,
    v4_finite_input_violations: usize,
    cutoff_scenarios: [SacredTimelineCutoffScenario; 5],
}

impl SacredTimelineStats {
    fn new(incumbent: &BigInt) -> Self {
        Self {
            prefix_states: 0,
            extension_attempts: 0,
            resource_conflicts: 0,
            complete_portfolios: 0,
            gross_bound_dead: 0,
            refinement_attempts: 0,
            refinement_successes: 0,
            refinement_failures: 0,
            canonical_histories: 0,
            ordered_histories: 0,
            v4_frontiers: 0,
            v4_subsets_represented: 0,
            v4_refinement_attempts: 0,
            v4_refinement_successes: 0,
            v4_incumbent_improvements: 0,
            v4_losing_frontiers: 0,
            v4_refinement_time_us: 0,
            v4_bound_supported: 0,
            v4_bound_unsupported: 0,
            v4_bound_pruned: 0,
            v4_finite_input_supported: 0,
            v4_finite_input_would_prune: 0,
            v4_finite_input_time_avoided_us: 0,
            v4_finite_input_violations: 0,
            cutoff_scenarios: SACRED_TIMELINE_CUTOFFS_CENTI_BPS
                .map(|centi_bps| SacredTimelineCutoffScenario::new(centi_bps, incumbent)),
        }
    }

    fn cutoff_decisions(&mut self, upper: &BigInt) -> [bool; 5] {
        self.cutoff_scenarios
            .each_mut()
            .map(|scenario| scenario.should_explore(upper))
    }

    fn record_cutoff_refinement(&mut self, decisions: [bool; 5], elapsed_us: u128) {
        for (scenario, explored) in self
            .cutoff_scenarios
            .iter_mut()
            .zip(decisions)
        {
            scenario.record_refinement(explored, elapsed_us);
        }
    }

    fn record_cutoff_result(&mut self, decisions: [bool; 5], net: &BigInt) {
        for (scenario, explored) in self
            .cutoff_scenarios
            .iter_mut()
            .zip(decisions)
        {
            scenario.record_result(explored, net);
        }
    }

    fn record_complete(&mut self, width: usize, can_beat_incumbent: bool) {
        self.complete_portfolios = self
            .complete_portfolios
            .saturating_add(1);
        self.canonical_histories = self
            .canonical_histories
            .saturating_add(1);
        self.ordered_histories = self
            .ordered_histories
            .saturating_add((1..=width).product::<usize>());
        if !can_beat_incumbent {
            self.gross_bound_dead = self.gross_bound_dead.saturating_add(1);
        }
    }

    fn record_v4_frontier(&mut self, width: usize) {
        self.v4_frontiers = self.v4_frontiers.saturating_add(1);
        // A maximal width-n frontier represents every non-empty allocation support through zero
        // flow. Saturate because this is diagnostic accounting, never a search input.
        self.v4_subsets_represented = self
            .v4_subsets_represented
            .saturating_add(
                1usize
                    .checked_shl(width as u32)
                    .unwrap_or(usize::MAX)
                    .saturating_sub(1),
            );
    }

    fn emit(&self, actual_incumbent: &BigInt) {
        let dead_extensions = self
            .resource_conflicts
            .saturating_add(self.gross_bound_dead);
        let total_classified = self
            .extension_attempts
            .saturating_add(self.complete_portfolios);
        let dead_fraction = if total_classified == 0 {
            0.0
        } else {
            dead_extensions as f64 / total_classified as f64 * 100.0
        };
        let history_compression = if self.canonical_histories == 0 {
            0.0
        } else {
            self.ordered_histories as f64 / self.canonical_histories as f64
        };

        eprintln!("\n=== SacredTimelineCensusV1 ===");
        eprintln!("prefix states visited:              {}", self.prefix_states);
        eprintln!("path extensions considered:         {}", self.extension_attempts);
        eprintln!("resource-conflict extensions:       {}", self.resource_conflicts);
        eprintln!("complete portfolios classified:     {}", self.complete_portfolios);
        eprintln!("gross-bound certified dead:         {}", self.gross_bound_dead);
        eprintln!("classified dead fraction:           {dead_fraction:.2}%");
        eprintln!("refinement attempts:                {}", self.refinement_attempts);
        eprintln!("refinement successes:               {}", self.refinement_successes);
        eprintln!("refinement failures:                {}", self.refinement_failures);
        eprintln!("canonical portfolio histories:      {}", self.canonical_histories);
        eprintln!("equivalent ordered histories:       {}", self.ordered_histories);
        eprintln!("history quotient compression:       {history_compression:.2}x");
        eprintln!("maximal V4 frontiers:               {}", self.v4_frontiers);
        eprintln!("V4 allocation supports represented: {}", self.v4_subsets_represented);
        eprintln!("V4 refinement attempts:             {}", self.v4_refinement_attempts);
        eprintln!("V4 refinement successes:            {}", self.v4_refinement_successes);
        eprintln!("V4 incumbent improvements:          {}", self.v4_incumbent_improvements);
        eprintln!("V4 losing frontier results:         {}", self.v4_losing_frontiers);
        eprintln!("V4 refinement time us:              {}", self.v4_refinement_time_us);
        eprintln!("V4 bound-supported frontiers:       {}", self.v4_bound_supported);
        eprintln!("V4 bound-unsupported frontiers:     {}", self.v4_bound_unsupported);
        eprintln!("V4 frontiers pruned before refine:  {}", self.v4_bound_pruned);
        eprintln!("V4 finite-input bound supported:        {}", self.v4_finite_input_supported);
        eprintln!("V4 finite-input bound would prune:      {}", self.v4_finite_input_would_prune);
        eprintln!(
            "V4 finite-input refine time avoidable us: {}",
            self.v4_finite_input_time_avoided_us
        );
        eprintln!("V4 finite-input certificate violations: {}", self.v4_finite_input_violations);
        for scenario in &self.cutoff_scenarios {
            let loss = (actual_incumbent - &scenario.incumbent).max(BigInt::from(0u8));
            let loss_bps = match (loss.to_f64(), actual_incumbent.to_f64()) {
                (Some(loss), Some(actual)) if actual > 0.0 => loss / actual * 10_000.0,
                _ => 0.0,
            };
            eprintln!(
                "cutoff result centi_bps={} supported={} pruned={} refinements_avoided={} refinement_time_avoided_us={} improvements={} changed={} loss_bps={loss_bps:.9}",
                scenario.centi_bps,
                scenario.supported_frontiers,
                scenario.frontiers_pruned,
                scenario.refinements_avoided,
                scenario.refinement_time_avoided_us,
                scenario.incumbent_improvements,
                scenario.incumbent != *actual_incumbent,
            );
        }
        eprintln!("new pruning enabled:                no");
        eprintln!("routing output changed:             no");
        eprintln!("=== end SacredTimelineCensusV1 ===\n");
    }
}

#[derive(Default)]
struct CanonicalPortfolioStats {
    canonical_portfolios: usize,
    equivalent_ordered_histories: usize,
    max_portfolio_width: usize,
}

impl CanonicalPortfolioStats {
    fn record(&mut self, width: usize) {
        self.canonical_portfolios = self
            .canonical_portfolios
            .saturating_add(1);
        self.equivalent_ordered_histories = self
            .equivalent_ordered_histories
            .saturating_add((1..=width).product::<usize>());
        self.max_portfolio_width = self.max_portfolio_width.max(width);
    }

    fn emit(&self) {
        let compression = if self.canonical_portfolios == 0 {
            0.0
        } else {
            self.equivalent_ordered_histories as f64 / self.canonical_portfolios as f64
        };
        eprintln!("\n=== CanonicalPortfolioCensusV1 ===");
        eprintln!("canonical portfolios:          {}", self.canonical_portfolios);
        eprintln!("equivalent ordered histories: {}", self.equivalent_ordered_histories);
        eprintln!("history/canonical compression: {compression:.2}x");
        eprintln!("maximum portfolio width:       {}", self.max_portfolio_width);
        eprintln!("production search changed:     no");
        eprintln!("=== end CanonicalPortfolioCensusV1 ===\n");
    }
}

struct PathSearch<'a> {
    ctx: &'a BellmanFordContext,
    total: &'a BigUint,
    max_hops: usize,
    nodes: FxHashSet<NodeIndex>,
    components: FxHashSet<String>,
    descriptors: Vec<HopDescriptor>,
    paths: Vec<PathAllocation>,
    /// Complete structural paths before full-order feasibility filtering. This is
    /// populated only when an analysis census is enabled and never affects the
    /// production candidate frontier.
    census_paths: Vec<Vec<HopDescriptor>>,
    collect_census: bool,
    include_v4: bool,
}

impl PathSearch<'_> {
    fn visit(&mut self, node: NodeIndex) -> Result<(), AlgorithmError> {
        if node == self.ctx.token_out_node && !self.descriptors.is_empty() {
            if self.collect_census {
                self.census_paths
                    .push(self.descriptors.clone());
            }

            let Ok(sim) = simulate_path(
                &self.descriptors,
                self.total,
                &self.ctx.market_data,
                &MarketOverrides::empty(),
            ) else {
                return Ok(());
            };
            let hops = self
                .descriptors
                .iter()
                .cloned()
                .zip(sim.hop_results)
                .map(|(descriptor, (amount_out, gas))| SimulatedHop { descriptor, amount_out, gas })
                .collect();
            self.paths.push(PathAllocation {
                hops,
                flow_fraction: 1.0,
                amount_in: self.total.clone(),
                amount_out: sim.amount_out,
                marginal_price_product: sim.marginal_price_product,
            });
            return Ok(());
        }
        if self.descriptors.len() == self.max_hops {
            return Ok(());
        }

        let Some(edges) = self.ctx.adj.get(&node) else { return Ok(()) };
        for (next, component_id) in edges {
            if self.nodes.contains(next) || self.components.contains(component_id) {
                continue;
            }
            let Some(state) = self
                .ctx
                .market_data
                .get_simulation_state(component_id)
            else {
                continue;
            };
            if state
                .as_any()
                .downcast_ref::<UniswapV2State>()
                .is_none() &&
                state
                    .as_any()
                    .downcast_ref::<UniswapV3State>()
                    .is_none() &&
                (!self.include_v4 ||
                    state
                        .as_any()
                        .downcast_ref::<UniswapV4State>()
                        .is_none())
            {
                continue;
            }
            let (Some(token_in), Some(token_out)) =
                (self.ctx.token_map.get(&node), self.ctx.token_map.get(next))
            else {
                continue;
            };
            self.nodes.insert(*next);
            self.components
                .insert(component_id.clone());
            self.descriptors
                .push(HopDescriptor::new(
                    component_id.clone(),
                    token_in.as_ref().clone(),
                    token_out.as_ref().clone(),
                ));
            self.visit(*next)?;
            self.descriptors.pop();
            self.components.remove(component_id);
            self.nodes.remove(next);
        }
        Ok(())
    }
}

fn protocol_word(path: &[HopDescriptor], ctx: &BellmanFordContext) -> String {
    path.iter()
        .map(|hop| {
            let Some(state) = ctx
                .market_data
                .get_simulation_state(&hop.component_id)
            else {
                return '?';
            };
            if state
                .as_any()
                .downcast_ref::<UniswapV2State>()
                .is_some()
            {
                '2'
            } else if state
                .as_any()
                .downcast_ref::<UniswapV3State>()
                .is_some()
            {
                '3'
            } else if state
                .as_any()
                .downcast_ref::<UniswapV4State>()
                .is_some()
            {
                '4'
            } else {
                '?'
            }
        })
        .collect()
}

fn scaled_amount(total: &BigUint, numerator: u64, denominator: u64) -> BigUint {
    let mut amount = total * BigUint::from(numerator);
    amount /= BigUint::from(denominator);
    if amount == BigUint::from(0u8) && *total != BigUint::from(0u8) {
        BigUint::from(1u8)
    } else {
        amount
    }
}

#[derive(Default)]
struct SignatureCounts {
    total: usize,
    survivors: usize,
}

/// Analysis-only differential inspired by the scheduler's dead-language census.
///
/// The concrete oracle is exact simulator replay at several flow scales. A path
/// is a "survivor" iff it enters the top-K frontier at any scale. The abstract
/// protocol word (e.g. `33`, `23`) is comparison-only: no production pruning is
/// derived from it. This lets us measure whether dead path families are pure or
/// mixed before attempting an automaton/cut-obligation implementation.
fn emit_language_census(
    structural_paths: &[Vec<HopDescriptor>],
    total: &BigUint,
    ctx: &BellmanFordContext,
) {
    if structural_paths.is_empty() {
        eprintln!("routing-language-census: no complete structural paths");
        return;
    }

    let mut outputs: Vec<Vec<Option<BigUint>>> =
        vec![vec![None; CENSUS_SCALES.len()]; structural_paths.len()];
    let mut feasible_by_scale = vec![0usize; CENSUS_SCALES.len()];

    for (path_index, path) in structural_paths.iter().enumerate() {
        for (scale_index, &(num, den)) in CENSUS_SCALES.iter().enumerate() {
            let amount = scaled_amount(total, num, den);
            if let Ok(sim) =
                simulate_path(path, &amount, &ctx.market_data, &MarketOverrides::empty())
            {
                feasible_by_scale[scale_index] += 1;
                outputs[path_index][scale_index] = Some(sim.amount_out);
            }
        }
    }

    let mut survivor = vec![false; structural_paths.len()];
    let mut first_surviving_scale = vec![None; structural_paths.len()];
    let mut full_topk = FxHashSet::default();

    for scale_index in 0..CENSUS_SCALES.len() {
        let mut ranked = outputs
            .iter()
            .enumerate()
            .filter_map(|(index, values)| {
                values[scale_index]
                    .as_ref()
                    .map(|value| (index, value))
            })
            .collect::<Vec<_>>();
        ranked.sort_unstable_by(|(_, a), (_, b)| b.cmp(a));
        ranked.truncate(MAX_CANDIDATE_PATHS);
        for (index, _) in ranked {
            survivor[index] = true;
            first_surviving_scale[index].get_or_insert(scale_index);
            if scale_index == 0 {
                full_topk.insert(index);
            }
        }
    }

    let survivors = survivor
        .iter()
        .filter(|&&value| value)
        .count();
    let rescued = survivor
        .iter()
        .enumerate()
        .filter(|(index, value)| **value && !full_topk.contains(index))
        .count();
    let full_infeasible_but_smaller_feasible = outputs
        .iter()
        .filter(|values| {
            values[0].is_none() &&
                values
                    .iter()
                    .skip(1)
                    .any(Option::is_some)
        })
        .count();

    let mut signatures: FxHashMap<String, SignatureCounts> = FxHashMap::default();
    for (index, path) in structural_paths.iter().enumerate() {
        let entry = signatures
            .entry(protocol_word(path, ctx))
            .or_default();
        entry.total += 1;
        if survivor[index] {
            entry.survivors += 1;
        }
    }

    let mut fully_surviving = 0usize;
    let mut mixed = 0usize;
    let mut fully_dead = 0usize;
    for counts in signatures.values() {
        if counts.survivors == 0 {
            fully_dead += 1;
        } else if counts.survivors == counts.total {
            fully_surviving += 1;
        } else {
            mixed += 1;
        }
    }

    eprintln!("\n=== RoutingMultiScaleLanguageCensusV1 ===");
    eprintln!("structural paths:              {}", structural_paths.len());
    eprintln!("full-order feasible:           {}", feasible_by_scale[0]);
    for (index, &(num, den)) in CENSUS_SCALES.iter().enumerate() {
        eprintln!(
            "feasible at {:>3}%:              {}",
            (100 * num) / den,
            feasible_by_scale[index]
        );
    }
    eprintln!("current full-size top-K:       {}", full_topk.len());
    eprintln!("multi-scale frontier:          {}", survivors);
    eprintln!("rescued by smaller-flow probe: {}", rescued);
    eprintln!("full-infeasible / smaller-feasible: {}", full_infeasible_but_smaller_feasible);
    eprintln!("abstract protocol signatures:  {}", signatures.len());
    eprintln!("  fully surviving:             {}", fully_surviving);
    eprintln!("  mixed:                       {}", mixed);
    eprintln!("  fully dead:                  {}", fully_dead);

    let mut rescued_paths = structural_paths
        .iter()
        .enumerate()
        .filter(|(index, _)| survivor[*index] && !full_topk.contains(index))
        .collect::<Vec<_>>();
    rescued_paths.sort_unstable_by_key(|(index, _)| first_surviving_scale[*index]);
    if !rescued_paths.is_empty() {
        eprintln!("rescued paths (up to 12):");
        for (index, path) in rescued_paths.into_iter().take(12) {
            let scale_index = first_surviving_scale[index].unwrap_or(0);
            let (num, den) = CENSUS_SCALES[scale_index];
            let components = path
                .iter()
                .map(|hop| hop.component_id.as_str())
                .collect::<Vec<_>>()
                .join(" -> ");
            eprintln!(
                "  scale={:>3}% word={} {}",
                (100 * num) / den,
                protocol_word(path, ctx),
                components
            );
        }
    }
    eprintln!("=== end RoutingMultiScaleLanguageCensusV1 ===\n");
}

fn paths_are_pool_disjoint(left: &[HopDescriptor], right: &[HopDescriptor]) -> bool {
    let used = left
        .iter()
        .map(|hop| hop.component_id.as_str())
        .collect::<FxHashSet<_>>();
    right
        .iter()
        .all(|hop| !used.contains(hop.component_id.as_str()))
}

struct ExactPathCache<'a> {
    paths: &'a [Vec<HopDescriptor>],
    ctx: &'a BellmanFordContext,
    values: Vec<FxHashMap<BigUint, Option<BigUint>>>,
    simulations: usize,
}

impl<'a> ExactPathCache<'a> {
    fn new(paths: &'a [Vec<HopDescriptor>], ctx: &'a BellmanFordContext) -> Self {
        Self { paths, ctx, values: vec![FxHashMap::default(); paths.len()], simulations: 0 }
    }

    fn output(&mut self, path_index: usize, amount: &BigUint) -> Option<BigUint> {
        if amount == &BigUint::from(0u8) {
            return Some(BigUint::from(0u8));
        }
        if let Some(value) = self.values[path_index].get(amount) {
            return value.clone();
        }
        self.simulations += 1;
        let value = simulate_path(
            &self.paths[path_index],
            amount,
            &self.ctx.market_data,
            &MarketOverrides::empty(),
        )
        .ok()
        .map(|sim| sim.amount_out);
        self.values[path_index].insert(amount.clone(), value.clone());
        value
    }
}

#[derive(Default)]
struct SymbolicCutStats {
    intervals: usize,
    certified_dead: usize,
    unresolved_leaves: usize,
    witness_leaves: usize,
    endpoint_unknown: usize,
    falsifier_violations: usize,
    first_witness_x: Option<BigUint>,
    best_witness: Option<BigUint>,
}

fn pair_output(
    cache: &mut ExactPathCache<'_>,
    anchor: usize,
    alternative: usize,
    total: &BigUint,
    x: &BigUint,
) -> Option<BigUint> {
    if x > total {
        return None;
    }
    let anchor_amount = total - x;
    let anchor_out = cache.output(anchor, &anchor_amount)?;
    let alternative_out = cache.output(alternative, x)?;
    Some(anchor_out + alternative_out)
}

/// Conservative upper bound for `x in [lo, hi]` where `x` is flow sent to the
/// alternative path. Exact-input V2/V3/V4 path output is monotone, therefore
///
///   Q_anchor(T - x) <= Q_anchor(T - lo)
///   Q_alt(x)        <= Q_alt(hi)
///
/// and the sum is a sound (possibly loose) upper bound. If either endpoint
/// replay fails we return `None`: analysis must remain unknown rather than turn
/// missing simulator information into a false impossibility proof.
fn pair_interval_upper_bound(
    cache: &mut ExactPathCache<'_>,
    anchor: usize,
    alternative: usize,
    total: &BigUint,
    lo: &BigUint,
    hi: &BigUint,
) -> Option<BigUint> {
    let anchor_amount = total - lo;
    let anchor_out = cache.output(anchor, &anchor_amount)?;
    let alternative_out = cache.output(alternative, hi)?;
    Some(anchor_out + alternative_out)
}

fn analyze_symbolic_cut(
    cache: &mut ExactPathCache<'_>,
    anchor: usize,
    alternative: usize,
    total: &BigUint,
    incumbent: &BigUint,
    lo: BigUint,
    hi: BigUint,
    depth: usize,
    stats: &mut SymbolicCutStats,
) {
    stats.intervals += 1;

    let Some(upper_bound) = pair_interval_upper_bound(cache, anchor, alternative, total, &lo, &hi)
    else {
        stats.endpoint_unknown += 1;
        return;
    };

    if upper_bound <= *incumbent {
        // Differential falsifier: sampled concrete points inside a certified-dead
        // interval must never beat the incumbent. The upper-bound argument is
        // the certificate; these exact probes are only an instrumentation guard.
        let mid = (&lo + &hi) / BigUint::from(2u8);
        for x in [&lo, &mid, &hi] {
            if let Some(exact) = pair_output(cache, anchor, alternative, total, x) {
                if exact > *incumbent {
                    stats.falsifier_violations += 1;
                }
            }
        }
        stats.certified_dead += 1;
        return;
    }

    let mid = (&lo + &hi) / BigUint::from(2u8);
    let mut witnessed = false;
    for x in [&lo, &mid, &hi] {
        if let Some(exact) = pair_output(cache, anchor, alternative, total, x) {
            if exact > *incumbent {
                witnessed = true;
                if stats.first_witness_x.is_none() {
                    stats.first_witness_x = Some(x.clone());
                }
                if stats
                    .best_witness
                    .as_ref()
                    .map_or(true, |best| exact > *best)
                {
                    stats.best_witness = Some(exact);
                }
            }
        }
    }

    if depth == SYMBOLIC_CUT_DEPTH || lo == hi {
        if witnessed {
            stats.witness_leaves += 1;
        } else {
            stats.unresolved_leaves += 1;
        }
        return;
    }

    if mid < hi {
        analyze_symbolic_cut(
            cache,
            anchor,
            alternative,
            total,
            incumbent,
            lo.clone(),
            mid.clone(),
            depth + 1,
            stats,
        );
        let right_lo = &mid + BigUint::from(1u8);
        if right_lo <= hi {
            analyze_symbolic_cut(
                cache,
                anchor,
                alternative,
                total,
                incumbent,
                right_lo,
                hi,
                depth + 1,
                stats,
            );
        }
    }
}

/// Analysis-only routing analogue of the scheduler's relational cut census.
///
/// The best exact full-order single path is the incumbent/anchor. Every
/// pool-disjoint structural alternative receives symbolic flow `x in [0,T]`.
/// We recursively prove allocation intervals dead using only a monotone
/// conservative upper bound; ambiguous intervals are refined and exact point
/// replay supplies productive witnesses. No result from this census changes the
/// production frontier or route selection.
fn emit_symbolic_allocation_cut_census(
    structural_paths: &[Vec<HopDescriptor>],
    total: &BigUint,
    ctx: &BellmanFordContext,
) {
    if structural_paths.is_empty() || total == &BigUint::from(0u8) {
        return;
    }

    let mut cache = ExactPathCache::new(structural_paths, ctx);
    let mut full_outputs = Vec::with_capacity(structural_paths.len());
    for index in 0..structural_paths.len() {
        full_outputs.push(cache.output(index, total));
    }
    let Some((anchor, incumbent)) = full_outputs
        .iter()
        .enumerate()
        .filter_map(|(index, output)| {
            output
                .as_ref()
                .map(|value| (index, value.clone()))
        })
        .max_by(|(_, a), (_, b)| a.cmp(b))
    else {
        eprintln!("symbolic-allocation-cut-census: no full-order feasible path");
        return;
    };

    let mut pairs = 0usize;
    let mut pair_root_dead = 0usize;
    let mut pair_has_witness = 0usize;
    let mut pair_unresolved = 0usize;
    let mut total_intervals = 0usize;
    let mut certified_dead = 0usize;
    let mut unresolved_leaves = 0usize;
    let mut witness_leaves = 0usize;
    let mut endpoint_unknown = 0usize;
    let mut falsifier_violations = 0usize;
    let mut witnesses = Vec::new();

    for alternative in 0..structural_paths.len() {
        if alternative == anchor ||
            !paths_are_pool_disjoint(&structural_paths[anchor], &structural_paths[alternative])
        {
            continue;
        }
        pairs += 1;
        let mut stats = SymbolicCutStats::default();
        analyze_symbolic_cut(
            &mut cache,
            anchor,
            alternative,
            total,
            &incumbent,
            BigUint::from(0u8),
            total.clone(),
            0,
            &mut stats,
        );

        if stats.intervals == 1 && stats.certified_dead == 1 {
            pair_root_dead += 1;
        }
        if stats.first_witness_x.is_some() {
            pair_has_witness += 1;
            witnesses.push((alternative, stats.first_witness_x.clone().unwrap()));
        } else if stats.certified_dead == 0 ||
            stats.unresolved_leaves > 0 ||
            stats.endpoint_unknown > 0
        {
            pair_unresolved += 1;
        }
        total_intervals += stats.intervals;
        certified_dead += stats.certified_dead;
        unresolved_leaves += stats.unresolved_leaves;
        witness_leaves += stats.witness_leaves;
        endpoint_unknown += stats.endpoint_unknown;
        falsifier_violations += stats.falsifier_violations;
    }

    let anchor_components = structural_paths[anchor]
        .iter()
        .map(|hop| hop.component_id.as_str())
        .collect::<Vec<_>>()
        .join(" -> ");

    eprintln!("\n=== SymbolicAllocationCutCensusV1 ===");
    eprintln!("structural paths:              {}", structural_paths.len());
    eprintln!("anchor word:                   {}", protocol_word(&structural_paths[anchor], ctx));
    eprintln!("anchor path:                   {}", anchor_components);
    eprintln!("incumbent gross raw:           {}", incumbent);
    eprintln!("pool-disjoint alternatives:    {}", pairs);
    eprintln!("pair roots certified dead:     {}", pair_root_dead);
    eprintln!("pairs with exact win witness:  {}", pair_has_witness);
    eprintln!("pairs still unresolved:        {}", pair_unresolved);
    eprintln!("interval nodes examined:       {}", total_intervals);
    eprintln!("certified-dead intervals:      {}", certified_dead);
    eprintln!("witness leaves:                {}", witness_leaves);
    eprintln!("unresolved leaves:             {}", unresolved_leaves);
    eprintln!("endpoint-unknown intervals:    {}", endpoint_unknown);
    eprintln!("exact simulator calls:         {}", cache.simulations);
    eprintln!("dead-certificate violations:   {}", falsifier_violations);

    if !witnesses.is_empty() {
        eprintln!("productive alternatives (up to 12):");
        for (alternative, x) in witnesses.into_iter().take(12) {
            let bps = ((&x * BigUint::from(10_000u64)) / total)
                .to_u64()
                .unwrap_or(0);
            let components = structural_paths[alternative]
                .iter()
                .map(|hop| hop.component_id.as_str())
                .collect::<Vec<_>>()
                .join(" -> ");
            eprintln!(
                "  witness~{}.{:02}% word={} {}",
                bps / 100,
                bps % 100,
                protocol_word(&structural_paths[alternative], ctx),
                components
            );
        }
    }
    eprintln!("=== end SymbolicAllocationCutCensusV1 ===\n");
}

const CERTIFIED_STRUCTURAL_INTERVAL_DEPTH: usize = 6;

#[derive(Default)]
struct CertifiedStructuralIntervalStats {
    pairs: usize,
    intervals: usize,
    certified_dead: usize,
    unresolved_leaves: usize,
    dead_width: BigUint,
    total_width: BigUint,
    dead_by_depth: [usize; CERTIFIED_STRUCTURAL_INTERVAL_DEPTH + 1],
    falsifier_probes: usize,
    falsifier_unknown: usize,
    falsifier_violations: usize,
}

fn is_v2_path(path: &[HopDescriptor], ctx: &BellmanFordContext) -> bool {
    !path.is_empty() &&
        path.iter().all(|hop| {
            ctx.market_data
                .get_simulation_state(&hop.component_id)
                .is_some_and(|state| {
                    state
                        .as_any()
                        .downcast_ref::<UniswapV2State>()
                        .is_some()
                })
        })
}

fn analyze_certified_structural_interval(
    cache: &mut ExactPathCache<'_>,
    left: usize,
    right: usize,
    total: &BigUint,
    incumbent: &BigUint,
    lo: BigUint,
    hi: BigUint,
    depth: usize,
    stats: &mut CertifiedStructuralIntervalStats,
) {
    stats.intervals += 1;
    let Some(upper) = certified_v2_interval_upper_for_paths(
        &cache.paths[left],
        &cache.paths[right],
        total,
        &lo,
        &hi,
        &cache.ctx.market_data,
    ) else {
        stats.unresolved_leaves += 1;
        return;
    };

    let midpoint = (&lo + &hi) / BigUint::from(2u8);
    if upper <= *incumbent {
        stats.certified_dead += 1;
        stats.dead_by_depth[depth] += 1;
        stats.dead_width += &hi - &lo;
        for x in [&lo, &midpoint, &hi] {
            match pair_output(cache, left, right, total, x) {
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

    if depth == CERTIFIED_STRUCTURAL_INTERVAL_DEPTH || lo == hi {
        stats.unresolved_leaves += 1;
        return;
    }

    if midpoint > lo {
        analyze_certified_structural_interval(
            cache,
            left,
            right,
            total,
            incumbent,
            lo.clone(),
            midpoint.clone(),
            depth + 1,
            stats,
        );
    }
    if midpoint < hi {
        analyze_certified_structural_interval(
            cache,
            left,
            right,
            total,
            incumbent,
            midpoint,
            hi,
            depth + 1,
            stats,
        );
    }
}

fn emit_v2_certified_structural_interval_census(
    structural_paths: &[Vec<HopDescriptor>],
    total: &BigUint,
    ctx: &BellmanFordContext,
) {
    if structural_paths.len() < 2 || total == &BigUint::from(0u8) {
        return;
    }

    let mut cache = ExactPathCache::new(structural_paths, ctx);
    let full_outputs = (0..structural_paths.len())
        .map(|index| cache.output(index, total))
        .collect::<Vec<_>>();
    let mut stats = CertifiedStructuralIntervalStats::default();

    for left in 0..structural_paths.len() {
        if !is_v2_path(&structural_paths[left], ctx) {
            continue;
        }
        for right in left + 1..structural_paths.len() {
            if !is_v2_path(&structural_paths[right], ctx) ||
                !paths_are_pool_disjoint(&structural_paths[left], &structural_paths[right])
            {
                continue;
            }
            let (Some(left_full), Some(right_full)) = (&full_outputs[left], &full_outputs[right])
            else {
                continue;
            };
            stats.pairs += 1;
            if std::env::var_os("FYND_V2_CERTIFIED_INTERVAL_SHADOW").is_some() {
                let _ = super::exact_v2_refiner::shadow_compare_structural_pair(
                    &structural_paths[left],
                    &structural_paths[right],
                    total,
                    &ctx.market_data,
                );
            }
            stats.total_width += total;
            let incumbent = left_full
                .clone()
                .max(right_full.clone());
            analyze_certified_structural_interval(
                &mut cache,
                left,
                right,
                total,
                &incumbent,
                BigUint::from(0u8),
                total.clone(),
                0,
                &mut stats,
            );
        }
    }

    let dead_bps = if stats.total_width == BigUint::from(0u8) {
        0u64
    } else {
        ((&stats.dead_width * BigUint::from(10_000u64)) / &stats.total_width)
            .to_u64()
            .unwrap_or(10_000)
            .min(10_000)
    };

    eprintln!("\n=== V2CertifiedStructuralIntervalCensusV1 ===");
    eprintln!("structural paths:              {}", structural_paths.len());
    eprintln!("V2 pool-disjoint pairs:        {}", stats.pairs);
    eprintln!("interval nodes examined:       {}", stats.intervals);
    eprintln!("certified-dead intervals:      {}", stats.certified_dead);
    eprintln!("unresolved depth-6 leaves:     {}", stats.unresolved_leaves);
    eprintln!("allocation width dead:         {}.{:02}%", dead_bps / 100, dead_bps % 100);
    eprintln!("exact falsifier probes:        {}", stats.falsifier_probes);
    eprintln!("falsifier unknown probes:      {}", stats.falsifier_unknown);
    eprintln!("certificate violations:        {}", stats.falsifier_violations);
    eprintln!("dead intervals by depth:");
    for (depth, count) in stats.dead_by_depth.iter().enumerate() {
        eprintln!("  depth {}: {}", depth, count);
    }
    eprintln!("exact simulator calls:         {}", cache.simulations);
    eprintln!("=== end V2CertifiedStructuralIntervalCensusV1 ===\n");
}

fn pure_v2_allocation(path: &PathAllocation, ctx: &BellmanFordContext) -> bool {
    !path.hops.is_empty() &&
        path.hops.iter().all(|hop| {
            ctx.market_data
                .get_simulation_state(&hop.descriptor.component_id)
                .is_some_and(|state| {
                    state
                        .as_any()
                        .downcast_ref::<UniswapV2State>()
                        .is_some()
                })
        })
}

fn contains_v4(path: &PathAllocation, ctx: &BellmanFordContext) -> bool {
    path.hops.iter().any(|hop| {
        ctx.market_data
            .get_simulation_state(&hop.descriptor.component_id)
            .is_some_and(|state| {
                state
                    .as_any()
                    .downcast_ref::<UniswapV4State>()
                    .is_some()
            })
    })
}

fn allocation_paths_pool_disjoint(left: &PathAllocation, right: &PathAllocation) -> bool {
    // Theorem 2, assumption 3 (separable replay): two admitted paths may not mutate the same
    // component. This predicate establishes pairwise resource disjointness; replay still uses the
    // common immutable `ctx.market_data` snapshot at the call site below.
    let used = left
        .hops
        .iter()
        .map(|hop| hop.descriptor.component_id.as_str())
        .collect::<FxHashSet<_>>();
    right
        .hops
        .iter()
        .all(|hop| !used.contains(hop.descriptor.component_id.as_str()))
}

fn same_allocation_path(left: &PathAllocation, right: &PathAllocation) -> bool {
    left.hops.len() == right.hops.len() &&
        left.hops
            .iter()
            .zip(&right.hops)
            .all(|(a, b)| a.descriptor.component_id == b.descriptor.component_id)
}

fn v4_compatible_portfolios(
    paths: &[PathAllocation],
    ctx: &BellmanFordContext,
    max_paths: Option<usize>,
) -> Vec<Vec<PathAllocation>> {
    // Theorem 2, maximal-frontier cover. With `max_paths == None`, every emitted portfolio is a
    // maximal set under component-disjoint compatibility. With `Some(k)`, this is the exhaustive
    // bounded-subset control and is not the maximal-frontier quotient.
    fn visit(
        paths: &[PathAllocation],
        ctx: &BellmanFordContext,
        max_paths: Option<usize>,
        index: usize,
        selected: &mut Vec<usize>,
        used: &mut FxHashSet<String>,
        portfolios: &mut Vec<Vec<PathAllocation>>,
    ) {
        if index != paths.len() {
            visit(paths, ctx, max_paths, index + 1, selected, used, portfolios);
            if max_paths.is_none_or(|max| selected.len() < max) &&
                paths[index]
                    .hops
                    .iter()
                    .all(|hop| !used.contains(&hop.descriptor.component_id))
            {
                for hop in &paths[index].hops {
                    used.insert(hop.descriptor.component_id.clone());
                }
                selected.push(index);
                visit(paths, ctx, max_paths, index + 1, selected, used, portfolios);
                selected.pop();
                for hop in &paths[index].hops {
                    used.remove(&hop.descriptor.component_id);
                }
            }
            return;
        }

        if !selected
            .iter()
            .any(|&index| contains_v4(&paths[index], ctx))
        {
            return;
        }
        let maximal = paths
            .iter()
            .enumerate()
            .all(|(index, path)| {
                selected.contains(&index) ||
                    path.hops
                        .iter()
                        .any(|hop| used.contains(&hop.descriptor.component_id))
            });
        if max_paths.is_some() || maximal {
            portfolios.push(
                selected
                    .iter()
                    .map(|&index| paths[index].clone())
                    .collect(),
            );
        }
    }

    let mut portfolios = Vec::new();
    visit(paths, ctx, max_paths, 0, &mut Vec::new(), &mut FxHashSet::default(), &mut portfolios);
    portfolios
}

fn ordered_portfolio_histories(portfolios: Vec<Vec<PathAllocation>>) -> Vec<Vec<PathAllocation>> {
    fn visit(
        remaining: &mut Vec<PathAllocation>,
        ordered: &mut Vec<PathAllocation>,
        histories: &mut Vec<Vec<PathAllocation>>,
    ) {
        if remaining.is_empty() {
            histories.push(ordered.clone());
            return;
        }
        for index in 0..remaining.len() {
            let path = remaining.remove(index);
            ordered.push(path);
            visit(remaining, ordered, histories);
            let path = ordered
                .pop()
                .expect("ordered history is non-empty");
            remaining.insert(index, path);
        }
    }

    let mut histories = Vec::new();
    for mut portfolio in portfolios {
        visit(&mut portfolio, &mut Vec::new(), &mut histories);
    }
    histories
}

/// Returns a gross-output upper bound for a compatible subset when every path was simulated at
/// the full order amount by one of the standard monotone Uniswap simulators.
///
/// Each path receives at most `total` in any allocation. Monotonicity therefore bounds its output
/// by the already-recorded full-input output. Summing those bounds and ignoring gas can only
/// overestimate the attainable net output.
/// Returns whether a V4 simulator state is passive for the monotonicity certificate.
///
/// This is intentionally stricter than "the simulator can execute this pool." A nonzero hook may
/// alter the specified amount, return balance deltas, override the LP fee, mutate hook state, add
/// input-dependent gas, or depend on state outside the pool component. Any of those can invalidate
/// the output-monotonicity or complete-resource-footprint premises used by the bound. Such pools
/// remain routable through ordinary simulator replay; they are merely unsupported by this
/// certificate family.
fn passive_uniswap_v4_state(state: &UniswapV4State) -> bool {
    state
        .hook
        .as_ref()
        .is_none_or(|hook| hook.address() == Address::ZERO)
}

fn standard_uniswap_path(path: &PathAllocation, ctx: &BellmanFordContext) -> bool {
    path.hops.iter().all(|hop| {
        ctx.market_data
            .get_simulation_state(&hop.descriptor.component_id)
            .is_some_and(|state| {
                let state = state.as_any();
                state
                    .downcast_ref::<UniswapV2State>()
                    .is_some() ||
                    state
                        .downcast_ref::<UniswapV3State>()
                        .is_some() ||
                    state
                        .downcast_ref::<UniswapV4State>()
                        .is_some_and(passive_uniswap_v4_state)
            })
    })
}

/// Returns a zero-cost gross-output upper bound for a compatible frontier.
///
/// Every path was already simulated with the complete order during discovery. For standard
/// exact-input V2/V3 and passive V4 pools, output is monotone in input, while an allocation gives
/// each path at most the complete order. Summing those full-order outputs therefore upper-bounds
/// every allocation over the frontier. Ignoring gas only makes the bound more conservative.
/// V4 handlers at nonzero addresses are deliberately unsupported because arbitrary hook behavior
/// need not preserve this monotonicity law.
fn monotone_full_input_upper_bound(
    paths: &[PathAllocation],
    ctx: &BellmanFordContext,
) -> Option<BigInt> {
    paths
        .iter()
        .all(|path| standard_uniswap_path(path, ctx))
        .then(|| {
            paths
                .iter()
                .fold(BigInt::from(0u8), |sum, path| sum + BigInt::from(path.amount_out.clone()))
        })
}

fn monotone_output_envelopes(
    paths: &[PathAllocation],
    total: &BigUint,
    ctx: &BellmanFordContext,
) -> Vec<Option<Vec<BigUint>>> {
    paths
        .iter()
        .map(|path| {
            if !standard_uniswap_path(path, ctx) {
                return None;
            }
            let descriptors = path
                .hops
                .iter()
                .map(|hop| hop.descriptor.clone())
                .collect::<Vec<_>>();
            let mut outputs = Vec::with_capacity(CERTIFIED_V4_BOUND_GRID + 1);
            outputs.push(BigUint::from(0u8));
            for step in 1..=CERTIFIED_V4_BOUND_GRID {
                let numerator = total * BigUint::from(step);
                let amount = (numerator + BigUint::from(CERTIFIED_V4_BOUND_GRID - 1)) /
                    BigUint::from(CERTIFIED_V4_BOUND_GRID);
                outputs.push(
                    simulate_path(
                        &descriptors,
                        &amount,
                        &ctx.market_data,
                        &MarketOverrides::empty(),
                    )
                    .ok()?
                    .amount_out,
                );
            }
            Some(outputs)
        })
        .collect()
}

/// Bounds a subset by rounding each real allocation up to its next monotone-envelope grid point.
fn monotone_grid_upper_bound(
    subset: &[PathAllocation],
    all_paths: &[PathAllocation],
    envelopes: &[Option<Vec<BigUint>>],
) -> Option<BigUint> {
    let budget = CERTIFIED_V4_BOUND_GRID + subset.len().saturating_sub(1);
    let mut dp = vec![None; budget + 1];
    dp[0] = Some(BigUint::from(0u8));
    for path in subset {
        let index = all_paths
            .iter()
            .position(|candidate| same_allocation_path(candidate, path))?;
        let outputs = envelopes.get(index)?.as_ref()?;
        let mut next = vec![None; budget + 1];
        for (used, current) in dp.iter().enumerate() {
            let Some(current) = current else { continue };
            for (step, output) in outputs.iter().enumerate() {
                if used + step > budget {
                    break;
                }
                let candidate = current + output;
                let slot = &mut next[used + step];
                if slot
                    .as_ref()
                    .is_none_or(|best| candidate > *best)
                {
                    *slot = Some(candidate);
                }
            }
        }
        dp = next;
    }
    dp.into_iter().flatten().max()
}

fn allocation_net_output(
    allocation: &[PathAllocation],
    order: &Order,
    ctx: &BellmanFordContext,
) -> Option<BigInt> {
    let route = build_split_route(allocation, &ctx.market_data, order).ok()?;
    route.validate().ok()?;
    PathFrankWolfeAlgorithm::compute_split_net_amount_out(&route, ctx).ok()
}

fn multi_scale_frontier(
    paths: &[PathAllocation],
    total: &BigUint,
    ctx: &BellmanFordContext,
) -> Vec<PathAllocation> {
    let detailed_trace = std::env::var_os("FYND_MULTI_SCALE_FRONTIER_DETAIL").is_some();
    let mut frontier = paths
        .iter()
        .take(MAX_CANDIDATE_PATHS)
        .cloned()
        .collect::<Vec<_>>();

    // Start with the smallest probes: these are the paths most likely to be
    // hidden by full-order ranking while still being valuable as a marginal
    // split leg.
    for &(numerator, denominator) in MULTI_SCALE_FRONTIER_SCALES.iter().rev() {
        if frontier.len() == MAX_MULTI_SCALE_CANDIDATE_PATHS {
            break;
        }
        let amount = scaled_amount(total, numerator, denominator);
        let mut ranked = paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| {
                let descriptors = path
                    .hops
                    .iter()
                    .map(|hop| hop.descriptor.clone())
                    .collect::<Vec<_>>();
                simulate_path(&descriptors, &amount, &ctx.market_data, &MarketOverrides::empty())
                    .ok()
                    .map(|simulation| (index, simulation.amount_out))
            })
            .collect::<Vec<_>>();
        ranked.sort_unstable_by(|(left_index, left_output), (right_index, right_output)| {
            right_output
                .cmp(left_output)
                .then_with(|| left_index.cmp(right_index))
        });

        if detailed_trace {
            eprintln!("multi-scale-detail: scale={numerator}/{denominator}");
            for (rank, (index, output)) in ranked.iter().enumerate() {
                let route = paths[*index]
                    .hops
                    .iter()
                    .map(|hop| hop.descriptor.component_id.as_str())
                    .collect::<Vec<_>>()
                    .join("|");
                eprintln!("  rank={} output={} route={}", rank + 1, output, route);
            }
        }

        for (index, _) in ranked
            .into_iter()
            .take(MULTI_SCALE_PATHS_PER_SCALE)
        {
            let candidate = &paths[index];
            if !frontier
                .iter()
                .any(|path| same_allocation_path(path, candidate))
            {
                frontier.push(candidate.clone());
                if frontier.len() == MAX_MULTI_SCALE_CANDIDATE_PATHS {
                    break;
                }
            }
        }
    }
    frontier
}

fn discover_paths(
    ctx: &BellmanFordContext,
    total: &BigUint,
    max_hops: usize,
    use_multi_scale_frontier: bool,
    include_v4: bool,
) -> Result<Vec<PathAllocation>, AlgorithmError> {
    let collect_census = census_enabled() ||
        symbolic_cut_census_enabled() ||
        concavity_audit_enabled() ||
        certified_interval_census_enabled();
    let mut search = PathSearch {
        ctx,
        total,
        max_hops,
        nodes: FxHashSet::from_iter([ctx.token_in_node]),
        components: FxHashSet::default(),
        descriptors: Vec::new(),
        paths: Vec::new(),
        census_paths: Vec::new(),
        collect_census,
        include_v4,
    };
    search.visit(ctx.token_in_node)?;

    if census_enabled() {
        emit_language_census(&search.census_paths, total, ctx);
    }
    if symbolic_cut_census_enabled() {
        emit_symbolic_allocation_cut_census(&search.census_paths, total, ctx);
    }
    if concavity_audit_enabled() {
        super::concavity_audit::emit(&search.census_paths, total, ctx);
    }

    if certified_interval_census_enabled() {
        emit_v2_certified_structural_interval_census(&search.census_paths, total, ctx);
    }

    search
        .paths
        .sort_unstable_by(|a, b| b.amount_out.cmp(&a.amount_out));

    // The global full-order Top-K can be entirely V3 even when competitive V2
    // pairs exist structurally. When the certified V2 allocator is enabled,
    // preserve a tiny protocol-specific V2 frontier through truncation so the
    // normal portfolio enumerator can actually exercise the 2-path allocator.
    // This adds at most two paths, keeping combinatorics bounded.
    let certified_v2_frontier = std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR").is_some();
    let v2_extras = if certified_v2_frontier {
        let v2_indices = search
            .paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| pure_v2_allocation(path, ctx).then_some(index))
            .collect::<Vec<_>>();
        let mut best_pair: Option<(usize, usize, BigUint)> = None;
        for (position, &left_index) in v2_indices.iter().enumerate() {
            for &right_index in &v2_indices[position + 1..] {
                let left = &search.paths[left_index];
                let right = &search.paths[right_index];
                if !allocation_paths_pool_disjoint(left, right) {
                    continue;
                }
                let combined = &left.amount_out + &right.amount_out;
                if best_pair
                    .as_ref()
                    .is_none_or(|(_, _, best)| combined > *best)
                {
                    best_pair = Some((left_index, right_index, combined));
                }
            }
        }
        best_pair
            .map(|(left, right, _)| vec![search.paths[left].clone(), search.paths[right].clone()])
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    if use_multi_scale_frontier {
        let full_order_paths = search.paths.len();
        search.paths = multi_scale_frontier(&search.paths, total, ctx);
        if std::env::var_os("FYND_MULTI_SCALE_FRONTIER_TRACE").is_some() {
            eprintln!(
                "multi-scale-frontier: full_order_paths={} retained={} extras={}",
                full_order_paths,
                search.paths.len(),
                search
                    .paths
                    .len()
                    .saturating_sub(MAX_CANDIDATE_PATHS)
            );
        }
    } else {
        if include_v4 {
            let mut established = search
                .paths
                .iter()
                .filter(|path| !contains_v4(path, ctx))
                .take(MAX_CANDIDATE_PATHS)
                .cloned()
                .collect::<Vec<_>>();
            established.extend(
                search
                    .paths
                    .iter()
                    .filter(|path| contains_v4(path, ctx))
                    .take(MAX_V4_CANDIDATE_PATHS)
                    .cloned(),
            );
            search.paths = established;
        } else {
            search
                .paths
                .truncate(MAX_CANDIDATE_PATHS);
        }
    }

    if certified_v2_frontier {
        let before = search.paths.len();
        let had_compatible_v2_pair = !v2_extras.is_empty();
        for candidate in v2_extras {
            if !search
                .paths
                .iter()
                .any(|path| same_allocation_path(path, &candidate))
            {
                search.paths.push(candidate);
            }
        }
        if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
            eprintln!(
                "v2-certified-frontier: global={} total={} added={} compatible_pair={}",
                before,
                search.paths.len(),
                search
                    .paths
                    .len()
                    .saturating_sub(before),
                usize::from(had_compatible_v2_pair)
            );
        }
    }

    Ok(search.paths)
}

pub(super) fn search_disjoint_portfolios(
    ctx: &BellmanFordContext,
    order: &Order,
    total: &BigUint,
    max_hops: usize,
    max_paths: usize,
    use_multi_scale_frontier: bool,
    include_v4: bool,
    quotient_v4_resources: bool,
    ordered_v4_histories: bool,
    v4_upper_bound_pruning: bool,
    certified_pair_face_closure: bool,
    certified_v4_subset_fallback: bool,
    cutoff_centi_bps: u32,
    minimum_net_output: &BigInt,
) -> Result<Vec<Vec<PathAllocation>>, AlgorithmError> {
    let paths = discover_paths(ctx, total, max_hops, use_multi_scale_frontier, include_v4)?;
    let mut portfolios = Vec::new();
    let sacred_timeline_enabled = sacred_timeline_census_enabled();
    let mut sacred_timeline = SacredTimelineStats::new(minimum_net_output);
    let mut sacred_timeline_actual_incumbent = minimum_net_output.clone();
    if include_v4 &&
        paths
            .iter()
            .any(|path| contains_v4(path, ctx))
    {
        let gas_cost_per_unit = paths
            .first()
            .and_then(|path| path.hops.last())
            .map(|hop| {
                PathFrankWolfeAlgorithm::gas_units_to_output_tokens(
                    &BigUint::from(1u8),
                    &hop.descriptor.token_out.address,
                    ctx,
                )
            })
            .unwrap_or_default();
        let exact_gas_valuation = paths
            .first()
            .and_then(|path| path.hops.last())
            .and_then(|hop| {
                PathFrankWolfeAlgorithm::exact_gas_valuation(&hop.descriptor.token_out.address, ctx)
            });
        // Pool conflicts are future-relevant state: paths from different
        // compatible components cannot be represented by one allocation vector.
        // Within a maximal pool-disjoint portfolio, however, every subset is
        // represented by assigning zero flow to its unused paths. Refine only
        // these maximal resource frontiers instead of every V4-containing subset.
        //
        // Theorem 2 boundary:
        // - assumption 3 is established by `v4_compatible_portfolios` component disjointness;
        // - assumption 5 is established because discovery, refinement, and final scoring all use
        //   this request's same `ctx.market_data` snapshot;
        // - assumptions 1 and 4 are NOT established for the general call to
        //   `refine_disjoint_allocations`: its multi-path fallback is coordinate ascent and the
        //   path-removal loop below is heuristic. Results from that path must not be described as
        //   the globally optimal maximal-frontier solution.
        let mut v4_incumbent = minimum_net_output.clone();
        let envelopes = (certified_v4_subset_fallback || sacred_timeline_enabled)
            .then(|| monotone_output_envelopes(&paths, total, ctx));
        let subset_width = (!quotient_v4_resources).then_some(max_paths);
        let compatible = v4_compatible_portfolios(&paths, ctx, subset_width);
        let compatible =
            if ordered_v4_histories { ordered_portfolio_histories(compatible) } else { compatible };
        for mut active in compatible {
            let frontier = certified_pair_face_closure.then(|| active.clone());
            let audit_frontier = allocation_support_audit_enabled().then(|| active.clone());
            let mut cutoff_decisions = [true; 5];
            let finite_input_upper = envelopes
                .as_ref()
                .and_then(|envelopes| monotone_grid_upper_bound(&active, &paths, envelopes))
                .map(BigInt::from);
            let finite_input_would_prune = finite_input_upper
                .as_ref()
                .is_some_and(|upper| upper <= &v4_incumbent);
            if sacred_timeline_enabled && quotient_v4_resources {
                sacred_timeline.record_v4_frontier(active.len());
            }
            if sacred_timeline_enabled && finite_input_upper.is_some() {
                sacred_timeline.v4_finite_input_supported = sacred_timeline
                    .v4_finite_input_supported
                    .saturating_add(1);
                if finite_input_would_prune {
                    sacred_timeline.v4_finite_input_would_prune = sacred_timeline
                        .v4_finite_input_would_prune
                        .saturating_add(1);
                }
            }
            match monotone_full_input_upper_bound(&active, ctx) {
                Some(upper) => {
                    if sacred_timeline_enabled {
                        sacred_timeline.v4_bound_supported = sacred_timeline
                            .v4_bound_supported
                            .saturating_add(1);
                        cutoff_decisions = sacred_timeline.cutoff_decisions(
                            finite_input_upper
                                .as_ref()
                                .unwrap_or(&upper),
                        );
                    }
                    if v4_upper_bound_pruning && upper <= v4_incumbent {
                        if sacred_timeline_enabled {
                            sacred_timeline.v4_bound_pruned = sacred_timeline
                                .v4_bound_pruned
                                .saturating_add(1);
                        }
                        continue;
                    }
                }
                None => {
                    if sacred_timeline_enabled {
                        sacred_timeline.v4_bound_unsupported = sacred_timeline
                            .v4_bound_unsupported
                            .saturating_add(1);
                    }
                }
            }
            loop {
                let started = Instant::now();
                let refined = refine_disjoint_allocations(
                    &active,
                    total,
                    &ctx.market_data,
                    gas_cost_per_unit,
                    exact_gas_valuation.as_ref(),
                    cutoff_centi_bps,
                );
                if sacred_timeline_enabled {
                    let elapsed_us = started.elapsed().as_micros();
                    sacred_timeline.record_cutoff_refinement(cutoff_decisions, elapsed_us);
                    if finite_input_would_prune {
                        sacred_timeline.v4_finite_input_time_avoided_us = sacred_timeline
                            .v4_finite_input_time_avoided_us
                            .saturating_add(elapsed_us);
                    }
                    sacred_timeline.v4_refinement_attempts = sacred_timeline
                        .v4_refinement_attempts
                        .saturating_add(1);
                    sacred_timeline.v4_refinement_time_us = sacred_timeline
                        .v4_refinement_time_us
                        .saturating_add(elapsed_us);
                }
                let Ok(Some(mut allocation)) = refined else { break };
                if sacred_timeline_enabled {
                    sacred_timeline.v4_refinement_successes = sacred_timeline
                        .v4_refinement_successes
                        .saturating_add(1);
                }
                // Theorem 2, assumption 4 requires the optimizer itself to find the global optimum
                // subject to `support <= max_paths`. This check enforces the output-width
                // invariant, but it does not establish that premise because the
                // preceding optimizer can be heuristic. The removal loop is a
                // production fallback, not part of the proof.
                if allocation.len() <= max_paths {
                    if let Some(frontier) = &audit_frontier {
                        emit_allocation_support_audit(
                            frontier,
                            &allocation,
                            total,
                            order,
                            ctx,
                            exact_gas_valuation.as_ref(),
                        )?;
                    }
                    if let Some(net) = allocation_net_output(&allocation, order, ctx) {
                        if sacred_timeline_enabled {
                            sacred_timeline.record_cutoff_result(cutoff_decisions, &net);
                            if finite_input_would_prune && net > v4_incumbent {
                                sacred_timeline.v4_finite_input_violations = sacred_timeline
                                    .v4_finite_input_violations
                                    .saturating_add(1);
                            }
                            if net > v4_incumbent {
                                sacred_timeline.v4_incumbent_improvements = sacred_timeline
                                    .v4_incumbent_improvements
                                    .saturating_add(1);
                            } else {
                                sacred_timeline.v4_losing_frontiers = sacred_timeline
                                    .v4_losing_frontiers
                                    .saturating_add(1);
                            }
                        }
                        v4_incumbent = v4_incumbent.max(net);
                    } else if sacred_timeline_enabled {
                        sacred_timeline.v4_losing_frontiers = sacred_timeline
                            .v4_losing_frontiers
                            .saturating_add(1);
                    }
                    portfolios.push(allocation);
                    break;
                }

                allocation.sort_unstable_by(|left, right| {
                    left.amount_out
                        .cmp(&right.amount_out)
                        .then_with(|| left.hops.len().cmp(&right.hops.len()))
                });
                let remove = &allocation[0];
                active.retain(|path| !same_allocation_path(path, remove));
                if active.len() < 2 {
                    break;
                }
            }

            if let Some(frontier) = frontier {
                // A maximal frontier represents all of its zero-flow faces, but the general
                // allocator is heuristic for support greater than two. Enumerate the small faces
                // whose integer optimum the certified pair solver can actually prove. This makes
                // the returned candidate set globally complete for support <= 2 without claiming
                // anything about larger supports.
                portfolios.extend(
                    frontier
                        .iter()
                        .cloned()
                        .map(|path| vec![path]),
                );
                for left in 0..frontier.len() {
                    for right in left + 1..frontier.len() {
                        let pair = [frontier[left].clone(), frontier[right].clone()];
                        if monotone_full_input_upper_bound(&pair, ctx)
                            .is_some_and(|upper| upper <= v4_incumbent)
                        {
                            continue;
                        }
                        if let CertifiedPairSolve::Complete(allocation) = certified_pair_allocation(
                            &pair,
                            total,
                            &ctx.market_data,
                            exact_gas_valuation.as_ref(),
                            cutoff_centi_bps,
                        )? {
                            if let Some(net) = allocation_net_output(&allocation, order, ctx) {
                                v4_incumbent = v4_incumbent.max(net);
                            }
                            portfolios.push(allocation);
                        }
                    }
                }
            }
        }

        if quotient_v4_resources && certified_v4_subset_fallback {
            for active in v4_compatible_portfolios(&paths, ctx, Some(max_paths)) {
                let upper = envelopes
                    .as_ref()
                    .and_then(|envelopes| monotone_grid_upper_bound(&active, &paths, envelopes));
                let cutoff_decisions = if sacred_timeline_enabled {
                    upper
                        .as_ref()
                        .map_or([true; 5], |upper| {
                            sacred_timeline.cutoff_decisions(&BigInt::from(upper.clone()))
                        })
                } else {
                    [true; 5]
                };
                if upper.is_some_and(|upper| BigInt::from(upper) <= v4_incumbent) {
                    continue;
                }
                let started = Instant::now();
                let refined = refine_disjoint_allocations(
                    &active,
                    total,
                    &ctx.market_data,
                    gas_cost_per_unit,
                    exact_gas_valuation.as_ref(),
                    cutoff_centi_bps,
                );
                if sacred_timeline_enabled {
                    let elapsed_us = started.elapsed().as_micros();
                    sacred_timeline.record_cutoff_refinement(cutoff_decisions, elapsed_us);
                    sacred_timeline.v4_refinement_attempts = sacred_timeline
                        .v4_refinement_attempts
                        .saturating_add(1);
                    sacred_timeline.v4_refinement_time_us = sacred_timeline
                        .v4_refinement_time_us
                        .saturating_add(elapsed_us);
                }
                if let Ok(Some(allocation)) = refined {
                    if sacred_timeline_enabled {
                        sacred_timeline.v4_refinement_successes = sacred_timeline
                            .v4_refinement_successes
                            .saturating_add(1);
                    }
                    if let Some(net) = allocation_net_output(&allocation, order, ctx) {
                        if sacred_timeline_enabled {
                            sacred_timeline.record_cutoff_result(cutoff_decisions, &net);
                            if net > v4_incumbent {
                                sacred_timeline.v4_incumbent_improvements = sacred_timeline
                                    .v4_incumbent_improvements
                                    .saturating_add(1);
                            } else {
                                sacred_timeline.v4_losing_frontiers = sacred_timeline
                                    .v4_losing_frontiers
                                    .saturating_add(1);
                            }
                        }
                        v4_incumbent = v4_incumbent.max(net);
                    } else if sacred_timeline_enabled {
                        sacred_timeline.v4_losing_frontiers = sacred_timeline
                            .v4_losing_frontiers
                            .saturating_add(1);
                    }
                    portfolios.push(allocation);
                }
            }
        }
        sacred_timeline_actual_incumbent = v4_incumbent;
    }

    let exhaustive_paths = if include_v4 {
        paths
            .iter()
            .filter(|path| !contains_v4(path, ctx))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        paths
    };
    let mut selected = Vec::new();
    let mut used = FxHashSet::default();
    let mut canonical_stats = CanonicalPortfolioStats::default();
    combinations(
        &exhaustive_paths,
        0,
        max_paths,
        total,
        ctx,
        &mut selected,
        &mut used,
        &mut portfolios,
        &mut canonical_stats,
        &mut sacred_timeline,
        sacred_timeline_enabled,
        cutoff_centi_bps,
        minimum_net_output,
    )?;
    if canonical_portfolio_census_enabled() {
        canonical_stats.emit();
    }
    if sacred_timeline_enabled {
        sacred_timeline.emit(&sacred_timeline_actual_incumbent);
    }
    Ok(portfolios)
}

fn combinations(
    paths: &[PathAllocation],
    start: usize,
    max_paths: usize,
    total: &BigUint,
    ctx: &BellmanFordContext,
    selected: &mut Vec<PathAllocation>,
    used: &mut FxHashSet<String>,
    portfolios: &mut Vec<Vec<PathAllocation>>,
    canonical_stats: &mut CanonicalPortfolioStats,
    sacred_timeline: &mut SacredTimelineStats,
    sacred_timeline_enabled: bool,
    cutoff_centi_bps: u32,
    minimum_net_output: &BigInt,
) -> Result<(), AlgorithmError> {
    if sacred_timeline_enabled {
        sacred_timeline.prefix_states = sacred_timeline
            .prefix_states
            .saturating_add(1);
    }
    // A newly discovered path is a valid competitor to the native single-path
    // incumbent even when no split is useful. Previously the exact-search layer
    // only emitted portfolios with at least two paths, so an independently
    // discovered direct V3 route could never replace a weaker V2 incumbent by
    // itself. Keep singleton paths in the same candidate stream; the caller's
    // existing post-gas acceptance remains authoritative.
    let gross_upper = selected
        .iter()
        .fold(BigUint::from(0u8), |sum, path| sum + &path.amount_out);
    let can_beat_incumbent = BigInt::from(gross_upper) > *minimum_net_output;
    if sacred_timeline_enabled && !selected.is_empty() {
        sacred_timeline.record_complete(selected.len(), can_beat_incumbent);
    }
    if selected.len() == 1 && can_beat_incumbent {
        canonical_stats.record(selected.len());
        portfolios.push(selected.clone());
    } else if selected.len() >= 2 && can_beat_incumbent {
        if sacred_timeline_enabled {
            sacred_timeline.refinement_attempts = sacred_timeline
                .refinement_attempts
                .saturating_add(1);
        }
        let gas_cost_per_unit = selected
            .first()
            .and_then(|path| path.hops.last())
            .map(|hop| {
                PathFrankWolfeAlgorithm::gas_units_to_output_tokens(
                    &BigUint::from(1u8),
                    &hop.descriptor.token_out.address,
                    ctx,
                )
            })
            .unwrap_or_default();
        let exact_gas_valuation = selected
            .first()
            .and_then(|path| path.hops.last())
            .and_then(|hop| {
                PathFrankWolfeAlgorithm::exact_gas_valuation(&hop.descriptor.token_out.address, ctx)
            });
        if let Ok(Some(allocation)) = refine_disjoint_allocations(
            selected,
            total,
            &ctx.market_data,
            gas_cost_per_unit,
            exact_gas_valuation.as_ref(),
            cutoff_centi_bps,
        ) {
            if sacred_timeline_enabled {
                sacred_timeline.refinement_successes = sacred_timeline
                    .refinement_successes
                    .saturating_add(1);
            }
            canonical_stats.record(selected.len());
            portfolios.push(allocation);
        } else if sacred_timeline_enabled {
            sacred_timeline.refinement_failures = sacred_timeline
                .refinement_failures
                .saturating_add(1);
        }
    }
    if selected.len() == max_paths {
        return Ok(());
    }
    for index in start..paths.len() {
        if sacred_timeline_enabled {
            sacred_timeline.extension_attempts = sacred_timeline
                .extension_attempts
                .saturating_add(1);
        }
        if paths[index]
            .hops
            .iter()
            .any(|hop| used.contains(&hop.descriptor.component_id))
        {
            if sacred_timeline_enabled {
                sacred_timeline.resource_conflicts = sacred_timeline
                    .resource_conflicts
                    .saturating_add(1);
            }
            continue;
        }
        for hop in &paths[index].hops {
            used.insert(hop.descriptor.component_id.clone());
        }
        selected.push(paths[index].clone());
        combinations(
            paths,
            index + 1,
            max_paths,
            total,
            ctx,
            selected,
            used,
            portfolios,
            canonical_stats,
            sacred_timeline,
            sacred_timeline_enabled,
            cutoff_centi_bps,
            minimum_net_output,
        )?;
        let path = selected.pop().unwrap();
        for hop in path.hops {
            used.remove(&hop.descriptor.component_id);
        }
    }
    Ok(())
}
