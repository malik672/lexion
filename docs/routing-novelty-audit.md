# Novelty audit: resource-constrained, simulator-bound routing

## Executive conclusion

The broad router is not novel. Candidate-path discovery, split routing, gas-aware scoring,
simulator replay, maximal or disjoint path selection, branch-and-bound, and continuous CFMM
optimization all have substantial prior art.

The strongest defensible research contribution is narrower:

> A bounded heterogeneous-AMM routing problem is represented as a finite resource-constrained
> transition system; path-selection histories are quotiented by a canonical future-sufficient
> resource state; compatible subsets are covered by maximal resource frontiers; and allocation
> work is eliminated only through sound upper bounds evaluated against authoritative integer
> simulator semantics, with an explicit bounded-quality variant.

No reviewed source presents that complete construction. The individual ingredients are known,
but their composition, proof boundary, and application to V2/V3/passive-V4 integer simulators
appear distinct in the public literature reviewed here. This supports **apparently novel
combination** or **novel application/architecture**, not an unconditional claim of being the
first router or the first optimal-routing algorithm.

This is a technical prior-art review, not a legal patentability opinion. A legal novelty opinion
would require jurisdiction-specific claim drafting, a professional patent search, unpublished
applications, and non-public production systems.

## 1. What is actually being claimed

Fix a market snapshot, input amount, gas price, finite candidate-path set, deterministic integer
simulators, and a bound on active routes. Each path owns a set of mutable pool resources. A
selection history is observable through

```math
\Phi(h)=(A(h),U(h)),
```

where `A(h)` is the canonical unordered selected-path set and `U(h)` is the union of occupied
mutable resources. Histories with equal observations are equivalent:

```math
h_1\sim h_2 \iff \Phi(h_1)=\Phi(h_2).
```

Because future selection legality and terminal scoring depend only on this observation, equal
states have equal continuation languages and scored completions. Searching one representative
per class therefore preserves the optimum of the bounded modeled system.

The second reduction covers every compatible subset by at least one maximal compatible resource
frontier. It preserves the optimum only when zero-flow paths cost nothing, replay is separable,
the support limit is retained, and the amount optimizer is globally exact on every admitted
frontier. The current general V3/V4 coordinate-ascent allocator does not satisfy that last premise;
the theorem is therefore conditional outside certified allocation families.

The third component is certificate-based elimination. A timeline or allocation interval may be
discarded only when a sound upper bound proves it cannot beat the exact incumbent. The approximate
variant prunes at

```math
U(T)\le B(1+\delta)
```

and proves the modeled guarantee `OPT <= B_final(1 + delta)`.

These claims concern exactness relative to the captured simulator-defined machine. They do not
prove that candidate discovery found every on-chain path, that arbitrary V4 hooks are faithfully
modeled, or that the state will remain unchanged before execution.

## 2. Claim decomposition

| Component | Prior-art status | Assessment |
|---|---|---|
| Graph/path discovery | Extensive | Not novel |
| Splitting one order across routes | Extensive | Not novel |
| Gas-adjusted route scoring | Extensive | Not novel |
| Local deterministic pool simulation | Used by production routers and Fynd | Not novel |
| Convex/KKT CFMM routing | Angeris et al.; Diamandis et al.; Balancer | Not novel |
| Fixed-cost activation in CFMM routing | Angeris et al.; Escudero et al. | Not novel |
| Branch-and-bound with sound upper bounds | General optimization prior art | Not novel |
| Canonicalizing permutations of the same selected resource set | Standard state-space reduction in principle | The mathematical device is not novel by itself |
| Proving that this canonical state is future-sufficient for bounded pool-disjoint AMM portfolios | No direct match found | Plausibly novel application/formalization |
| Maximal-compatible-frontier cover with zero-flow closure | Related to maximal-set/face covers generally | No direct DEX-routing match found; theorem is simple but useful |
| Simulator-bound exactness for integer, discontinuous, potentially opaque AMM behavior | No direct match found in reviewed routing systems | Strong differentiator |
| Exact/epsilon-certified elimination layered onto that quotient machine | No complete match found | Strongest combination claim |

The novelty, if publishable, is therefore not a new primitive algorithm. It is the exact
composition and the boundary between certified and uncertified behavior.

## 3. Academic routing literature

### Angeris, Chitra, Evans, and Boyd

*Optimal Routing for Constant Function Market Makers* formulates network routing as convex
optimization when fixed per-CFMM execution costs are ignored. With fixed costs, it becomes a
mixed-integer convex problem, for which the paper discusses global methods and convex heuristics.^1
This work is mathematically stronger than Lexion on the domain satisfying its trading-set
assumptions: it can optimize a whole CFMM network rather than only a bounded discovered frontier.

