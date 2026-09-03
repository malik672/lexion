# Hybrid V2/V3 Router

This is a runnable Rust workspace for a hybrid DEX-routing algorithm. It uses Fynd's market ingestion and pool simulators as infrastructure, while the routing decision is extended with our exact-replay V2/V3 refinement.

The hybrid begins from Fynd's native Path Frank-Wolfe route, searches a bounded family of compatible disjoint V2, V3, and mixed V2/V3 paths, exactly simulates each candidate allocation, and returns a hybrid route only when its gas-adjusted integer output is strictly better than the native result.

## Core invariant

For one market snapshot and gas model, let `native` be the baseline net output and `hybrid` the best exact-replayed candidate. The result is:

```text
hybrid, if hybrid > native
native, otherwise
```

So the hybrid layer never replaces the baseline with a lower simulated net output.

## Run

```sh
cargo test -p fynd-core exact_v2 --lib

# Offline native-versus-hybrid comparison.
./scripts/compare-exact-v2.sh

# Quote a live USDT -> WETH trade; credentials stay in your environment.
TYCHO_URL=... TYCHO_API_KEY=... RPC_URL=... \
  ./scripts/quote-hybrid-usdt-to-eth.sh 100000
```

The live quote is a snapshot simulation, not an executable-trade guarantee. Production validation still needs repeated live-state samples, calldata/settlement checks, gas accuracy, staleness/latency tests, reverts, and MEV analysis.

## Where the hybrid lives

- `fynd-core/src/algorithm/exact_v2_search.rs`: candidate-path search.
- `fynd-core/src/algorithm/exact_v2_refiner.rs`: exact V2/V3 allocation replay and acceptance.
- `fynd-core/src/algorithm/path_frank_wolfe.rs`: baseline-to-hybrid integration.
- `scripts/`: benchmark, randomized campaign, and live-quote helpers.

## Attribution

This workspace is based on [Fynd](https://github.com/propeller-heads/fynd), whose simulator and market graph make exact route replay possible. Tycho is an infrastructure dependency used for live market data; it is not the algorithm described here.
