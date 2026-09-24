# Lexion: Engineering an Executable Ethereum Router

## Project summary

I built Lexion to explore a deceptively difficult systems problem: given an exact-input Ethereum
swap, find a high-quality allocation across fragmented Uniswap V2, V3, and passive V4 liquidity,
then turn that plan into a transaction that can actually execute.

The result is a local Rust router backed by Tycho market data and integer pool simulation. It keeps
an immutable view of a live market, discovers bounded paths, constructs compatible split-route
portfolios, optimizes the allocation, produces Tycho Router calldata, and validates the unsigned
transaction with `eth_call` before returning it.

Repository: [github.com/malik672/lexion](https://github.com/malik672/lexion)

## Why I built it

A routing algorithm cannot be judged only by the price predicted by its planner. Real execution
introduces integer rounding, tick boundaries, mutable pool state, gas, incompatible paths, calldata
constraints, and state changes between quote and inclusion.

I wanted a router whose final decision was grounded in the same physical transition that a user
would execute. That led to four design requirements:

1. Evaluate candidate routes through integer pool simulators rather than a floating-point proxy.
2. Reject split portfolios whose legs conflict over a mutable pool.
3. Keep market topology and pool state pinned to one coherent block snapshot.
4. Treat transaction construction and replay as part of correctness, not an integration detail.

## What I implemented

### Live, versioned market state

The Tycho integration decodes live V2, V3, and V4 components into a compact market representation.
Topology is reusable across state updates, while readers pin an immutable snapshot for the entire
quote. A completed update is published atomically, so a route cannot accidentally combine pool
states from different generations.

Relevant code:

- [`src/tycho.rs`](src/tycho.rs): stream ingestion and protocol adapters
- [`src/market.rs`](src/market.rs): topology, immutable snapshots, and publication
- [`src/runtime.rs`](src/runtime.rs): quote workers, cache use, health, and staleness checks

### Bounded path and portfolio search

Lexion discovers direct and bounded-hop paths, canonicalizes portfolio construction so equivalent
path orderings are not revisited, and rejects portfolios containing shared mutable components.
Rather than trusting the searcher's estimate, it replays each proposed allocation against the pool
simulators and retains the best observed integer output.

Relevant code:

- [`src/router.rs`](src/router.rs): structural path discovery
- [`src/portfolio.rs`](src/portfolio.rs): compatible portfolio construction
- [`src/frontier.rs`](src/frontier.rs): frontier enumeration
- [`src/solver.rs`](src/solver.rs): simulation-backed selection and gas valuation
- [`src/allocator.rs`](src/allocator.rs): allocation refinement
- [`src/certified_v2.rs`](src/certified_v2.rs): certified V2 subproblem

### Executable transaction boundary

The CLI can produce an unsigned Tycho Router transaction for its selected allocation. It derives a
minimum output from the user's slippage limit, encodes the exact pool sequence and amounts, reports
the required approval spender, and performs a same-block `eth_call` before printing the
transaction.

I deliberately made this boundary fail closed. The current encoder rejects native-token input and
portfolios that reuse one pool across legs because those cases require additional balance-flow and
merged-state rules. Lexion never reads a private key, signs, or broadcasts.

Relevant code:

- [`src/execution.rs`](src/execution.rs): route conversion, calldata encoding, and dry-run replay
- [`src/main.rs`](src/main.rs): `quote` and `swap` commands

## Architecture

```text
Tycho block stream
      |
      v
decoded V2/V3/V4 components
      |
      v
immutable MarketSnapshot ──> bounded path discovery
                                  |
                                  v
                         compatible portfolios
                                  |
                                  v
                        allocation + integer replay
                                  |
                                  v
                         best simulated solution
                                  |
                                  v
                    Tycho Router calldata + eth_call
```

The main boundary is intentional: data acquisition, structural search, physical simulation, and
execution encoding remain separate. This made it possible to test each layer independently and to
compare the rewrite with the original router at the exact-output level.

## Results

### Rewrite parity

In the latest five-pair live smoke comparison with the original implementation, four pairs returned
exactly equal gross output. The fifth differed by `0.00723` bps gross in favor of the original, but
the rewrite was `0.01798` bps better after the harness's gas normalization. Observed rewrite
latency ranged from roughly `2.7` to `88.1` ms in that small sample.

### Original research prototype versus Fynd baseline

On one retained 500-order V2/V3/V4 capture, the broader portfolio search and the existing Fynd
Bellman–Ford configuration both solved 485 orders. Lexion improved 127, tied exactly on 358, and
returned no lower integer output. Mean improvement was `11.3` bps; median improvement was zero.

That quality came with a real cost: total solve time rose from `326` ms for the baseline to
`29.299` seconds for the broader search. This is an explicit engineering trade-off, not a hidden
footnote.

### Original research prototype versus Uniswap SOR

A five-block executable replay attempted 150 orders and obtained 75 comparable transactions. With
differences below one dollar treated as economic ties, the diagnostic result was 18 Lexion wins,
54 ties, and three Uniswap wins.

This does **not** establish general superiority: comparable coverage was only 50%, below the
predeclared 80% gate, and the paired 95% confidence interval for mean advantage crossed zero. I
retain these negative qualifications because reliable benchmarking is part of the work.

The complete measurements and interpretation rules are in [`BENCHMARKS.md`](BENCHMARKS.md).

## Engineering lessons

### The simulator is the authority

A planner predicts which route should win; exact replay decides which route did win under the
captured model. Keeping those roles separate prevented attractive analytical approximations from
silently becoming execution claims.

### State consistency matters as much as search quality

Comparing two routes at different blocks can manufacture an apparent routing advantage. The same
principle applies inside one router: a quote assembled from independently changing pool states is
not a coherent quote. Immutable, generation-tagged snapshots solve that class of bug directly.

### More search is not automatically better

Canonical combinations and maximal frontiers removed redundant histories in controlled ablations.
An experimental `0.01`-bps cutoff looked promising on one capture, then became slower and reduced
coverage in repetition. I left it disabled. The lesson was to promote optimizations only after
paired, repeated measurement.

### Missing evidence is not a guarantee

The project distinguishes executable candidates, simulator-relative optima, certified subproblems,
and unsupported cases. Arbitrary V4 hooks remain outside certification because they may alter
amounts, fees, gas, external state, or the mutable-resource footprint.

## What this project demonstrates

- Rust systems engineering across asynchronous ingestion, immutable state, graph search, numeric
  optimization, and EVM transaction construction.
- Translating a research prototype into a smaller implementation with explicit module boundaries.
- Differential testing against an existing implementation at exact integer-output granularity.
- Performance measurement that reports latency, coverage, failures, and uncertainty rather than
  only favorable wins.
- Security-oriented API choices: no private-key custody, same-block dry runs, explicit slippage,
  and fail-closed handling of unsupported execution shapes.
- Comfort revising a design when experiments falsify its assumptions.

## Current scope and next steps

Lexion is currently suitable for experimentation and personal local use, not unattended custody or
production execution. The most valuable next steps are broader rewrite-native benchmarks, retained
snapshot fixtures for deterministic regression tests, richer execution support for shared-pool and
native-token flows, and shadow-mode reliability monitoring across state advancement.

## Run it

```bash
git clone https://github.com/malik672/lexion.git
cd lexion
cp .env.example .env
# Configure TYCHO_API_KEY, RPC_URL, wallet, token pair, and amount.
cargo build --release
./target/release/lexion quote
./target/release/lexion swap
```

The project is MIT licensed.