It does not make Lexion redundant. Lexion deliberately keeps integer rounding, exact simulator
failures, tick behavior, route-specific gas, and admitted V4 behavior inside the objective. The
local concavity census in this repository falsifies universal concavity for that concrete
objective. The two systems therefore optimize different mathematical models.

### Diamandis, Resnick, Chitra, and Angeris

*An Efficient Algorithm for Optimal Routing Through Constant Function Market Makers* decomposes
the convex routing problem into market-level arbitrage subproblems coupled by dual prices.^2 It can
represent aggregate bounded-liquidity markets such as Uniswap V3 and solves a convex dual rather
than enumerating route histories. This is important prior art for structural reduction and
extensible market interfaces.

The distinction is not merely implementation language. Diamandis et al. rely on convex duality
and market price/arbitrage interfaces. Lexion asks a finite transition-system question over exact
integer simulator outputs and uses bounds that remain sound without assuming global concavity or
differentiability. Lexion should not claim to improve the Diamandis algorithm on its admitted
model; it supplies a different fallback domain when those assumptions cannot be certified.

### Routing with gas and generalized convexity

Escudero, Lara, and Sama's 2026 paper explicitly incorporates fixed gas costs with mixed-integer
activation and derives KKT conditions beyond global convexity using generalized convexity.^3 This
substantially narrows any claim that “existing mathematics ignores gas” or “all prior routing
requires ordinary concavity.” However, the framework still assumes differentiable invariant
functions and proves conditions on that analytic model. It does not cover arbitrary black-box
integer simulators or hook-defined state transitions merely by treating them as callable code.

Zhavoronkov's 2026 multi-path system combines gas-aware marginal k-shortest paths, concave
allocation, and a KKT improving-path certificate for omitted paths.^4 This is close prior art for
certificate-driven route discovery. Its certificate asks whether an omitted path can improve a
continuous concave solution; Lexion's certificate asks whether any descendant of a finite
simulator-defined state or any integer allocation interval can beat an incumbent. The proof
mechanisms and admitted domains differ.

### Result of the academic comparison

The reviewed papers establish that these claims would be indefensible:

- first mathematically optimized DEX router;
- first globally optimal CFMM router;
- first router with gas costs;
- first decomposed or certificate-based router;
- first router supporting concentrated liquidity.

They do not disclose the exact resource-state quotient plus maximal-frontier cover plus
integer-simulator certificate architecture described here.

## 4. Production-router comparison

### Uniswap AlphaRouter

Uniswap's public router enumerates candidate pools/routes, obtains quotes at configured percentage
buckets, and performs a breadth-first combination search over route splits. Its configuration
explicitly exposes `distributionPercent`, `maxSplits`, and `maxSwapsPerPath`; its code also includes
an early stop justified as further splits being “very unlikely” to improve once a layer fails to
improve.^5 This is strong prior art for split, gas-adjusted V2/V3/V4 routing, but it is a discretized
heuristic search, not the resource-state quotient or simulator-bound certificate construction.

The repository's same-state replay comparison therefore demonstrates behavior of two concrete
implementations, not novelty. It is still useful evidence that the architecture can be practical.

### Balancer SOR

Balancer's archived SOR describes optimal splitting by equal post-swap spot prices and requires
first- and second-order differentiable `spotPriceAfterSwap` functions for pool integrations.^6
That is direct production prior art for marginal-price/KKT-style routing over heterogeneous pool
math. It is not a black-box integer transition solver and does not disclose the Lexion quotient.

### 0x

0x publicly described a three-stage system: sample increasing fill sizes from liquidity sources,
convert samples into fill-path DAGs, then merge paths to maximize gas-adjusted return.^7 This is
especially important prior art because it uses a discrete sampled representation and DAGs rather
than only closed-form CFMM curves. Lexion must therefore not claim that simulator/sampled routing,
fill DAGs, or gas-aware path merging are new.

The reviewed 0x description does not identify histories by canonical occupied-resource state,
cover compatible supports with maximal frontiers, or attach exact integer upper-bound certificates
to that quotient.

### 1inch Pathfinder

1inch's 2025 Pathfinder description reports finer splitting, concentrated-liquidity use, and
merging intermediate route steps so that a profitable market can be reused and execution gas is
reduced.^8 The implementation is not published at sufficient detail to rule out internal methods
similar to Lexion. This uncertainty is a major reason the conclusion must be “no public match
found,” not “no router has ever done this.”

### ParaSwap / Velora

Velora documents optimal-price routing across many exchanges, `MultiPath`/`MegaPath` execution,
gas-optimized v6.2 contracts, and an API capable of restricting the DEX universe.^9 Its current
public documentation does not disclose enough of the Hopper routing algorithm to compare internal
state abstraction or proof rules. The executable equal-universe benchmark in this repository is
performance evidence only.

