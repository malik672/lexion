//! Analysis-only exact audit of pairwise split concavity.

use num_bigint::{BigInt, BigUint};
use rustc_hash::{FxHashMap, FxHashSet};

use super::{
    bellman_ford::BellmanFordContext,
    split_primitives::{simulate_path, HopDescriptor, MarketOverrides},
};
use tycho_simulation::evm::protocol::{
    uniswap_v2::state::UniswapV2State, uniswap_v3::state::UniswapV3State,
};

const GRID_CELLS: u64 = 64;

struct ExactPathCache<'a> {
    paths: &'a [Vec<HopDescriptor>],
    ctx: &'a BellmanFordContext,
    values: Vec<FxHashMap<BigUint, Option<BigUint>>>,
    simulations: usize,
}

impl<'a> ExactPathCache<'a> {
    fn new(paths: &'a [Vec<HopDescriptor>], ctx: &'a BellmanFordContext) -> Self {
        Self {
            paths,
            ctx,
            values: vec![FxHashMap::default(); paths.len()],
            simulations: 0,
        }
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

fn pair_output(
    cache: &mut ExactPathCache<'_>,
    left: usize,
    right: usize,
    total: &BigUint,
    x: &BigUint,
) -> Option<BigUint> {
    if x > total {
        return None;
    }
    let left_amount = total - x;
    Some(cache.output(left, &left_amount)? + cache.output(right, x)?)
}

fn pool_disjoint(left: &[HopDescriptor], right: &[HopDescriptor]) -> bool {
    let used = left
        .iter()
        .map(|hop| hop.component_id.as_str())
        .collect::<FxHashSet<_>>();
    right
        .iter()
        .all(|hop| !used.contains(hop.component_id.as_str()))
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

#[derive(Default)]
struct ClassStats {
    pairs: usize,
    complete: usize,
    concave: usize,
    violating: usize,
    incomplete: usize,
    triples: usize,
    bad_triples: usize,
}

struct Violation {
    left: usize,
    right: usize,
    class: String,
    x0: BigUint,
    x1: BigUint,
    x2: BigUint,
    y0: BigUint,
    y1: BigUint,
    y2: BigUint,
}

/// Exact-grid differential for the coupled objective
///
///     F(x) = Q_left(T - x) + Q_right(x).
///
/// Concavity requires adjacent secant slopes to be non-increasing. Because
/// integer division can make neighboring x steps differ by one unit, slopes are
/// compared by exact signed cross multiplication rather than floating point.
/// Missing simulator samples make a pair incomplete. This function is
/// instrumentation only: no result is used for pruning or route selection.
pub(super) fn emit(
    paths: &[Vec<HopDescriptor>],
    total: &BigUint,
    ctx: &BellmanFordContext,
) {
    if paths.len() < 2 || total == &BigUint::from(0u8) {
        return;
    }

    let mut cache = ExactPathCache::new(paths, ctx);
    let mut classes: FxHashMap<String, ClassStats> = FxHashMap::default();
    let mut first_violations = Vec::new();
    let mut pairs = 0usize;
    let mut complete = 0usize;
    let mut concave = 0usize;
    let mut violating = 0usize;
    let mut incomplete = 0usize;
    let mut triples = 0usize;
    let mut bad_triples = 0usize;

    for left in 0..paths.len() {
        for right in (left + 1)..paths.len() {
            if !pool_disjoint(&paths[left], &paths[right]) {
                continue;
            }
            pairs += 1;
            let class = format!("{}+{}", protocol_word(&paths[left], ctx), protocol_word(&paths[right], ctx));
            classes.entry(class.clone()).or_default().pairs += 1;

            let mut points = Vec::with_capacity((GRID_CELLS + 1) as usize);
            let mut pair_complete = true;
            for i in 0..=GRID_CELLS {
                let x = (total * BigUint::from(i)) / BigUint::from(GRID_CELLS);
                if points.last().is_some_and(|(prev, _): &(BigUint, BigUint)| prev == &x) {
                    continue;
                }
                let Some(y) = pair_output(&mut cache, left, right, total, &x) else {
                    pair_complete = false;
                    break;
                };
                points.push((x, y));
            }

            let stats = classes.get_mut(&class).expect("class inserted above");
            if !pair_complete || points.len() < 3 {
                incomplete += 1;
                stats.incomplete += 1;
                continue;
            }
            complete += 1;
            stats.complete += 1;

            let mut pair_violates = false;
            for w in points.windows(3) {
                let (x0, y0) = (&w[0].0, &w[0].1);
                let (x1, y1) = (&w[1].0, &w[1].1);
                let (x2, y2) = (&w[2].0, &w[2].1);
                let dx1 = BigInt::from(x1 - x0);
                let dx2 = BigInt::from(x2 - x1);
                if dx1 == BigInt::from(0u8) || dx2 == BigInt::from(0u8) {
                    continue;
                }
                let dy1 = BigInt::from(y1.clone()) - BigInt::from(y0.clone());
                let dy2 = BigInt::from(y2.clone()) - BigInt::from(y1.clone());
                triples += 1;
                stats.triples += 1;

                if &dy2 * &dx1 > &dy1 * &dx2 {
                    bad_triples += 1;
                    stats.bad_triples += 1;
                    pair_violates = true;
                    if first_violations.len() < 12 {
                        first_violations.push(Violation {
                            left,
                            right,
                            class: class.clone(),
                            x0: x0.clone(),
                            x1: x1.clone(),
                            x2: x2.clone(),
                            y0: y0.clone(),
                            y1: y1.clone(),
                            y2: y2.clone(),
                        });
                    }
                }
            }

            if pair_violates {
                violating += 1;
                stats.violating += 1;
            } else {
                concave += 1;
                stats.concave += 1;
            }
        }
    }

    eprintln!("\n=== CoupledConcavityAuditV1 ===");
    eprintln!("structural paths:              {}", paths.len());
    eprintln!("pool-disjoint pairs:           {}", pairs);
    eprintln!("complete pairs:                {}", complete);
    eprintln!("empirically concave pairs:     {}", concave);
    eprintln!("pairs with violation:          {}", violating);
    eprintln!("incomplete pairs:              {}", incomplete);
    eprintln!("checked slope triples:         {}", triples);
    eprintln!("violating slope triples:       {}", bad_triples);
    eprintln!("exact simulator calls:         {}", cache.simulations);
    eprintln!("grid cells:                    {}", GRID_CELLS);

    let mut rows = classes.into_iter().collect::<Vec<_>>();
    rows.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    if !rows.is_empty() {
        eprintln!("protocol classes:");
        for (class, s) in rows {
            eprintln!(
                "  {:<7} pairs={} complete={} concave={} violating={} incomplete={} triples={} bad_triples={}",
                class, s.pairs, s.complete, s.concave, s.violating, s.incomplete, s.triples, s.bad_triples
            );
        }
    }

    if !first_violations.is_empty() {
        eprintln!("first concavity violations (up to 12):");
        for v in first_violations {
            let left = paths[v.left]
                .iter()
                .map(|hop| hop.component_id.as_str())
                .collect::<Vec<_>>()
                .join(" -> ");
            let right = paths[v.right]
                .iter()
                .map(|hop| hop.component_id.as_str())
                .collect::<Vec<_>>()
                .join(" -> ");
            eprintln!("  class={} left={} right={}", v.class, left, right);
            eprintln!(
                "    x=[{}, {}, {}] F=[{}, {}, {}]",
                v.x0, v.x1, v.x2, v.y0, v.y1, v.y2
            );
        }
    }
    eprintln!("=== end CoupledConcavityAuditV1 ===\n");
}
