# Lexion

Lexion is a hybrid DEX-routing system built on Fynd's market ingestion, graph, and pool-simulation infrastructure. Its routing layer adds resource-aware quotient search, exact integer replay, certified allocation bounds, and the experimental Sacred Timeline cutoff.

Lexion begins from Fynd's native Path Frank-Wolfe route, searches a bounded family of compatible disjoint V2, V3, V4, and mixed paths, exactly simulates each candidate allocation, and returns a hybrid route only when its gas-adjusted integer output is strictly better than the native result.

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

## Research direction: canonical flow-DAG search

The bounded hybrid is also the experimental base for a deeper exact-routing design derived from exact compiler-scheduling work: search canonical future-observable flow topologies instead of ordered swap histories, merge histories that differ only by commuting independent swaps, prune states that cannot jointly complete to the destination under the remaining pool resources, and solve amounts with topology-specific analytic or simulator-backed methods.

The design and its correctness obligations are documented in [`docs/canonical-flow-dag-search.md`](docs/canonical-flow-dag-search.md). It is research work, not a claim about the current production path.

The implemented pool-disjoint scheduling model, quotient proofs, related-work boundary, and
quotient-versus-exhaustive measurements are documented in
[`docs/routing-scheduling-formalization.md`](docs/routing-scheduling-formalization.md).

The implemented hybrid's method, acceptance rule, and limits are summarized in
[`docs/hybrid-algorithm.md`](docs/hybrid-algorithm.md).

The remaining proof, benchmarking, baseline, and production work is tracked in
[`REMAIN.md`](REMAIN.md).

## Attribution

Lexion is based on [Fynd](https://github.com/propeller-heads/fynd), whose ingestion, simulator, and market graph make exact route replay possible. Existing `fynd-*` crate names and APIs are retained for upstream compatibility; they do not name Lexion's routing contribution. Tycho supplies live market data and is not the routing algorithm described here.