### CoW Protocol

CoW uses combinatorial batch auctions in which solvers compete over user orders, coincidence of
wants, and external liquidity; its driver simulates and ranks submitted solutions.^10 This is a
broader market-design problem than a single-order AMM router. CoW is relevant prior art for
combinatorial optimization and executable simulation, but a quote comparison does not isolate
algorithm quality because the product, fee incidence, liquidity universe, and settlement model
differ.

### Upstream Fynd

Upstream Fynd already provides real-time Tycho state, multi-protocol routing, multiple competing
algorithms, local simulators, and gas-aware net ranking.^11 Lexion is an extension of this system,
not an independently invented data or execution platform. The research contribution must be
attributed to the added search/reduction/certificate layers, while clearly crediting Fynd and
Tycho for the base architecture and market model.

## 5. The closest conceptual prior art

Outside DEX routing, several ideas are classical:

- dynamic programming merges histories that reach the same sufficient state;
- automata minimization and right-language equivalence identify states with identical futures;
- partial-order reduction removes redundant interleavings of independent actions;
- resource-constrained path algorithms carry resource sets in their state;
- branch-and-bound discards subtrees using valid objective bounds;
- maximal compatible sets and zero-extension cover lower-dimensional faces of a simplex.

Accordingly, the proof that path-order permutations may be quotiented is mathematically natural,
not a new general theorem. What may be original is choosing the precise observation
`(selected paths, occupied mutable pools)` for this router, proving it sufficient under
pool-disjoint replay, connecting maximal frontiers to zero-flow allocation, and preserving exact
integer simulator semantics through explicit certificate families.

This distinction matters for writing. “A new state-space reduction principle” is too broad.
“A future-sufficient resource quotient for bounded heterogeneous-AMM portfolio search” is precise.

## 6. Patent-search warning

A preliminary keyword search found broad cryptocurrency smart-order-router patents, including a
distributed router that selects orders across exchanges using cost calculations.^12 None of the
reviewed material matched the narrow quotient/certificate construction. Keyword searching is not
claim-chart analysis, however, and patent applications can use vocabulary far removed from the
paper's terminology. This review cannot support freedom-to-operate or patentability conclusions.

## 7. Benchmark evidence and what it proves

Internal quotient benchmarking measured a 2.41x aggregate speedup against unquotiented exhaustive
portfolio construction, with nearly identical but not universally equal outputs because the
general amount optimizer remains heuristic. A 500-order census measured 5.42x compression from
ordered histories to canonical portfolios. Those measurements directly support the usefulness of
the quotient; they do not by themselves establish novelty.

The strongest external controlled result is executable same-state replay. On the equal
Uniswap-V2/V3/V4 universe, 28 Lexion/ParaSwap routes were replayed from the same captured state.
Lexion led 23 to 5 after measured gas; with differences at or below two cents treated as ties, the
result was 19 Lexion, 5 ties, and 4 ParaSwap. The earlier result based on ParaSwap's self-reported
gas pointed in the opposite direction. This demonstrates why authoritative transition replay is
valuable, but it remains a small observational sample rather than proof of universal superiority.

Likewise, the three-block Uniswap comparison and CoW runs establish implementation behavior under
specific captures. They do not prove “best router,” and API latency is not isolated solver CPU
time.

## 8. Defensible novelty statement

The recommended paper claim is:

> We introduce and evaluate a resource-constrained scheduling formulation for bounded
> heterogeneous-AMM routing. The formulation quotients path-selection traces by a canonical
> future-sufficient resource state, covers compatible path supports through maximal resource
> frontiers under zero-flow closure, and applies exact or explicitly bounded-quality elimination
> against integer simulator semantics. Unlike convex and marginal-price routing methods, the
> certified subproblems do not require global concavity or differentiability; unsupported
> simulator families fall back without being mislabeled as exact.

The phrase “to our knowledge” should precede any first-of-kind statement. A safe version is:

> To our knowledge, this is the first published formulation combining a future-sufficient
> resource quotient, maximal compatible-frontier cover, and simulator-validated integer
> certificates for bounded V2/V3/V4 route portfolios.

That sentence should remain provisional until the work has passed a systematic scholarly search,
code search, and ideally external peer review. The present audit supports it, but does not prove a
universal negative about private router implementations.

## 9. What remains before publication

1. **Separate theorem from implementation.** Mark every theorem assumption beside the production
   code path that establishes it. In particular, do not extend maximal-frontier optimality to the
   coordinate-ascent fallback.
