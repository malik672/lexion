#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REFINER = ROOT / "fynd-core/src/algorithm/exact_v2_refiner.rs"

text = REFINER.read_text()

old = '''            Ok(Some(refined)) => {
                if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
                    eprintln!("v2-certified-allocator: used paths={} total={}", current.len(), total);
                }
                return Ok(Some(refined));
            }
'''

new = '''            Ok(Some(refined)) => {
                if std::env::var_os("FYND_V2_CERTIFIED_PROD_DIFF").is_some() {
                    if let Some(baseline) = allocate_uniswap_v2_paths(current, total, market)? {
                        let baseline_out = baseline
                            .iter()
                            .fold(BigUint::zero(), |sum, path| sum + &path.amount_out);
                        let certified_out = refined
                            .iter()
                            .fold(BigUint::zero(), |sum, path| sum + &path.amount_out);
                        let (relation, delta) = if certified_out > baseline_out {
                            ("WIN", &certified_out - &baseline_out)
                        } else if certified_out < baseline_out {
                            ("LOSS", &baseline_out - &certified_out)
                        } else {
                            ("TIE", BigUint::zero())
                        };
                        let baseline_x = baseline
                            .get(1)
                            .map(|path| path.amount_in.clone())
                            .unwrap_or_else(BigUint::zero);
                        let certified_x = refined
                            .get(1)
                            .map(|path| path.amount_in.clone())
                            .unwrap_or_else(BigUint::zero);
                        eprintln!(
                            "v2-certified-prod-diff: relation={} baseline={} certified={} delta={} baseline_x={} certified_x={}",
                            relation,
                            baseline_out,
                            certified_out,
                            delta,
                            baseline_x,
                            certified_x,
                        );
                    }
                }
                if std::env::var_os("FYND_V2_CERTIFIED_ALLOCATOR_TRACE").is_some() {
                    eprintln!("v2-certified-allocator: used paths={} total={}", current.len(), total);
                }
                return Ok(Some(refined));
            }
'''

if new not in text:
    if old not in text:
        raise SystemExit("could not locate certified allocator success arm; install production allocator first")
    text = text.replace(old, new, 1)

REFINER.write_text(text)
print("installed certified V2 production differential")
