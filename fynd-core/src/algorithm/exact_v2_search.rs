//! Independent pool-disjoint path discovery for the exact replay refinement layer.

use num_bigint::BigUint;
use petgraph::graph::NodeIndex;
use rustc_hash::FxHashSet;
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

struct PathSearch<'a> {
    ctx: &'a BellmanFordContext,
    total: &'a BigUint,
    max_hops: usize,
    nodes: FxHashSet<NodeIndex>,
    components: FxHashSet<String>,
    descriptors: Vec<HopDescriptor>,
    paths: Vec<PathAllocation>,
}

impl PathSearch<'_> {
    fn visit(&mut self, node: NodeIndex) -> Result<(), AlgorithmError> {
        if node == self.ctx.token_out_node && !self.descriptors.is_empty() {
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
                    .is_none()
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

fn discover_paths(
    ctx: &BellmanFordContext,
    total: &BigUint,
    max_hops: usize,
) -> Result<Vec<PathAllocation>, AlgorithmError> {
    let mut search = PathSearch {
        ctx,
        total,
        max_hops,
        nodes: FxHashSet::from_iter([ctx.token_in_node]),
        components: FxHashSet::default(),
        descriptors: Vec::new(),
        paths: Vec::new(),
    };
    search.visit(ctx.token_in_node)?;
    search
        .paths
        .sort_unstable_by(|a, b| b.amount_out.cmp(&a.amount_out));
    search
        .paths
        .truncate(MAX_CANDIDATE_PATHS);
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
    if selected.len() >= 2 {
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
