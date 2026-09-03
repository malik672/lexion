#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SEARCH = ROOT / "fynd-core/src/algorithm/exact_v2_search.rs"

text = SEARCH.read_text()

helper = '''fn pure_v2_allocation(path: &PathAllocation, ctx: &BellmanFordContext) -> bool {
    !path.hops.is_empty()
        && path.hops.iter().all(|hop| {
            ctx.market_data
                .get_simulation_state(&hop.descriptor.component_id)
                .is_some_and(|state| state.as_any().downcast_ref::<UniswapV2State>().is_some())
        })
}

fn same_allocation_path(left: &PathAllocation, right: &PathAllocation) -> bool {
    left.hops.len() == right.hops.len()
        && left
            .hops
            .iter()
            .zip(&right.hops)
            .all(|(a, b)| a.descriptor.component_id == b.descriptor.component_id)
}

'''

if helper not in text:
    anchor = 'fn discover_paths(\n'
    pos = text.find(anchor)
    if pos == -1:
        raise SystemExit("could not locate discover_paths")
    text = text[:pos] + helper + text[pos:]

old = '''    search.paths.sort_unstable_by(|a, b| b.amount_out.cmp(&a.amount_out));
    search.paths.truncate(MAX_CANDIDATE_PATHS);
    Ok(search.paths)
'''
new = '''    search.paths.sort_unstable_by(|a, b| b.amount_out.cmp(&a.amount_out));

    // The global full-order Top-K can be entirely V3 even when competitive V2
    // pairs exist structurally. When the certified V2 allocator is enabled,
    // preserve a tiny protocol-specific V2 frontier through truncation so the
    // normal portfolio enumerator can actually exercise the 2-path allocator.
    // This adds at most two paths, keeping combinatorics bounded.
    let certified_v2_frontier = std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR").is_some();
    let v2_extras = if certified_v2_frontier {
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

    search.paths.truncate(MAX_CANDIDATE_PATHS);

    if certified_v2_frontier {
        let before = search.paths.len();
        for candidate in v2_extras {
            if !search.paths.iter().any(|path| same_allocation_path(path, &candidate)) {
                search.paths.push(candidate);
            }
        }
        if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
            eprintln!(
                "v2-certified-frontier: global={} total={} added={}",
                before,
                search.paths.len(),
                search.paths.len().saturating_sub(before)
            );
        }
    }

    Ok(search.paths)
'''

if new not in text:
    if old not in text:
        raise SystemExit("could not locate candidate truncation block")
    text = text.replace(old, new, 1)

SEARCH.write_text(text)
print("installed certified V2 frontier preservation")
