# Lexion Benchmarks

This document preserves the benchmark record produced by the original Lexion implementation and
separates it from measurements of this rewrite. Results from the original implementation are not
presented as measurements of the rewrite until the same campaign is rerun here.

## Rewrite parity smoke test

The rewrite was compared directly with the original router against live Tycho snapshots using the
same five representative orders, a two-hop limit, and a 10 ETH minimum-TVL filter.

In the latest recorded run, four of five pairs had exactly equal gross output. The remaining pair,
WETH to USDC, differed by `0.00723` bps gross in favor of the original, while the rewrite was
`0.01798` bps better after the harness's gas normalization. The rewrite answered in approximately
`2.7` to `88.1` ms; original responses ranged from approximately `5` ms to `19.1` seconds in that
sample.

This is a parity smoke test, not a statistically powered performance comparison. Live block
movement, cache state, and different process boundaries can affect latency.

Reproduce it while the original service is listening on port `30320`:

```bash
./scripts/parity-original.sh --details
```

## Original implementation: routing ablation

A repeated ablation across three pinned captures compared five search configurations:

| Configuration | Solved | Mean capture time |
|---|---:|---:|
| Ordered-history control | 149/150 | 132.846 s |
| Canonical combinations | 149/150 | 122.884 s |
| Maximal frontiers | 149/150 | 121.831 s |
| Exact zero-loss predicate | 149/150 | 123.051 s |
| `0.01`-bps cutoff | 125/150 | 220.565 s |

Canonical combinations and maximal frontiers removed redundant work in this experiment. The
approximate cutoff did not: although an earlier single capture showed a `4.48x` speedup, the
repeated campaign found it slower and observed 24 additional unsolved orders. It therefore remained
disabled by default.

A later paired reliability campaign produced identical counts for the normal and cutoff
configurations: 152 successful replays, 12 reverts, and 106 unavailable quotes over 270 attempts
per configuration. That passed the comparative reliability gate but did not remove the cutoff's
latency and coverage regression.

## Original implementation versus Tycho/Fynd baseline

The original implementation compared Lexion's multiscale portfolio allocator
(`path_frank_wolfe_multiscale_d2`) with the routing framework's existing depth-two Bellman–Ford
configuration (`bellman_ford_d2`) on one live 500-order capture. Both configurations used the same
captured Uniswap V2, V3, and V4 market.

| Metric | Bellman–Ford baseline | Lexion multiscale |
|---|---:|---:|
| Solved | 485/500 | 485/500 |
| Compared | — | 485 |
| Better / worse / exact tie | — | 127 / 0 / 358 |
| Mean improvement | — | +11.3 bps |
| Median improvement | — | +0.0 bps |
| Total solve time | 326 ms | 29,299 ms |
| Per-order p50 | 467 us | 8,213 us |
| Per-order p95 | 1,230 us | 282,532 us |

This establishes that the broader portfolio search improved 127 orders without reducing any of the
485 commonly solved integer outputs in that capture. It was also substantially more expensive than
the baseline. Ties dominated, and the positive mean must not be read as the typical order because
the median was exactly zero.

Here “Tycho/Fynd baseline” means the original `bellman_ford_d2` configuration in the same local
benchmark harness. It is not a benchmark against every algorithm supported by Tycho, nor a claim
about Tycho's data quality.

Source artifact in the original repository:
`artifacts/benchmarks/runs/sacred-timeline-cutoff-500/report.md`.

## Original implementation versus Uniswap SOR

The external comparator was Uniswap's Smart Order Router/AlphaRouter. The five-block executable
comparison attempted 150 orders and produced 75 comparable observations. Both selected routes were
normalized through executable replay and common gas accounting where the harness could construct
both transactions. Differences below one US dollar were classified as economic ties.

| Metric | Result |
|---|---:|
| Blocks | 5 |
| Attempted orders | 150 |
| Comparable executable orders | 75 |
| Comparable coverage | 50.00% |
| Required publication coverage | 80.00% |
| Diagnostic Lexion / tie / Uniswap | 18 / 54 / 3 |
| Mean Lexion-relative advantage | 1.743345 bps |
| Paired 95% CI | [-13.128544, 16.615235] bps |
| Mean Lexion response latency | 1421.7 ms |
| Mean Uniswap response latency | 30827.5 ms |

The publication gate failed because comparable coverage was below 80%, and the confidence interval
crossed zero. This result is diagnostic and does not establish general superiority over Uniswap.

The observed response-time difference also does not isolate algorithmic speed. Lexion queried its
already-maintained local market, while the Uniswap pipeline crossed a different process/network and
data boundary. It records user-observed latency for this harness, not a controlled CPU-time race.

All 49 recorded `lexion:no_route` observations involved six endpoint tokens absent from a later
admitted-market snapshot. That supports a market-boundary explanation but is not exact historical
proof because the original captures did not retain their complete topology.

Source artifact in the original repository:
`artifacts/benchmarks/comparisons/uniswap/publication-multiblock-v2/lexion-vs-uniswap-multiblock-report.md`.

## Interpretation rules

- Compare executable routes against the same captured state.
- Normalize both routes through the same gas and replay machinery.
- Report failures and missing coverage, not only comparable winners.
- Treat confidence intervals crossing zero as inconclusive.
- Do not infer a universal latency advantage from different system boundaries.
- Do not attribute original-implementation campaigns to this rewrite until they are rerun.
