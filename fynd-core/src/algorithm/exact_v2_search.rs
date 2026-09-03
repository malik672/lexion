//! Independent pool-disjoint path discovery for the exact replay refinement layer.

use num_bigint::BigUint;
use petgraph::graph::NodeIndex;
use rustc_hash::{FxHashMap, FxHashSet};
use tycho_simulation::evm::protocol::{
    uniswap_v2::state::UniswapV2State, uniswap_v3::state::UniswapV3State,
};

use super::{
    bellman_ford::BellmanFordContext,
    exact_v2_refiner::refine_disjoint_allocations,
    split_primitives::{
        simulate_path, HopDescriptor, MarketOverrides, PathAllocation, SimulatedHop,
    },
    AlgorithmError,
};

/// A bounded V2/V3 frontier. The exact replay allocator evaluates path
/// portfolios, so raising this has combinatorial cost; six candidates retain
/// the strongest alternatives while keeping user-order latency bounded.
const MAX_CANDIDATE_PATHS: usize = 6;

/// Analysis-only probe scales for the routing-language census. These deliberately
/// include the 25% and 10% regimes seen in live Uniswap counterexamples.
const CENSUS_SCALES: &[(u64, u64)] = &[(1, 1), (1, 2), (1, 4), (1, 10), (1, 20)];

fn census_enabled() -> bool {
    std::env::var_os("FYND_ROUTING_LANGUAGE_CENSUS").is_some()
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
    /// populated only when the analysis census is enabled and never affects the
    /// production candidate frontier.
    census_paths: Vec<Vec<HopDescriptor>>,
    collect_census: bool,
}

impl PathSearch<'_> {
    fn visit(&mut self, node: NodeIndex) -> Result<(), AlgorithmError> {
        if node == self.ctx.token_out_node && !self.descriptors.is_empty() {
            if self.collect_census {
                self.census_paths.push(self.descriptors.clone());
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
            let Some(state) = self.ctx.market_data.get_simulation_state(component_id) else {
                continue;
            };
            if state.as_any().downcast_ref::<UniswapV2State>().is_none() &&
                state.as_any().downcast_ref::<UniswapV3State>().is_none()
            {
                continue;
            }
            let (Some(token_in), Some(token_out)) =
                (self.ctx.token_map.get(&node), self.ctx.token_map.get(next))
            else {
                continue;
            };
            self.nodes.insert(*next);
            self.components.insert(component_id.clone());
            self.descriptors.push(HopDescriptor::new(
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
            let Some(state) = ctx.market_data.get_simulation_state(&hop.component_id) else {
                return '?';
            };
            if state.as_any().downcast_ref::<UniswapV2State>().is_some() {
                '2'
            } else if state.as_any().downcast_ref::<UniswapV3State>().is_some() {
                '3'
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
                values[scale_index].as_ref().map(|value| (index, value))
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

    let survivors = survivor.iter().filter(|&&value| value).count();
    let rescued = survivor
        .iter()
        .enumerate()
        .filter(|(index, value)| **value && !full_topk.contains(index))
        .count();
    let full_infeasible_but_smaller_feasible = outputs
        .iter()
        .filter(|values| values[0].is_none() && values.iter().skip(1).any(Option::is_some))
        .count();

    let mut signatures: FxHashMap<String, SignatureCounts> = FxHashMap::default();
    for (index, path) in structural_paths.iter().enumerate() {
        let entry = signatures.entry(protocol_word(path, ctx)).or_default();
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
    eprintln!(
        "full-infeasible / smaller-feasible: {}",
        full_infeasible_but_smaller_feasible
    );
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

fn discover_paths(
    ctx: &BellmanFordContext,
    total: &BigUint,
    max_hops: usize,
) -> Result<Vec<PathAllocation>, AlgorithmError> {
    let collect_census = census_enabled();
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
    };
    search.visit(ctx.token_in_node)?;

    if collect_census {
        emit_language_census(&search.census_paths, total, ctx);
    }

    search.paths.sort_unstable_by(|a, b| b.amount_out.cmp(&a.amount_out));
    search.paths.truncate(MAX_CANDIDATE_PATHS);
    Ok(search.paths)
}

pub(super) fn search_disjoint_portfolios(
    ctx: &BellmanFordContext,
    total: &BigUint,
    max_hops: usize,
    max_paths: usize,
) -> Result<Vec<Vec<PathAllocation>>, AlgorithmError> {
    let paths = discover_paths(ctx, total, max_hops)?;
    let mut portfolios = Vec::new();
    let mut selected = Vec::new();
    let mut used = FxHashSet::default();
    combinations(&paths, 0, max_paths, total, ctx, &mut selected, &mut used, &mut portfolios)?;
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
) -> Result<(), AlgorithmError> {
    // A newly discovered path is a valid competitor to the native single-path
    // incumbent even when no split is useful. Previously the exact-search layer
    // only emitted portfolios with at least two paths, so an independently
    // discovered direct V3 route could never replace a weaker V2 incumbent by
    // itself. Keep singleton paths in the same candidate stream; the caller's
    // existing post-gas acceptance remains authoritative.
    if selected.len() == 1 {
        portfolios.push(selected.clone());
    } else if selected.len() >= 2 {
        if let Ok(Some(allocation)) = refine_disjoint_allocations(selected, total, &ctx.market_data)
        {
            portfolios.push(allocation);
        }
    }
    if selected.len() == max_paths {
        return Ok(());
    }
    for index in start..paths.len() {
        if paths[index]
            .hops
            .iter()
            .any(|hop| used.contains(&hop.descriptor.component_id))
        {
            continue;
        }
        for hop in &paths[index].hops {
            used.insert(hop.descriptor.component_id.clone());
        }
        selected.push(paths[index].clone());
        combinations(paths, index + 1, max_paths, total, ctx, selected, used, portfolios)?;
        let path = selected.pop().unwrap();
        for hop in path.hops {
            used.remove(&hop.descriptor.component_id);
        }
    }
    Ok(())
}
