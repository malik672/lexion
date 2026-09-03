#!/usr/bin/env python3
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
REFINER = ROOT / "fynd-core/src/algorithm/exact_v2_refiner.rs"
INSTALLER = ROOT / "scripts/install-v2-structural-interval-census.py"

text = REFINER.read_text()

compact_path_curve = '''fn path_curve(path: &PathAllocation, market: &MarketState) -> Option<ContinuousCpmm> {
    let mut curve = None;
    for hop in &path.hops {
        let descriptor = &hop.descriptor;
        let state = market.get_simulation_state(&descriptor.component_id)?.as_any().downcast_ref::<UniswapV2State>()?;
        let hop_curve = ContinuousCpmm::one_hop(state, descriptor.token_in.address < descriptor.token_out.address);
        curve = Some(match curve { Some(previous) => ContinuousCpmm::compose(&previous, &hop_curve), None => hop_curve });
    }
    curve
}
'''

formatted_path_curve = '''fn path_curve(path: &PathAllocation, market: &MarketState) -> Option<ContinuousCpmm> {
    let mut curve = None;
    for hop in &path.hops {
        let descriptor = &hop.descriptor;
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
'''

if "fn descriptor_path_curve(" not in text:
    if formatted_path_curve not in text:
        if compact_path_curve not in text:
            raise SystemExit("compat: could not locate current path_curve implementation")
        text = text.replace(compact_path_curve, formatted_path_curve, 1)

pair_sig = "fn certified_v2_pair_upper("
pair_anchor = "/// Sound upper bound on the exact output of a two-path V2 split over x in [0,T].\nfn certified_v2_pair_upper("
if "pub(super) fn certified_v2_interval_upper_for_paths(" not in text and pair_anchor not in text:
    if pair_sig not in text:
        raise SystemExit("compat: could not locate certified_v2_pair_upper")
    text = text.replace(pair_sig, "/// Sound upper bound on the exact output of a two-path V2 split over x in [0,T].\n" + pair_sig, 1)

REFINER.write_text(text)

result = subprocess.run([sys.executable, str(INSTALLER)], cwd=ROOT)
raise SystemExit(result.returncode)
