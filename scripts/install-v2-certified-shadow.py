#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REFINER = ROOT / "fynd-core/src/algorithm/exact_v2_refiner.rs"

text = REFINER.read_text()

module_decl = '#[path = "v2_certified_interval_allocator.rs"]\nmod v2_certified_interval_allocator;\n\n'
if module_decl not in text:
    anchor = 'use crate::feed::market_data::MarketState;\n\n'
    if anchor not in text:
        raise SystemExit("could not locate MarketState import anchor")
    text = text.replace(anchor, anchor + module_decl, 1)

old = '    if let Some(refined) = allocate_uniswap_v2_paths(current, total, market)? { return Ok(Some(refined)); }\n'
new = '''    if let Some(refined) = allocate_uniswap_v2_paths(current, total, market)? {
        if std::env::var_os("FYND_V2_CERTIFIED_INTERVAL_SHADOW").is_some() {
            v2_certified_interval_allocator::shadow_compare(current, total, market, &refined)?;
        }
        return Ok(Some(refined));
    }
'''

if new not in text:
    if old not in text:
        formatted = '''    if let Some(refined) = allocate_uniswap_v2_paths(current, total, market)? {
        return Ok(Some(refined));
    }
'''
        if formatted not in text:
            raise SystemExit("could not locate V2 allocator return block")
        text = text.replace(formatted, new, 1)
    else:
        text = text.replace(old, new, 1)

REFINER.write_text(text)
print("installed V2 certified interval shadow allocator")
