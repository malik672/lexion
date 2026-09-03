#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REFINER = ROOT / "fynd-core/src/algorithm/exact_v2_refiner.rs"
SEARCH = ROOT / "fynd-core/src/algorithm/exact_v2_search.rs"

r = REFINER.read_text()
module_decl = '#[path = "v2_certified_interval_allocator.rs"]\nmod v2_certified_interval_allocator;\n\n'
if module_decl not in r:
    anchor = 'use crate::feed::market_data::MarketState;\n\n'
    if anchor not in r:
        raise SystemExit('could not locate MarketState import')
    r = r.replace(anchor, anchor + module_decl, 1)

wrapper = '''pub(super) fn shadow_compare_structural_pair(
    left: &[HopDescriptor],
    right: &[HopDescriptor],
    total: &BigUint,
    market: &MarketState,
) -> Result<(), AlgorithmError> {
    v2_certified_interval_allocator::shadow_compare_descriptors(left, right, total, market)
}

'''
if wrapper not in r:
    anchor = 'pub(super) fn refine_disjoint_allocations('
    if anchor not in r:
        raise SystemExit('could not locate refine_disjoint_allocations')
    r = r.replace(anchor, wrapper + anchor, 1)
REFINER.write_text(r)

s = SEARCH.read_text()
hook = '''            if std::env::var_os("FYND_V2_CERTIFIED_INTERVAL_SHADOW").is_some() {
                let _ = super::exact_v2_refiner::shadow_compare_structural_pair(
                    &structural_paths[left],
                    &structural_paths[right],
                    total,
                    &ctx.market_data,
                );
            }
'''
if hook not in s:
    anchor = '            stats.pairs += 1;\n'
    count = s.count(anchor)
    if count != 1:
        raise SystemExit(f'expected one structural pair anchor, found {count}')
    s = s.replace(anchor, anchor + hook, 1)
SEARCH.write_text(s)
print('installed structural V2 shadow differential')
