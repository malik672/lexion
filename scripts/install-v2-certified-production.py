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

prod = '''    if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR").is_some() && current.len() == 2 {
        match v2_certified_interval_allocator::certified_allocate(current, total, market) {
            Ok(Some(refined)) => {
                if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
                    eprintln!("v2-certified-allocator: used paths={} total={}", current.len(), total);
                }
                return Ok(Some(refined));
            }
            Ok(None) => {
                if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
                    eprintln!("v2-certified-allocator: fallback unsupported paths={}", current.len());
                }
            }
            Err(error) => {
                if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
                    eprintln!("v2-certified-allocator: fallback error={error}");
                }
            }
        }
    }

'''

if prod not in text:
    anchor = '    if let Some(refined) = allocate_uniswap_v2_paths(current, total, market)? {'
    pos = text.find(anchor)
    if pos == -1:
        # Compact source before shadow wiring.
        anchor = '    if let Some(refined) = allocate_uniswap_v2_paths(current, total, market)? { return Ok(Some(refined)); }'
        pos = text.find(anchor)
    if pos == -1:
        raise SystemExit("could not locate V2 allocator call")
    text = text[:pos] + prod + text[pos:]

REFINER.write_text(text)
print("installed feature-gated certified V2 allocator")