2. **Define the admitted protocol families.** Passive V4 is not arbitrary V4. State exactly which
   hooks are unsupported and why. This boundary is now specified in the formalization's
   “Admitted protocol families” section: all nonzero V4 hook handlers remain routable when their
   simulator succeeds but are excluded from certificates until their monotonicity, gas behavior,
   feasible domain, and complete mutable-resource footprint are proved.
3. **Complete an ablation matrix.** Compare ordered-history search, canonical quotient search,
   maximal-frontier search, exact zero-bps pruning, and 0.01-bps pruning on identical captures.
4. **Repeat external replay.** Use more blocks, independent order samples, paired confidence
   intervals, per-pair concentration, and identical executable-state replay.
5. **Add stronger baselines.** Include 0x and 1inch when reproducible API access is available;
   keep CoW separate as a different settlement product.
6. **Publish counterexamples.** Include concrete non-concavity/tick/rounding examples that defeat
   continuous assumptions, not only aggregate counts.
7. **Freeze terminology.** Use “trace quotient,” “maximal-frontier cover,” “simulator-bound
   certificate,” and “bounded-quality elimination.” “Sacred Timeline” is memorable engineering
   nomenclature, but the formal term should be used in the theorem statement.
8. **Seek adversarial review.** Ask reviewers specifically to find a prior system with the same
   state quotient, a violation of future sufficiency, or a certificate that underestimates a
   simulator completion.

## 10. Final assessment

The architecture is not novel because it beats a router on a benchmark, and it is not novel merely
because it uses scheduling language. It is potentially novel because a particular ordering of
known ideas yields a different formal object:

```text
heterogeneous integer simulators
    + explicit mutable-resource ownership
    + future-sufficient history quotient
    + maximal-support cover
    + sound exact/epsilon bounds
    + fail-closed unsupported families
```

The review found close neighbors for every row individually, but no public source for the whole
column. The correct verdict is therefore:

```text
Broad routing algorithm:                 not novel
Individual optimization ingredients:    mostly not novel
Exact trace quotient theorem here:       novel application is plausible
Full simulator-bound architecture:       apparently novel combination
Universal first / patent novelty:        not established
Research-paper contribution:             yes, if claims remain narrow
```

## Sources

1. Guillermo Angeris, Tarun Chitra, Alex Evans, and Stephen Boyd, [“Optimal Routing for Constant Function Market Makers,”](https://arxiv.org/abs/2204.05238) 2022.
2. Theo Diamandis, Max Resnick, Tarun Chitra, and Guillermo Angeris, [“An Efficient Algorithm for Optimal Routing Through Constant Function Market Makers,”](https://arxiv.org/abs/2302.04938) 2023.
3. Carlos Escudero, Felipe Lara, and Miguel Sama, [“Optimal Routing across Constant Function Market Makers with Gas Fees,”](https://arxiv.org/abs/2603.02844) 2026.
4. Ilia Zhavoronkov, [“Multi-Path Routing in Decentralized Exchange Networks: Convex Allocation and an Improving-Path Certificate,”](https://arxiv.org/abs/2607.22540) 2026.
5. Uniswap Labs, [Smart Order Router](https://github.com/Uniswap/smart-order-router), especially [`alpha-router.ts`](https://github.com/Uniswap/smart-order-router/blob/main/src/routers/alpha-router/alpha-router.ts) and [`best-swap-route.ts`](https://github.com/Uniswap/smart-order-router/blob/main/src/routers/alpha-router/functions/best-swap-route.ts).
6. Balancer, [“Smart Order Router,”](https://github.com/balancer/docs-v2-archive/blob/v2/products/smart-order-router.md) archived documentation; [Balancer SOR repository](https://github.com/balancer/balancer-sor).
7. 0x, [“0x Smart Order Routing,”](https://0x.org/post/0x-smart-order-routing) 2020.
8. 1inch, [“Better swap rates in DeFi: how 1inch’s upgraded Pathfinder finds optimal routes,”](https://1inch.com/blog/post/new-pathfinder-algorithm-better-swap-rates) 2025, updated 2026.
9. Velora, [Market API v6.2](https://developers.velora.xyz/api/velora-api/velora-market-api/master/api-v6.2), [price endpoint](https://developers.velora.xyz/api/get-rate-for-a-token-pair), and [Augustus Swapper architecture](https://developers.velora.xyz/augustus-swapper).
10. CoW Protocol, [protocol documentation](https://docs.cow.fi/) and [services architecture](https://github.com/cowprotocol/services/blob/main/docs/ONBOARDING.md).
11. PropellerHeads, [Fynd](https://github.com/propeller-heads/fynd).
12. [US11580600B2, “Distributed crypto-currency smart order router with cost calculator,”](https://patents.google.com/patent/US11580600B2/en) filed 2018, granted 2023.
