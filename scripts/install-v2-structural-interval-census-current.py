#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REFINER = ROOT / "fynd-core/src/algorithm/exact_v2_refiner.rs"
SEARCH = ROOT / "fynd-core/src/algorithm/exact_v2_search.rs"


def function_span(text: str, signature: str) -> tuple[int, int]:
    start = text.find(signature)
    if start < 0:
        raise SystemExit(f"could not locate function signature: {signature}")
    brace = text.find("{", start)
    if brace < 0:
        raise SystemExit(f"could not locate opening brace: {signature}")
    depth = 0
    for i in range(brace, len(text)):
        ch = text[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                end = i + 1
                if end < len(text) and text[end] == "\n":
                    end += 1
                return start, end
    raise SystemExit(f"unterminated function: {signature}")


def insert_before_once(text: str, marker: str, insertion: str, already: str) -> str:
    if already in text:
        return text
    pos = text.find(marker)
    if pos < 0:
        raise SystemExit(f"could not locate insertion marker: {marker}")
    return text[:pos] + insertion + text[pos:]


def patch_refiner() -> None:
    text = REFINER.read_text()

    if "fn descriptor_path_curve(" not in text:
        start, end = function_span(
            text,
            "fn path_curve(path: &PathAllocation, market: &MarketState) -> Option<ContinuousCpmm>",
        )
        replacement = '''fn descriptor_path_curve(
    path: &[HopDescriptor],
    market: &MarketState,
) -> Option<ContinuousCpmm> {
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
'''
        text = text[:start] + replacement + text[end:]

    helper = '''pub(super) fn certified_v2_interval_upper_for_paths(
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

'''
    text = insert_before_once(
        text,
        "fn certified_v2_pair_upper(",
        helper,
        "pub(super) fn certified_v2_interval_upper_for_paths(",
    )
    REFINER.write_text(text)


def patch_search() -> None:
    text = SEARCH.read_text()

    if "certified_v2_interval_upper_for_paths" not in text:
        old = "    exact_v2_refiner::refine_disjoint_allocations,"
        new = "    exact_v2_refiner::{certified_v2_interval_upper_for_paths, refine_disjoint_allocations},"
        if old not in text:
            raise SystemExit("could not locate exact_v2_refiner import")
        text = text.replace(old, new, 1)

    if "fn certified_interval_census_enabled()" not in text:
        _, end = function_span(text, "fn concavity_audit_enabled() -> bool")
        addition = '''
fn certified_interval_census_enabled() -> bool {
    std::env::var_os("FYND_V2_CERTIFIED_INTERVAL_CENSUS").is_some()
}
'''
        text = text[:end] + addition + text[end:]

    if "struct CertifiedStructuralIntervalStats" not in text:
        census_code = r'''const CERTIFIED_STRUCTURAL_INTERVAL_DEPTH: usize = 6;

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
    !path.is_empty() && path.iter().all(|hop| {
        ctx.market_data
            .get_simulation_state(&hop.component_id)
            .is_some_and(|state| state.as_any().downcast_ref::<UniswapV2State>().is_some())
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
            if !is_v2_path(&structural_paths[right], ctx)
                || !paths_are_pool_disjoint(&structural_paths[left], &structural_paths[right])
            {
                continue;
            }
            let (Some(left_full), Some(right_full)) =
                (&full_outputs[left], &full_outputs[right])
            else {
                continue;
            };
            stats.pairs += 1;
            stats.total_width += total;
            let incumbent = left_full.clone().max(right_full.clone());
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

'''
        pos = text.find("fn discover_paths(")
        if pos < 0:
            raise SystemExit("could not locate discover_paths")
        text = text[:pos] + census_code + text[pos:]

    if "|| certified_interval_census_enabled();" not in text:
        old = '''    let collect_census =
        census_enabled() || symbolic_cut_census_enabled() || concavity_audit_enabled();'''
        new = '''    let collect_census = census_enabled()
        || symbolic_cut_census_enabled()
        || concavity_audit_enabled()
        || certified_interval_census_enabled();'''
        if old not in text:
            raise SystemExit("could not locate collect_census expression")
        text = text.replace(old, new, 1)

    if "emit_v2_certified_structural_interval_census(&search.census_paths, total, ctx);" not in text:
        marker = "    search.paths.sort_unstable_by(|a, b| b.amount_out.cmp(&a.amount_out));"
        if marker not in text:
            raise SystemExit("could not locate path-sort marker")
        call = '''    if certified_interval_census_enabled() {
        emit_v2_certified_structural_interval_census(&search.census_paths, total, ctx);
    }

'''
        text = text.replace(marker, call + marker, 1)

    SEARCH.write_text(text)


if __name__ == "__main__":
    patch_refiner()
    patch_search()
    print("installed structural V2 interval census")
