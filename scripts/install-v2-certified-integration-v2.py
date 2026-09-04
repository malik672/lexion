#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REFINER = ROOT / "fynd-core/src/algorithm/exact_v2_refiner.rs"
SEARCH = ROOT / "fynd-core/src/algorithm/exact_v2_search.rs"

# 1. Do not call the certified V2 allocator for mixed/V3 portfolios.
refiner = REFINER.read_text()
old_gate = '''    if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR").is_some() && current.len() == 2 {
        match v2_certified_interval_allocator::certified_allocate(current, total, market) {
'''
new_gate = '''    let certified_v2_supported = current.len() == 2
        && current.iter().all(|path| {
            !path.hops.is_empty()
                && path.hops.iter().all(|hop| {
                    market
                        .get_simulation_state(&hop.descriptor.component_id)
                        .is_some_and(|state| {
                            state.as_any().downcast_ref::<UniswapV2State>().is_some()
                        })
                })
        });
    if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR").is_some() && certified_v2_supported {
        match v2_certified_interval_allocator::certified_allocate(current, total, market) {
'''
if new_gate not in refiner:
    if old_gate not in refiner:
        raise SystemExit("could not locate installed certified allocator gate in exact_v2_refiner.rs")
    refiner = refiner.replace(old_gate, new_gate, 1)
REFINER.write_text(refiner)

# 2. Preserve the best *pool-disjoint* pure-V2 pair instead of blindly taking
#    the first two V2 paths. The path list is sorted by full-order output, but
#    the best two may share a pool and therefore can never enter combinations().
search = SEARCH.read_text()

helper = '''fn allocation_paths_pool_disjoint(left: &PathAllocation, right: &PathAllocation) -> bool {
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

'''
if helper not in search:
    anchor = 'fn same_allocation_path(left: &PathAllocation, right: &PathAllocation) -> bool {'
    pos = search.find(anchor)
    if pos == -1:
        raise SystemExit("could not locate certified frontier helpers in exact_v2_search.rs")
    # Insert immediately before same_allocation_path.
    search = search[:pos] + helper + search[pos:]

old_extras = '''    let v2_extras = if certified_v2_frontier {
        search
            .paths
            .iter()
            .filter(|path| pure_v2_allocation(path, ctx))
            .take(2)
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
'''
new_extras = '''    let v2_extras = if certified_v2_frontier {
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
'''
if new_extras not in search:
    if old_extras not in search:
        raise SystemExit("could not locate installed V2 frontier selection block in exact_v2_search.rs")
    search = search.replace(old_extras, new_extras, 1)

# Make trace explicitly report whether a compatible pair existed before Top-K.
old_trace = '''            eprintln!(
                "v2-certified-frontier: global={} total={} added={}",
                before,
                search.paths.len(),
                search.paths.len().saturating_sub(before)
            );
'''
new_trace = '''            eprintln!(
                "v2-certified-frontier: global={} total={} added={} compatible_pair={}",
                before,
                search.paths.len(),
                search.paths.len().saturating_sub(before),
                if v2_extras.is_empty() { 0 } else { 1 }
            );
'''
# v2_extras is consumed by the loop in the current installer. Capture the flag first.
if new_trace not in search:
    if old_trace in search:
        search = search.replace(
            '    if certified_v2_frontier {\n        let before = search.paths.len();\n        for candidate in v2_extras {',
            '    if certified_v2_frontier {\n        let before = search.paths.len();\n        let had_compatible_v2_pair = !v2_extras.is_empty();\n        for candidate in v2_extras {',
            1,
        )
        search = search.replace(
            old_trace,
            '''            eprintln!(
                "v2-certified-frontier: global={} total={} added={} compatible_pair={}",
                before,
                search.paths.len(),
                search.paths.len().saturating_sub(before),
                usize::from(had_compatible_v2_pair)
            );
''',
            1,
        )
    else:
        raise SystemExit("could not locate certified frontier trace block")

SEARCH.write_text(search)
print("installed certified V2 integration v2")
