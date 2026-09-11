# Lexion: routing as resource-constrained scheduling

## Status and scope

This note formalizes Lexion's bounded, pool-disjoint portfolio search implemented in
`exact_v2_search.rs`. It does not claim that the broader canonical flow-DAG design is implemented,
nor that the current simulator-backed allocator is globally exact.

The central result has two layers:

1. ordering histories that select the same compatible paths can be quotiented exactly;
2. searching only maximal compatible resource frontiers preserves the global optimum under an
   exact, zero-flow-closed amount optimizer.

The second theorem is conditional. Our current V3/V4 amount solver uses coordinate ascent, so the
production implementation does not yet meet its exact-optimizer premise.

## 1. Finite routing machine

Fix one quote request, market snapshot, gas price, candidate-path frontier, maximum route width
`k`, and deterministic pool simulators.

Let:

- `P` be the finite set of candidate paths;
- `R(p)` be the set of mutable pool components used by path `p`;
- `M` be the integer input amount;
- `k` be the maximum number of paths that may receive positive flow.

Two paths are compatible when they use disjoint mutable resources:

```math
p \perp r \iff R(p) \cap R(r) = \varnothing.
```

A structural scheduling state is:

```math
q=(A,U),
```

where `A` is the canonical unordered set of selected paths and
`U = union_{p in A} R(p)` is the occupied-resource set. The initial state is `(empty, empty)`.
The legal path-selection transition is:

```math
(A,U) --select(p)--> (A union {p}, U union R(p))
```

iff `p` is not in `A`, `R(p)` is disjoint from `U`, and `|A| < k` for ordinary subset search.

For a compatible portfolio `A`, its integer allocation domain is:

```math
Delta_A(M) = {x in N^A | sum_p x_p = M}.
```

Zero allocations are permitted mathematically. The active support is
`supp(x) = {p | x_p > 0}` and must satisfy `|supp(x)| <= k`.

Because admitted paths are pool-disjoint, each path can be replayed from the same market snapshot.
Let `Out_p(x_p)` and `Gas_p(x_p)` be the authoritative integer simulator results, with both zero
when `x_p = 0`. The score is:

```math
J_A(x) = sum_p Out_p(x_p) - PriceOut(sum_p Gas_p(x_p)).
```

The bounded routing problem is:

```math
OPT = max_{A compatible} max_{x in Delta_A(M), |supp(x)| <= k} J_A(x).
```

This is scheduling because a search policy chooses a legal ordering of `select` actions while
managing finite mutable resources, then schedules integer flow across the selected paths.

## 2. Trace equivalence

A selection history is a word `h = p_1 ... p_n`. Define its canonical observation:

```math
Phi(h) = (A(h), U(h)),
```

where `A(h)` is the unordered selected-path set and `U(h)` its resource union. Define:

```math
h_1 ~ h_2 iff Phi(h_1) = Phi(h_2).
```

This is an equivalence relation because equality of canonical observations is reflexive,
symmetric, and transitive. For pairwise-compatible paths it identifies every permutation of the
same selected set; a width-`n` portfolio has as many as `n!` ordered selection histories.

### Theorem 1: equivalent histories have equivalent feasible futures

If `h_1 ~ h_2`, then the two states admit the same legal continuation words and every shared
continuation has the same terminal allocation domain and scoring semantics.

#### Proof

`Phi(h_1) = Phi(h_2)` gives equal selected sets and occupied-resource sets. The legality of the
next `select(p)` action depends only on membership in `A`, disjointness from `U`, and the width
bound, all equal in the two states. Therefore the enabled actions are equal. Applying the same
enabled action produces equal successor observations. Induction over continuation length proves
equality of continuation languages. At termination the selected path set is equal, hence so are
the allocation domain, authoritative path simulators, gas calculation, and objective. QED.

### Corollary 1: trace-quotient search preserves the optimum

Searching one representative of each `~` class returns the same optimum as searching every legal
ordered path-selection history, because Theorem 1 gives identical feasible scored completions for
all representatives of a class.

## 3. Maximal resource-frontier cover

The implemented V4 optimization goes further than permutation quotienting. It enumerates maximal
compatible portfolios and optimizes one allocation vector over each portfolio, allowing unused
paths to receive zero flow.

Every compatible subset `A` of a finite candidate set is contained in at least one maximal
compatible portfolio `B`: repeatedly add any compatible path until none remains.

Define zero extension from `A` to `B`:

```math
(extend_B x)_p = x_p when p in A, otherwise 0.
```

### Theorem 2: maximal-frontier optimization preserves the optimum

Assume:

1. the amount optimizer returns the global maximum over each closed integer simplex;
2. zero-flow paths contribute zero output and zero gas;
3. path replay is separable because portfolios are pool-disjoint;
4. the optimizer permits at most `k` positive coordinates even when a maximal frontier is wider;
5. every candidate portfolio uses the same market snapshot and scoring function.

Then:

```math
max_{A compatible} max_{x in Delta_A(M)} J_A(x)
=
max_{B maximal compatible} max_{y in Delta_B(M), |supp(y)| <= k} J_B(y).
```

#### Proof

For any feasible `(A,x)`, choose a maximal compatible `B` containing `A`. Its zero extension `y`
is feasible in `B`, has the same active support, and assumptions 2 and 3 give `J_B(y)=J_A(x)`.
Thus the right side is at least the left side. Conversely, every allocation on a maximal `B` has
active support `A=supp(y)`, which is itself a compatible portfolio of width at most `k`; deleting
zero coordinates preserves its score. Thus the left side is at least the right side. QED.

This is best understood as a cover of all subset allocation faces by maximal simplices, not as an
equivalence relation between distinct nonzero allocations.

## 4. Implementation correspondence and gap

The implementation maps the formal objects as follows:

| Formal object | Implementation |
|---|---|
| candidate path `p` | `PathAllocation` before final reallocation |
| mutable resources `R(p)` | hop `component_id`s |
| compatibility | `allocation_paths_pool_disjoint` |
| maximal frontiers | `v4_compatible_portfolios(..., None)` |
| exhaustive control | `v4_compatible_portfolios(..., Some(max_paths))` |
| amount optimizer | `refine_disjoint_allocations` |
| authoritative score | exact simulator replay followed by gas conversion |

### Theorem-to-production assumption ledger

Theorem 2 is a conditional statement about a particular execution path, not a blanket property of
`find_disjoint_path_allocations`. Each premise maps to production code as follows:

| Theorem 2 premise | Production evidence | Status |
|---|---|---|
| 1. Global maximum over every admitted closed integer simplex | `certified_pair_branch_and_bound` exhausts or soundly bounds every integer split and exposes a certificate only as `CertifiedPairSolve::Complete` | **Established only for a completed supported two-path certificate.** `refine_simulated_paths` coordinate ascent, `Unsupported`, and `BudgetExceeded` are outside the theorem |
| 2. Zero-flow paths add zero output and zero gas | `zero_path` clears input, output, marginal state, and per-hop gas; `gas` excludes zero-input paths; completed allocations remove zero-input paths | **Established by representation and scoring** |
| 3. Portfolio paths replay independently | `v4_compatible_portfolios`, `allocation_paths_pool_disjoint`, and `paths_are_pool_disjoint` reject shared component IDs | **Established for admitted component-disjoint portfolios**, assuming component IDs completely identify mutable simulator resources |
| 4. At most `k` positive coordinates even when the maximal frontier is wider | Returned allocations are checked against `max_paths`; excess paths are removed and refinement is retried | **Output invariant established; global support-constrained optimum not established.** Greedy removal is not a proof-preserving optimizer |
| 5. One snapshot and scoring function | `discover_paths`, `refine_disjoint_allocations`, simulator replay, and `allocation_net_output` receive the same request-local `BellmanFordContext` / `market_data`; exact certified scoring uses `ExactGasValuation` derived from that context | **Established within one solve invocation** |

The maximal-frontier production path therefore has three distinct guarantee levels:

```text
maximal frontier + Complete exact allocator + zero cutoff
    -> Theorem 2 applies on the admitted family

maximal frontier + Complete exact allocator + positive cutoff
    -> Theorem 4's bounded-quality guarantee applies, not exact Theorem 2

maximal frontier + coordinate-ascent / unsupported / budget fallback
    -> useful heuristic result only; no maximal-frontier optimality claim
```

The current general V3/V4 path is predominantly the third case. Merely enumerating maximal
frontiers does not upgrade a heuristic amount optimizer into an exact one.

The V2 closed-form path is much closer to the theorem's optimizer premise. General V3/V4 paths,
however, use pairwise coordinate ascent. That is a deterministic useful optimizer, not a proof of
the global optimum over every maximal frontier. Consequently, maximizing each subset separately
can find a better local optimum than optimizing its maximal superset. The benchmark below observes
exactly this gap.

## 5. Controlled benchmark

`scripts/compare-v4-quotient.sh` keeps candidate discovery, candidate cap, market, order set,
simulators, gas model, route-width limit, and timeout fixed. It changes only:

- control: enumerate every compatible V4-containing subset of width at most `k`;
- quotient: optimize maximal compatible V4 resource frontiers and allow zero-flow paths.

On Ethereum block `25919677`, 500 historical orders, Uniswap V2/V3/V4, depth 2, one worker, and a
5-second per-order budget:

| Metric | Exhaustive subsets | Maximal-frontier quotient | Quotient delta |
|---|---:|---:|---:|
| solved | 484/500 | 484/500 | 0 |
| total solve time | 9.900 s | 4.104 s | -58.5% |
| median solve time | 8.066 ms | 7.006 ms | -13.1% |
| wins in direct comparison | 8 | 1 | — |
| exact ties | 475 | 475 | — |
| mean net-output delta | — | -0.00123 bps | — |

The quotient is 2.41 times faster in aggregate and almost output-equivalent, but it is not exactly
equivalent under the current heuristic V3/V4 allocator. This benchmark therefore supports the
state-space reduction claim and falsifies an unconditional production optimality claim.

The complete result is in
`artifacts/benchmarks/runs/v4-quotient-vs-exhaustive-500/report.md`.

## 6. Relationship to published work

A comprehensive claim-by-claim prior-art review is maintained in
[`routing-novelty-audit.md`](routing-novelty-audit.md). Its conclusion is deliberately narrower
than “a novel router”: nearly every individual ingredient has prior art, while the complete
resource-quotient, maximal-frontier, integer-simulator-certificate architecture appears to be a
distinct combination in the reviewed public literature.

| System/work | Published method | Relationship to this construction |
|---|---|---|
| Uniswap smart-order-router | Candidate pool/route enumeration, route splitting, and gas-adjusted selection across Uniswap protocols | Closest production SOR comparison; it establishes that split routing and gas-aware route selection are not new |
| Balancer SOR | Optimizes swaps across Balancer pools using pool-specific price functions and gas-aware routing | Prior art for amount allocation across heterogeneous pools |
| Angeris et al., *Optimal Routing for Constant Function Market Makers* | Expresses routing across CFMM networks as a convex optimization problem under concavity assumptions | Stronger global optimality theory on its admitted mathematical model; our simulator-backed bounded search covers opaque behavior that is not necessarily represented by that convex model |
| Diamandis et al., *An Efficient Algorithm for Optimal Routing Through Constant Function Market Makers* | Exploits routing-problem structure and decomposition to make convex routing practical | Closely related principle: use mathematical structure to reduce optimization cost rather than enumerate routes naively |
| Escudero et al., *Optimal Routing across Constant Function Market Makers with Gas Fees* | Adds fixed gas activation and KKT conditions under differentiable generalized-convex market models | Prevents claims that prior theory ignores gas or extends only to ordinary convexity; it still does not model arbitrary integer simulator transitions |
| Zhavoronkov, *Multi-Path Routing in Decentralized Exchange Networks* | Gas-aware candidate generation, concave allocation, and an improving-path certificate | Close prior art for certified omitted-path search, but its certificate relies on continuous concave allocation rather than exact opaque simulator replay |
| 0x Smart Order Routing | Samples increasing fill sizes, constructs fill-path DAGs, and merges gas-adjusted paths | Establishes prior art for discrete sampled routing and fill DAGs; no public resource-state quotient or integer certificate was identified |
| 1inch Pathfinder | Splits across liquidity, consolidates route steps, and optimizes concentrated-liquidity use and gas | Public detail is insufficient to exclude similar private internals, so first-of-kind claims must remain qualified |
| CoW Protocol | Competitive combinatorial batch auctions with executable solution simulation | A broader and different settlement problem; relevant combinatorial prior art, not a like-for-like single-order routing algorithm |
| Upstream Fynd | Bellman-Ford discovery plus Path Frank-Wolfe split allocation over simulated market state | Direct implementation baseline; the exact portfolio frontier, multi-scale preservation, and V4 maximal-resource cover are the added experimental layers here |

Primary references:

- [Uniswap smart-order-router](https://github.com/Uniswap/smart-order-router)
- [Balancer SOR](https://github.com/balancer/balancer-sor)
- [Optimal Routing for Constant Function Market Makers](https://arxiv.org/abs/2204.05238)
- [An Efficient Algorithm for Optimal Routing Through Constant Function Market Makers](https://doi.org/10.1007/978-3-031-47751-5_8)
- [Optimal Routing across Constant Function Market Makers with Gas Fees](https://arxiv.org/abs/2603.02844)
- [Multi-Path Routing in Decentralized Exchange Networks](https://arxiv.org/abs/2607.22540)
- [0x Smart Order Routing](https://0x.org/post/0x-smart-order-routing)
- [1inch Pathfinder](https://1inch.com/blog/post/new-pathfinder-algorithm-better-swap-rates)
- [CoW Protocol](https://docs.cow.fi/)
- [Upstream Fynd](https://github.com/propeller-heads/fynd)

## 7. Defensible contribution and next theorem obligation

The broad ideas of path discovery, split routing, exact replay, gas-aware scoring, sampled fill
DAGs, fixed-cost activation, certificates, and mathematical CFMM optimization have substantial
prior art. The narrower contribution supported here is:

> represent bounded V4 portfolio search as resource-constrained scheduling, quotient path-selection
> histories by canonical resource state, and cover compatible subsets by maximal resource
> frontiers before simulator-backed allocation.

The trace quotient is proved exactly. The maximal-frontier reduction is proved conditionally. To
make the production search satisfy the latter theorem, the remaining obligation is an allocator
with a global certificate on the admitted portfolio family, or a fallback that exhaustively checks
the unresolved subsets. Until then, describe the implemented V4 quotient as a measured
quality/latency tradeoff rather than an exact optimum-preserving transformation.

The defensible novelty category is therefore an **apparently novel combination/application**, not
a new general optimization primitive. A publication may say “to our knowledge” before claiming
the first combination of a future-sufficient resource quotient, maximal compatible-frontier cover,
and simulator-validated integer certificates for bounded V2/V3/V4 portfolios. It should not claim
the first optimal DEX router, the first gas-aware router, the first split router, or superiority to
convex routing on the convex model. Public descriptions of proprietary routers are incomplete, and
this literature review is not a patentability or freedom-to-operate opinion.

## 8. Allocation-certificate boundary

The per-portfolio objective is

```math
J_A(x)=\sum_{p\in A}Q_p(x_p)-G_A(x),
\qquad x_p\in\mathbb N,\quad \sum_{p\in A}x_p=M.
```

The exact-replay objective cannot currently be assumed concave. `CoupledConcavityAuditV1` evaluates
adjacent secant slopes on a 64-cell grid using the same integer simulator replay as allocation. On
Ethereum block `25921226`, a focused 50-order V2/V3/V4 run produced portfolio instances with
widespread positive second differences. One representative invocation observed 6,628 violating
pairs among 23,582 complete pool-disjoint pairs and 128,501 violating slope triples. Other orders
included sets where 498 of 501 complete pairs violated sampled concavity. The complete benchmark
artifact is `artifacts/benchmarks/runs/concavity-certificate-precheck-50/`.

This is a falsification of universal concavity, not a proof that every individual path family is
non-concave. Integer rounding, multi-hop composition, tick boundaries, simulator feasibility, gas
changes, and V4 behavior all remain visible in `J_A`. Consequently, ordinary continuous KKT
conditions cannot certify the shipped mixed-family allocator.

The implementation contract must classify portfolio families:

1. **Proved family.** Use a family-specific globally certified optimizer. The current example is
   the closed-form/certified-interval V2 path machinery.
2. **Bounded black-box family.** Use exact simulator replay with a sound regional upper bound and
   branch-and-bound. A region may be pruned only when its upper bound cannot beat the incumbent.
3. **Opaque family.** Coordinate ascent may produce a candidate, but the result is explicitly
   uncertified. Exactness requires either a simulator-supplied bound or exhaustive evaluation of
   the finite allocation domain.

Enumerating every compatible path subset does not by itself close the theorem: the current
"exhaustive" control is exhaustive over supports but still uses coordinate ascent over amounts.
For Theorem 2, every maximal frontier must either use a globally certified amount optimizer or
fall back to a complete amount search. Unsupported or opaque simulator families therefore remain
outside the theorem's exact domain.

### Existing monotone interval certificate

For a two-path allocation, let `x` be the amount assigned to the second path and let
`x in [l,h]`. The analysis implementation in `exact_v2_search.rs` already computes

```math
U([l,h])=Q_1(M-l)+Q_2(h).
```

For monotone exact-input paths this is sound because `M-x <= M-l` and `x <= h`. It does not
assume differentiability or concavity, so tick crossings and integer staircases do not invalidate
the inequality. Simulator failure produces `Unknown`, never a pruning decision. Ignoring gas in
the upper bound is also conservative because gas cost is non-negative.

This bound is sufficient for an exact branch-and-bound architecture, but two obligations remain
before production use:

1. The incumbent net score must use exact rational/integer gas conversion. `ExactGasValuation` is
   now threaded through every refinement entry point and computes certificate scores in integer
   output-token units. Coordinate ascent retains its `f64` score only as a candidate-generation
   heuristic; certified comparisons must use the exact representation.
2. The loose monotone rectangle bound must be subdivided until every surviving allocation point is
   evaluated or dominated. The earlier 16-cell envelope retained exhaustive agreement but cost
   more than exhaustive subset evaluation, so merely enabling it does not produce a practical
   exact solver.

### Budgeted pairwise branch-and-bound result

The first budgeted exact prototype in `exact_v2_refiner.rs` was deliberately restricted to
two-path portfolios whose hops were all standard V3 pools. It seeds an incumbent
with coordinate ascent, scores candidates with `ExactGasValuation`, bisects the complete integer
allocation interval, and prunes only with the sound monotone rectangle bound above. Its result is
explicitly one of:

```text
Complete(optimum)
Unsupported
BudgetExceeded
```

Neither `Unsupported` nor `BudgetExceeded` exposes a partial search result as certified. The first
prototype was analysis-only behind `FYND_CERTIFIED_PAIR_BRANCH_BOUND`; the completed supported
solver described below now runs normally and retains the same safe fallback outcomes.

The controlled 50-order live-market experiment at block `25921309` shows that this first bound is
not practical. `path_frank_wolfe_multiscale_d2` took 9.397 seconds total, with 155.264 ms median,
482.537 ms p95, and 909.783 ms maximum solve time. The Bellman-Ford comparison took 65 ms total.
The trace contained many unsupported mixed-family portfolios and many pure-V3 searches ending in
`BudgetExceeded`; the 16,384-interval budget prevented partial certificates from escaping.

This is a useful negative result. Exact integer branch-and-bound is now implemented with the right
safety boundary, but the endpoint-sum rectangle bound remains too loose over realistic token
amount domains. Raising the budget would primarily buy more subdivision, not a structural speedup.

### Interval-local V3 tangent certificate

The tighter prototype derives an exact rational terminal marginal rate from each V3 hop's
post-swap square price and fee. For an interval `x in [lo, hi]`, it replays the left path at its
minimum flow `M-hi` and the right path at its minimum flow `lo`. Exact-input V3 execution moves the
price against the trader, so each terminal marginal rate upper-bounds all additional output after
that base point, including across later initialized ticks. These rational hop envelopes compose
over a path. Their sum is affine in `x`, so its interval maximum is one of the two endpoints.

The certificate also subtracts a lower bound on interval gas using those minimum-flow replays and
the exact gas valuation. This removes the permanent gross-versus-net gap that previously preserved
large near-optimal plateaus. Integer output arithmetic rounds against the trader; the final
rational envelope is rounded upward once.

On the final 50-order live-market run at block `25921721`, after aligning zero-flow gas scoring,
the result was:

```text
pure-V3 exact certificates complete: 1
pure-V3 budget exceeded:             0
mixed/unsupported refinements:       234
total solve time:                     423 ms
median solve time:                    8.640 ms
p95 solve time:                       17.673 ms
maximum solve time:                   39.813 ms
```

The original monotone-rectangle run took 9.397 seconds for the same 50-order workload shape and
ended observed pure-V3 searches at the interval budget. The live blocks differ, so this is not a
cycle-stable microbenchmark, but the status change from `BudgetExceeded` to `Complete` establishes
that the local tangent removes the structural search failure. Mixed V2/V3/V4 portfolios remain
outside this certificate and continue to use the heuristic allocator.

`V3TangentCertificateFalsifierV1` exhaustively checks every allocation point and every subinterval
of a 32-unit domain across both swap directions, asymmetric liquidity, five V3 fee tiers, and
initialized-tick crossings. A second fixture checks two competing two-hop paths over a 16-unit
domain, including composition across four independently evolving V3 pools. Both fixtures check
gross output and exact gas-valued objectives, require every calculated interval upper bound to
dominate the exhaustive maximum, and require the final branch-and-bound score to equal the
exhaustive optimum. The current matrix covers 18 exact objectives, 562 allocation points, and
9,282 interval certificates. A reproducible 24-case pseudo-random matrix adds independently varied
liquidity, fee tier, direction, and exact gas valuation over a 12-unit domain. In total the tests
now cover 42 exact objectives, 874 allocation points, and 11,466 interval certificates with zero
violations.

The gas-valued falsifier exposed and fixed a separate consistency bug: zero-flow paths retained
their simulated gas while candidates were scored, even though those paths were removed from the
emitted allocation. Gas scoring now ignores zero-flow paths, so the certified objective and final
allocation use the same semantics.

### Mixed V2/V3 completion

The same local-tangent search now admits each two-path portfolio where every individual path is
either an all-V2 path or an all-V3 path. An all-V2 path obtains its exact terminal derivative from
the composed constant-product curve; an all-V3 path obtains it from the replayed terminal pool
states. This covers V2/V3, V2/V2, and V3/V3 pairings without treating a mixed-hop path as a family
we have proved. Pure V2 portfolios still take the existing specialized closed-form allocator.

The mixed-family falsifier covers V2 on either side, both swap directions, and gross and exact
gas-valued objectives. Adding it brings the exhaustive matrix to 50 objectives, 1,138 allocation
points, and 15,954 interval certificates with zero violations or optimum mismatches. It also found
that V2 rejects zero-input simulation: endpoint allocations now materialize an explicit zero path
without invoking any pool simulator, giving every supported family the same endpoint semantics.

The exact pair solver now runs by default after coordinate ascent for supported pair families.
`Unsupported` and `BudgetExceeded` retain the coordinate-ascent result. A 50-order live run at
block `25924433` completed its one admitted exact pair, reported no budget exhaustion, and solved
all orders in 840 ms total (19.354 ms median, 30.906 ms p95, 35.022 ms maximum). The other 263
refinement attempts were outside the certified family and fell back.

## 9. Simulator-bound semantics

The routing machine is exact only relative to a fixed market snapshot and the protocol simulators
used to evaluate it. For a path `p` and input `x`, `Out_p(x)` is not an analytic approximation: it
is the integer output obtained by replaying the path through those simulators. Likewise, a route is
ranked by the same gas-adjusted semantics used to construct the returned allocation.

This gives the following boundary:

```math
Exact_{router}
=
Exact_{search\ over\ the\ modeled\ transition\ system},
```

not a claim that an incomplete or stale market snapshot represents every executable on-chain
possibility. Candidate discovery may omit pools or paths; a simulator may fail to model arbitrary
V4 hook behavior; and the state may change before execution. Search certificates therefore prove
statements about the captured transition system. Execution-time protections such as slippage
limits remain necessary.

### Admitted protocol families

“Supported by the router” and “admitted by a certificate” are different properties. The current
implementation has the following nested domains:

| Layer | Admitted family | Excluded family | Meaning of exclusion |
|---|---|---|---|
| Market ingestion | `uniswap_v2`, `uniswap_v3`, ordinary `uniswap_v4`, and simulated `uniswap_v4_hooks`, except explicitly blocked hook addresses | Hook addresses in `BLOCKED_UNISWAP_V4_HOOKS` and ordinary component blocklists | The component is not placed in the routing market |
| Heuristic route replay | Any ingested path for which every hop simulator succeeds | Simulation failure or unavailable state | The path is not a usable candidate for that replay |
| Full-input / finite-input outer bound | Paths composed entirely of `UniswapV2State`, `UniswapV3State`, or passive `UniswapV4State` | Every V4 state with a nonzero hook handler; every other simulator family | No certified frontier bound is returned; normal refinement may still run |
| Exact pair allocation certificate | Exactly two pool-disjoint paths, each either representable by the V2 constant-product curve or composed entirely of V3 hops | V4, mixed-hop V2/V3 paths not represented by one proved family, arbitrary hooks, other protocols, wider portfolios | Returns `Unsupported`; coordinate ascent remains an explicitly heuristic fallback |
| Maximal-frontier Theorem 2 | Any component-disjoint family satisfying all five theorem premises, including a globally exact allocator | Every production invocation lacking one of those premises | The theorem does not apply even if the implementation still returns a quote |

#### Exact definition of passive V4

For the current outer certificate, a V4 hop is passive exactly when its simulator state satisfies:

```rust
state.hook.as_ref().is_none_or(|hook| hook.address() == Address::ZERO)
```

This is implemented by `passive_uniswap_v4_state` and used by `standard_uniswap_path`. It is a
conservative representation-level predicate. It does not attempt to inspect hook permission bits
and prove that one particular nonzero hook is harmless. Consequently, even a nonzero hook that in
practice performs no swap-affecting action remains outside the certificate until it receives a
separate proved capability classification.

“Passive V4” therefore means the ordinary V4 concentrated-liquidity transition with protocol and
LP fees already represented in `UniswapV4State`, tick crossing, integer rounding, and no active
nonzero hook handler. It does **not** mean arbitrary V4.

#### Why every nonzero hook is currently unsupported by certificates

The hook simulator interface permits behavior that the current proof does not bound:

1. `beforeSwap` may change the specified amount through a returned delta;
2. `beforeSwap` may override the LP fee as a function of input or state;
3. `beforeSwap` and `afterSwap` may change the trader's final balance delta;
4. hook execution adds input- and state-dependent gas;
5. hook storage and transient state may change between or during simulations;
6. hook-defined amount ranges or failures may make the feasible domain discontinuous;
7. a hook may depend on or mutate state not uniquely identified by the V4 pool component ID,
   invalidating the resource-disjoint separability premise.

The full-input bound requires, for every path and every `0 <= x <= M`,

```math
Q_p(x)\le Q_p(M).
```

An arbitrary hook has not been proved to satisfy this monotonicity law. More subtly, the trace
quotient and maximal-frontier proof require the occupied component-ID set to contain every mutable
resource affecting future replay. A hook that shares external state across nominally distinct
pools can violate that condition even if its output happens to be monotone. Rejecting all nonzero
hooks from certification protects both premises.

This rejection is fail-closed:

```text
nonzero V4 hook
    -> may remain available to ordinary simulator-backed routing
    -> `standard_uniswap_path` is false
    -> outer certificate returns no bound
    -> no pruning decision may be justified by that certificate
```

It is not a claim that every hook is unsafe or non-monotone. It means the current certificate has
no proof for that hook.

#### Feed blocklist is not the certificate boundary

`BLOCKED_UNISWAP_V4_HOOKS` is an operational ingestion blocklist for hook implementations with
known simulation problems. It removes those pools entirely. Conversely, the certificate boundary
rejects **all** nonzero hook handlers, including hooks that remain ingested and simulator-routable.
Passing the feed filter therefore does not imply certificate admission.

Future hook admission must be capability-based and proof-carrying. At minimum, a hook family would
need independently established monotone exact-input output, a complete mutable-resource footprint,
deterministic replay on the captured state, a sound gas lower bound, and closure of its feasible
input domain under the certificate's interval operations. An allowlist by address alone would not
establish those properties.

This separation is useful. Topology enumeration and allocation may use symbolic curves, marginal
envelopes, and bounds, but every accepted incumbent is still judged by authoritative integer
replay. A symbolic model may remove work only when it is a proved over-approximation of that
replay on its admitted protocol family.

## 10. Sacred-timeline branch and bound

Call a partial compatible portfolio-selection history a *timeline*. Let `B` be the best exact net
output already replayed, and let `U(T)` be a sound upper bound on the net output of every completion
of timeline `T`:

```math
\forall C\in Completions(T),\qquad J(C)\le U(T).
```

### Theorem 3: exact timeline elimination

If `U(T) <= B`, removing `T` and all of its descendants cannot remove a strict improvement over
the incumbent.

#### Proof

Every completion `C` of `T` satisfies `J(C) <= U(T) <= B`. Therefore no descendant of `T` can
replace `B` under strict-improvement selection. QED.

The currently implemented V4 pre-refinement certificate applies to pool-disjoint portfolios whose
paths consist of standard V2/V3 pools and passive V4 pools. In the current simulator interface, a
V4 pool is admitted only when it has no hook handler or its hook address is zero. Arbitrary nonzero
hooks are `Unsupported`, because a hook can invalidate monotonicity or add state-dependent behavior
not represented by the certificate.

For an admitted frontier `A`, let `Q_p(M)` be the already replayed gross output of path `p` when it
receives the entire order `M`. Every feasible allocation has `0 <= x_p <= M`. Monotonic exact-input
output gives:

```math
\sum_{p\in A} Q_p(x_p)
\le
\sum_{p\in A} Q_p(M).
```

Thus

```math
U_{gross}(A)=\sum_{p\in A}Q_p(M)
```

is a sound, deliberately loose bound. Comparing this gross bound with the net incumbent is safe
because omitted gas is non-negative. It may retain losing frontiers, but it cannot prune a true
winner within the admitted monotone family.

The amount allocator uses the tighter interval certificates described in Section 8. Each interval
is either proved unable to beat the incumbent, subdivided, evaluated exactly at a singleton, or
reported as `Unsupported`/`BudgetExceeded`. A partial result from either failure outcome is never
presented as certified.

### Approximate quality cutoff

Exact pruning uses a zero tolerance. A separately configured approximate policy may decline work
whose *maximum possible* improvement is economically negligible. For a relative tolerance
`delta >= 0`, prune `T` when:

```math
U(T) \le B(1+\delta).
```

Equivalently, in basis points:

```math
PotentialBps(T)=10{,}000\left(\frac{U(T)}{B}-1\right)
```

and the timeline remains active only when `PotentialBps(T)` exceeds the selected cutoff.

### Theorem 4: bounded-quality timeline elimination

Assume every upper bound is sound, the incumbent is monotonically non-decreasing, and a timeline
is pruned only under `U(T) <= B_t(1+delta)` for the incumbent `B_t` at that time. If the final
incumbent is `B_f` and the true modeled optimum is `OPT`, then:

```math
OPT \le B_f(1+\delta),
```

or equivalently:

```math
B_f \ge \frac{OPT}{1+\delta}.
```

#### Proof

If an optimal completion is explored, `B_f=OPT`. Otherwise it belongs to a timeline pruned when
`OPT <= U(T) <= B_t(1+delta)`. Since incumbents only improve, `B_t <= B_f`, hence
`OPT <= B_f(1+delta)`. QED.

This theorem makes the policy explicit: `delta=0` preserves the exact optimum on the certified
domain; a positive `delta` buys latency with a quantified modeled-quality guarantee. A proposed
`0.1` bps cutoff is only an experimental candidate, not yet a production default.

## 11. Five-hundred-order live-market census

The `sacred-timeline-500` run used Ethereum block `25926009`, 2,379 captured V2/V3/V4 components
with at least 10 ETH TVL, 500 eligible historical orders from a 9,973-order dataset, depth two, one
order in flight, one worker, and a five-second per-solve timeout. The comparison baseline was
`bellman_ford_d2`.

### Quote quality and coverage

| Metric | Bellman-Ford | Path Frank-Wolfe multiscale | Delta |
|---|---:|---:|---:|
| solved | 482/500 | 482/500 | 0 |
| compared | — | 482 | — |
| exact-output wins | — | 114 | — |
| exact-output losses | — | 0 | — |
| exact ties | — | 368 | — |
| mean output delta | — | +14.0 bps | — |
| median output delta | — | +0.0 bps | — |

Both configurations reported the same 18 orders as `no route`. The improved solver therefore won
on 23.7% of jointly solved orders and did not lose on this sample. The zero median and pair-level
outliers are essential context: `+14.0` bps is not the gain of a typical order. For example,
`USDT -> USDe` averaged `+424.7` bps over four orders, while the busiest WETH/stablecoin pairs were
mostly within `0.0` to `0.4` bps.

### Latency

| Metric | Bellman-Ford | Path Frank-Wolfe multiscale | Ratio |
|---|---:|---:|---:|
| total | 306 ms | 27,377 ms | 89.5x |
| p50 | 436 us | 9,081 us | 20.8x |
| p95 | 1,155 us | 74,798 us | 64.8x |
| maximum | 7,821 us | 2,476,371 us | 316.6x |

The quality improvement is real on the captured sample, but this configuration is not yet a
latency-competitive replacement for the baseline. The extreme tail also matters independently of
the average: one solve consumed approximately 2.48 seconds.

### Structural and allocation census

Across the 482 solved multiscale runs, instrumentation recorded:

| Counter | Value |
|---|---:|
| prefix states visited | 6,370 |
| path extensions considered | 7,184 |
| complete/canonical portfolios | 5,888 |
| equivalent ordered histories | 31,917 |
| history quotient compression | 5.42x |
| gross-bound certified-dead portfolios | 810 |
| maximal V4 frontiers | 1,799 |
| V4 refinement attempts | 1,765 |
| V4 refinement successes | 1,663 |
| V4 incumbent improvements | 265 |
| V4 losing frontier results | 1,380 |
| V4 frontiers pruned before refinement | 52 |
| V4 refinement time | 3.420 s |

The outer passive-V4 gross bound eliminated only 52 of 1,799 frontiers, approximately 2.9%.
Although the trace quotient removed substantial history duplication, most surviving frontiers
still entered amount refinement and most completed refinements did not improve the incumbent.

The allocation-level census recorded:

| Counter | Value |
|---|---:|
| allocator invocations | 5,736 |
| intervals examined | 258,809 |
| certified-bound pruned intervals | 118,601 |
| interval pruning fraction | 45.8% |
| envelope simulator replays | 517,590 |
| exact singleton replays | 20,532 |
| unsupported invocations | 5,264 |
| measured branch-and-bound time | 20.072 s |

The dominant engineering failure is entering expensive certificate machinery before discovering
that the portfolio is unsupported: 91.8% of allocator invocations ended `Unsupported`. The
interval bound itself is useful where admitted, rejecting 45.8% of examined intervals, but that
does not compensate for performing setup or partial analysis on thousands of ineligible calls.

These timers are instrumentation totals and may cover nested work; they should not be added as if
they were disjoint profile buckets. They nevertheless identify the allocation layer, and
especially unsupported entry, as the next controlled target.

## 12. Next controlled experiment

The next implementation step is a constant-time capability predicate evaluated before certified
allocation. It must inspect the complete path family and required materialization mode without
running interval search:

```text
portfolio
    -> certificate family fully supported?
       -> no: use the existing heuristic allocator immediately
       -> yes: enter certified branch and bound
```

This changes neither candidate discovery nor quote semantics. Its required differential property
is byte-for-byte/allocation-for-allocation equality with the current solver on the same snapshot,
while avoiding the work associated with the 5,264 unsupported invocations.

The analysis-only sacred-timeline cutoff sweep now evaluates these cutoffs counterfactually during
one unchanged production search:

```text
0, 0.01, 0.1, 0.5, and 1.0 bps.
```

For each threshold, retain the same orders, market snapshot, candidates, timeouts, and simulator
semantics, and report:

- orders whose final allocation or exact output changes;
- maximum and aggregate modeled quality loss;
- valuable incumbent improvements lost;
- timelines and simulator replays avoided;
- total, p50, p95, p99, and maximum solve latency;
- runtime saved per basis point sacrificed.

Each threshold maintains its own monotonically improving incumbent. The normal solver still
refines every frontier it would have refined without the experiment; the analysis records whether
each counterfactual incumbent would have admitted that frontier, then conditionally incorporates
the exact replayed result into that scenario. This makes all thresholds share one market capture
without allowing the experiment to alter returned routing output. Measured refinement durations
also provide the directly avoidable refinement time for each scenario. End-to-end hypothetical
latency remains an estimate until a chosen cutoff is enabled in a controlled A/B run.

The zero-bps configuration tests exact certified pruning. Positive thresholds test Theorem 4's
explicit quality/latency tradeoff. No positive cutoff should become a default merely because it
looks small; the sample's zero median and large outliers show why the complete loss distribution
must choose the policy.

## 13. Cutoff-sweep result

The first counterfactual sweep ran on Ethereum block `25930732` with 2,437 captured components,
the same 500-order dataset shape, and the same V2/V3/V4 configuration as Section 11. The multiscale
solver solved the same 485 orders as Bellman-Ford, improved 149 exact outputs, lost none, and tied
336. Its mean improvement was `+11.2` bps and its median remained zero. Total multiscale solve time
was 10.516 seconds, with 9.004 ms p50, 96.063 ms p95, and 304.821 ms maximum latency. Because this
is a new live block, those timings must not be treated as a controlled before/after comparison with
the earlier captures.

The cutoff analysis produced:

| cutoff | supported | pruned | refinements avoided | changed orders | mean loss | maximum loss |
|---:|---:|---:|---:|---:|---:|---:|
| 0 bps | 3,433 | 78 | 0 | 0 | 0 bps | 0 bps |
| 0.01 bps | 3,433 | 83 | 5 | 0 | 0 bps | 0 bps |
| 0.1 bps | 3,433 | 86 | 8 | 0 | 0 bps | 0 bps |
| 0.5 bps | 3,433 | 98 | 20 | 0 | 0 bps | 0 bps |
| 1.0 bps | 3,433 | 104 | 26 | 0 | 0 bps | 0 bps |

The production-zero row already skips the 78 exactly dominated frontiers, so it reports no
additional avoided refinements. Positive thresholds safely preserved the observed final output,
but the extra 5--26 avoided calls all returned `Unsupported` nearly immediately in the control
run. Their individually measured refinement bodies rounded below one microsecond, so this sweep
does not establish a meaningful latency win from the positive cutoff.

This is an informative negative result. The full-input gross bound is too loose on the expensive
losing frontiers. Raising the acceptable quality loss cannot help when their optimistic upper
bounds remain far above the incumbent. The next bound must become more discriminative, rather than
the cutoff becoming more permissive. Candidate directions are allocation-aware path envelopes,
frontier-level marginal/tangent bounds, or another sound simulator-derived upper bound that is
tight specifically on the 1,279 losing V4 results. Any replacement must first retain the zero-bps
soundness property before the positive-threshold policy is reconsidered.

## 12. Controlled external-router comparison

An external comparison must distinguish route quality from execution-gas accounting and from API
latency. We therefore compare executable V2/V3/V4 transactions on one immutable Ethereum state.
The local router quote is replayed at its recorded block, Uniswap's `AlphaRouter` is restricted to
the same block, and both transactions are passed through `eth_estimateGas` on an Anvil fork of that
block. V2/V3 routes use SwapRouter02; V4 routes use Universal Router with corresponding Permit2
allowance state. The same sender, injected ERC-20 balance and allowance, and output-token gas
conversion are applied to both sides. This replaces the earlier asymmetric comparison, which
combined different gas estimators and therefore cannot establish net quote quality.

A September 10, 2026 pilot at block `25944963` produced two comparable orders out of three sampled
orders. Lexion won both gross-output comparisons. After normalized execution gas, each router won
one. The first order favored Lexion by `0.0194` bps gross but Uniswap by `0.4583` bps net because the
Lexion transaction was estimated at `303,788` gas versus `214,701`. The second favored Lexion by
`60.0383` bps gross and `60.0712` bps net, with estimates of `409,928` and `434,957` gas.

This pilot validates the measurement method, not a population-level superiority claim. Its sample
is too small. It also measures a large systems-level latency difference in this environment:
Lexion returned the two successful quotes in `5 ms` and `4 ms`, while the corresponding Uniswap
pipeline calls took `150.1 s` and `74.6 s`. Those figures include AlphaRouter's external data and
quote-provider work and therefore do not isolate its in-process search algorithm. The complete
machine-readable artifacts are
`artifacts/benchmarks/comparisons/uniswap/lexion-vs-uniswap-pinned-3.json` and
`artifacts/benchmarks/comparisons/uniswap/lexion-vs-uniswap-pinned-3-uniswap-normalized.json`.

For user-facing comparisons, a normalized-net advantage whose approximate USD value is at most
`$0.02` is classified as an economic tie. The raw integer, gas-normalized, bps, and approximate-USD
deltas remain in the artifact. If the output token has no configured USD price, the order is marked
`unpriced` and excluded from the economic scoreboard rather than silently treated as a win.

### 12.1 Three-block V2/V3/V4 comparison

A larger run sampled the same 30 seeded orders at captured Ethereum blocks `25945688`, `25945692`,
and `25945701`. Requested block numbers acted only as sampling triggers: because the Ethereum RPC
and Tycho market stream advance asynchronously, the actual captured block hash is authoritative.
Both executable transactions were always replayed and gas-estimated on an Anvil fork of that exact
captured block.

The run used one order in flight, the default deterministic sampling seed (`42`), the recorded
10,000-order trade dataset, two-hop routing, and Uniswap V2/V3/V4. It is reproduced with:

```text
./scripts/compare-uniswap-blocks.sh \
  --blocks +5,+10,+15 \
  --orders 30
```

Of 90 order-block observations, 36 were directly comparable, with 12 comparable observations at
each block. Forty-five had no successfully replayed local quote. Nine more were excluded by the
external comparison layer: three lacked token metadata and six had no executable Uniswap route.
These are coverage failures and are not counted as wins for either router.

| score over 36 comparable observations | Lexion | tie | Uniswap |
|---|---:|---:|---:|
| replayed gross output | 24 | 9 | 3 |
| normalized net output after measured gas | 24 | 0 | 12 |
| economic score after the `$0.02` cutoff | **24** | **11** | **1** |

The materiality cutoff changes the interpretation substantially. Eleven nominal differences were
worth at most two cents and become ties. Ten of those were nominal Uniswap wins; only one Uniswap
advantage remained economically material, at approximately `$0.1046`. The absolute value of all
discarded differences was approximately `$0.1395` across the 36 observations.

| captured block | comparable | Lexion economic wins | ties | Uniswap economic wins | Lexion winning value | Uniswap winning value |
|---:|---:|---:|---:|---:|---:|---:|
| `25945688` | 12 | 8 | 4 | 0 | `$241.6391` | `$0.0000` |
| `25945692` | 12 | 8 | 3 | 1 | `$212.3610` | `$0.1046` |
| `25945701` | 12 | 8 | 4 | 0 | `$190.2677` | `$0.0000` |

Summing observations gives approximately `$644.2678` of Lexion-favoring output and `$0.1046` of
Uniswap-favoring output, a signed observational difference of approximately `$644.1632`. This is
not a claim that a user earned `$644`: the same seeded orders were re-evaluated at three nearby
blocks, so the total counts repeated opportunities through time. Per block, the signed difference
was approximately `$241.64`, `$212.26`, and `$190.27`.

The dollar result is concentrated. The repeated WBTC-to-USDC observation contributes about
`$421.07`, and WETH-to-USDT contributes about `$163.44`; together they account for roughly `90.7%`
of Lexion's measured winning value. Those routes therefore require individual pool-state and
execution inspection before the aggregate can support a broad production-quality claim. Dollar
conversion is exact for stablecoin outputs and approximate for configured volatile-token prices.

V4 is materially exercised on the Lexion side: 24 of the 36 comparable local routes contain at
least one Uniswap V4 leg. The Uniswap SOR selected its V2/V3/mixed candidate in all 36 comparable
cases; its independently queried V4 family did not produce the selected executable transaction.
This result therefore measures the benefit of permitting V4 in the shared market, but it does not
show a head-to-head V4-route win by Uniswap.

For the same 36 comparable observations, Lexion's measured quote latency was `28.5 ms` median,
`107 ms` p95, and `113 ms` maximum. The complete AlphaRouter pipeline measured `32.93 s` median,
`101.36 s` p95, and `109.33 s` maximum. These timings are end-to-end for the tested systems:
AlphaRouter's figures include external pool discovery, quote-provider, subgraph, and RPC work, so
they must not be interpreted as isolated in-process algorithm runtimes.

The machine-readable results are stored in
`artifacts/benchmarks/comparisons/uniswap/by-block/lexion-vs-uniswap-block-25945688-uniswap-normalized.json`,
`artifacts/benchmarks/comparisons/uniswap/by-block/lexion-vs-uniswap-block-25945692-uniswap-normalized.json`,
and
`artifacts/benchmarks/comparisons/uniswap/by-block/lexion-vs-uniswap-block-25945701-uniswap-normalized.json`.

### 12.2 CoW Protocol and ParaSwap comparison

A controlled 30-order run compared the same Lexion V2/V3/V4 instance with the public CoW Protocol
and ParaSwap v6.2 Market APIs. The orders were sampled with seed `20260910` from 3,038 historical
orders whose tokens are recognized by all three systems, spread across three consecutive market
snapshots with one order in flight. This restriction is important: a broader random run contained
many long-tail tokens absent from ParaSwap's token registry and would have confused token coverage
with routing quality.

The comparison uses three different execution products. Lexion and ParaSwap return market-router
quotes; CoW returns a solver-settled intent whose `buyAmount` is already the user's output after
CoW's fee. CoW's settlement gas is therefore not charged to the user a second time. ParaSwap's
reported `gasCost` is normalized using the output-token gas price inferred from Lexion's raw/net
quote pair. Unlike the Uniswap experiment above, the public CoW and ParaSwap APIs cannot be pinned
to and replayed against Lexion's exact captured block. Requests were concurrent, but this remains a
near-simultaneous API comparison rather than an immutable-state execution proof.

| metric | Lexion | CoW Protocol | ParaSwap |
|---|---:|---:|---:|
| successful quotes | 29/30 | 28/30 | 29/30 |
| directly comparable | — | 28 | 29 |
| gross wins / ties / losses for Lexion | — | 23 / 0 / 5 | 4 / 1 / 24 |
| reported-gas net wins / ties / losses for Lexion | — | 23 / 0 / 5 | 1 / 0 / 28 |
| median net difference, Lexion-relative | — | `+3.67 bps` | `-0.82 bps` |
| mean net difference, Lexion-relative | — | `+56.40 bps` | `-2.53 bps` |
| quote latency p50 | `29 ms` | `1,585 ms` | `718 ms` |
| quote latency p95 | `58 ms` | `6,773 ms` | `5,381 ms` |

Under the same approximate `$0.02` materiality rule used above, Lexion recorded 23 material wins
and five losses against CoW. The sum of Lexion-favoring differences was approximately `$24.37`,
while CoW's five favorable differences summed to approximately `$43.73`. Thus Lexion won more
orders, but not the aggregate dollar scoreboard; a small number of larger CoW wins dominated the
sample.

Against ParaSwap, four differences were worth at most two cents and became ties. ParaSwap retained
25 economically material wins, worth approximately `$46.65` in aggregate, while Lexion retained no
material win. The raw-output result points the same way: ParaSwap led 24 of 29 comparable orders,
with one exact tie. This is evidence that the current Lexion configuration is latency-first relative
to ParaSwap, not evidence that it has superior quote quality.

The latency result is large but system-level. Lexion's median was roughly `55x` lower than CoW's and
`25x` lower than ParaSwap's in this run. CoW and ParaSwap timings include public-network/API work;
Lexion ran locally against a continuously maintained market model. These figures therefore measure
the experienced quote pipelines, not isolated routing-algorithm runtimes.

The machine-readable artifact is
`artifacts/benchmarks/comparisons/lexion-cow-paraswap/common-30.json`. ParaSwap integration follows
the documented v6.2 [`/prices`](https://developers.velora.xyz/api/get-rate-for-a-token-pair)
endpoint. A larger repeated-block campaign and executable ParaSwap transaction replay are required
before treating the 30-order quality result as a production ranking.

#### Five-snapshot replication

The same seeded 30-order common-token workload was repeated across five fresh snapshots. The
replication produced 29 directly comparable CoW observations and 28 directly comparable ParaSwap
observations. It did not erase ParaSwap's advantage.

| replication metric | Lexion vs CoW | Lexion vs ParaSwap |
|---|---:|---:|
| gross wins / ties / losses for Lexion | 26 / 0 / 3 | 7 / 0 / 21 |
| reported-gas net wins / ties / losses for Lexion | 23 / 0 / 6 | 1 / 0 / 27 |
| median net difference, Lexion-relative | `+3.67 bps` | `-3.01 bps` |
| mean net difference, Lexion-relative | `+56.40 bps` | `-8.15 bps` |
| Lexion latency p50 / p95 | `30 / 83 ms` | `30 / 83 ms` |
| external latency p50 / p95 | `1,626 / 9,638 ms` | `593 / 8,875 ms` |

After the `$0.02` materiality rule, Lexion had 23 wins against CoW worth about `$36.43`; CoW had
six worth about `$49.89`. Lexion had one material win against ParaSwap worth about `$4.33`, while
ParaSwap had 26 worth about `$94.75`. The largest ParaSwap-favoring observation was WETH-to-WBTC at
approximately `$58.68`, so aggregate value remains sensitive to a large counterexample, but the
direction is not explained by that observation alone: ParaSwap still won 25 other material cases.

Across the two campaigns, the economic census is 46 Lexion wins versus 11 CoW wins, but the summed
favorable differences are approximately `$60.80` for Lexion and `$93.62` for CoW. Against ParaSwap,
the combined census is one Lexion win, four economic ties, and 51 ParaSwap wins, with approximately
`$4.33` versus `$141.40` of favorable differences. Repeated seeded orders at different times are
observations, not independent user profit, so these totals quantify persistent quote gaps rather
than realizable earnings.

The replication artifact is
`artifacts/benchmarks/comparisons/lexion-cow-paraswap/common-30-5-blocks.json`. The first attempted
replication was discarded after the Ethereum RPC timed out and terminated the local market process.

#### Equal routing-universe control

The unrestricted ParaSwap comparison confounds search quality with market coverage. Lexion was
configured with only Uniswap V2, V3, and V4, while ParaSwap could use its full exchange and RFQ
universe. A third run therefore passed ParaSwap's documented
`includeDEXS=UniswapV2,UniswapV3,UniswapV4` restriction and otherwise retained the seeded
common-token workload and five-snapshot protocol.

| equal-universe metric | Lexion | tie | ParaSwap |
|---|---:|---:|---:|
| gross output | 14 | 1 | 14 |
| reported-gas net output | 2 | 0 | 27 |
| economic result after `$0.02` cutoff | 0 | 9 | 20 |

The gross-output result is symmetric: once both systems see the same Uniswap families, neither
dominates route output by order count. Gas normalization changes the result sharply. ParaSwap has
20 economically material wins worth approximately `$15.31`; Lexion has no material win, and nine
differences become economic ties. The largest remaining observation is WETH-to-WBTC at about
`$11.15` in ParaSwap's favor.

Relative to the unrestricted five-snapshot result, ParaSwap's favorable dollar total falls from
about `$94.75` to `$15.31`, a reduction of approximately `$79.45` or `83.8%`. This attributes most
of the original deficit to the broader routing universe. The residual same-universe deficit is
mainly an execution-cost problem: gross routing is tied 14-to-14, but ParaSwap reports lower gas
often enough to win 27 of 29 net comparisons. Because ParaSwap transaction calldata was not replayed
on the captured state, this gas conclusion remains based on its reported `gasCost`; executable
same-state validation is the next necessary control.

Latency remains in Lexion's favor: `26 ms` p50 and `69 ms` p95 versus ParaSwap's `493 ms` p50 and
`5,456 ms` p95. The equal-universe artifact is
`artifacts/benchmarks/comparisons/lexion-cow-paraswap/paraswap-uniswap-only-30.json`.

#### Executable same-state gas verification

The reported-gas result above does not survive executable validation. The benchmark client was
extended to request ParaSwap v6.2 `/swap` calldata, use the same synthetic sender for both routers,
inject its input-token balance and allowance, and replay both transactions at Lexion's captured
block. Only pairs for which both transactions returned an observable output and measured gas are
scored.

Twenty-eight of the 30 seeded orders produced a valid replay pair. Under measured execution:

| executable equal-universe metric | Lexion | tie | ParaSwap |
|---|---:|---:|---:|
| measured-gas net output | **23** | 0 | 5 |
| economic result after `$0.02` cutoff | **19** | 5 | 4 |

Lexion's median measured advantage is `+0.99 bps` and its mean is `+0.53 bps`. Median measured gas
is `288,735` for Lexion versus `314,390` for ParaSwap. The economically material Lexion-favoring
differences total approximately `$19.93`; ParaSwap-favoring differences total approximately
`$4.25`. The largest material observations are about `$10.10` for Lexion on USDT-to-DAI and about
`$2.40` for ParaSwap on USDT-to-AAVE.

ParaSwap's advertised quote was optimistic relative to its replayed output by about `1.00 bps` at
the median and `1.54 bps` at the mean. Lexion's median quote-to-replay difference was zero. This,
together with measured rather than estimated gas, explains why the previous 2-to-27 reported-gas
score reverses to 23-to-5. The earlier statement that the equal-universe residual was an execution-
cost loss is therefore superseded by this replay experiment.

The strongest supported conclusion is now narrower and more defensible: ParaSwap's unrestricted
advantage mainly comes from its larger routing universe. When both systems are restricted to
Uniswap V2/V3/V4 and their executable transactions are replayed at one state, Lexion wins this
sample by both count and aggregate economically material value while retaining its latency
advantage. This remains a 28-order sample, not a universal router ranking.

The executable artifact is
`artifacts/benchmarks/comparisons/lexion-cow-paraswap/paraswap-uniswap-only-executable-30.json`.

## 14. Controlled search ablation

The earlier experiments measured useful pieces of the search architecture, but they were captured
at different blocks or changed more than one mechanism at once. They therefore remain historical
support rather than a causal ablation. For example, the block-25919677 V4 experiment measured
`9.907 s` for exhaustive subsets and `4.111 s` for maximal frontiers (`2.41x`), while the historical
quotient census counted `31,917` ordered histories collapsing to `5,888` canonical portfolios
(`5.42x`). Neither alone isolates all transformations on one market.

The analysis-only runner `scripts/routing-ablation-matrix.sh` supplies the missing controlled
experiment. It captures one market once, reuses one order sequence, gas price, timeout, hop bound,
and worker count, and changes only the following treatment:

| row | treatment |
|---|---|
| A | enumerate and evaluate every ordering of every compatible bounded V4 subset |
| B | evaluate each compatible bounded subset once in canonical order; no V4 upper-bound prune |
| C | evaluate maximal component-disjoint frontiers; no V4 upper-bound prune |
| D | C plus the exact zero-loss full-input upper-bound prune |
| E | D plus the `0.01`-bps exact-allocation stopping bound |

The first full run used Ethereum block `25,950,056`, 2,490 captured components, 500 seeded orders,
one in-flight solve, one worker, a five-second per-solve timeout, and only Uniswap V2, V3, and V4.

| row | solved | total solve time | p50 | p95 | slowest |
|---|---:|---:|---:|---:|---:|
| A: ordered histories | 481/500 | 176.150 s | 27.738 ms | 2,001.743 ms | 5,002.512 ms |
| B: canonical subsets | 483/500 | 29.272 s | 10.470 ms | 120.902 ms | 5,002.240 ms |
| C: maximal frontiers | 483/500 | 21.561 s | 7.464 ms | 31.879 ms | 5,001.363 ms |
| D: exact zero-loss prune | 483/500 | 21.315 s | 7.190 ms | 31.782 ms | 5,001.484 ms |
| E: `0.01`-bps cutoff | 484/500 | 4.756 s | 7.291 ms | 29.285 ms | 89.108 ms |

Thus canonicalizing histories reduced total time by `6.02x`; maximal-frontier enumeration reduced
the remaining time by `1.36x`; the zero-loss outer prune was neutral on this capture (`1.01x`);
and the bounded allocation cutoff reduced D's total time by `4.48x`. End to end, E was `37.04x`
faster than A and eliminated A's three timeout-only misses.

The quality result is deliberately not described as equivalence. On the 481 orders common to A
and B, B tied A on 441 and returned a smaller integer result on 40; the largest difference was only
`0.00313` bps. On all 483 orders common to B and C, C tied on 474 and was smaller on nine, with a
largest difference of `0.13176` bps. C and D were bit-for-bit equal on all 483 common solved orders.
D and E were also equal on all 483 common solved orders, while E additionally solved the one order
on which D timed out.

The A-to-C drift is evidence for the theorem boundary, not evidence against the set quotient. The
production multi-path allocator uses order-sensitive coordinate ascent and a heuristic path-removal
fallback, so it does not establish the global-allocation premise required by maximal-frontier
optimality. Repeating a set in different orders can therefore give the heuristic more starting
orders, and optimizing a subset independently can differ from optimizing it as a zero-weight face
of a maximal frontier. D's exact upper bound is semantics-preserving on its admitted domain. E is a
bounded approximation; this capture observed no output loss, but its contractual guarantee remains
at most `0.01` bps rather than exact equality.

The complete reproducibility artifact is
`artifacts/benchmarks/runs/routing-ablation-matrix-500/report.md`; per-order integer outputs and
latencies are in the adjacent `orders.csv`.

### Certified pair-face closure experiment

We next tested the strongest exact extension available without inventing a multidimensional
certificate. For every maximal frontier, the experimental
`path_frank_wolfe_multiscale_v4_certified_pair_faces_d2` treatment retains the ordinary
full-frontier candidate and additionally optimizes every supported one- and two-path face using the
integer branch-and-bound solver. A pair candidate is returned only when that solver reports
`Complete`; `Unsupported` and `BudgetExceeded` expose no certificate. Before invoking it, the
monotone full-input bound rejects any pair that provably cannot beat the current incumbent.

This establishes exact allocation completeness over supported portfolios with final support at
most two. It does not establish the optimum over three- or four-path allocations.

On a 100-order live capture at block `25,950,177`, the normal maximal-frontier treatment and
certified pair-face closure both solved 98 orders. Their outputs tied on 97; certified closure
improved one result by effectively one output wei. Runtime increased from `2.595 s` to `7.814 s`:
p50 moved from `7.064 ms` to `7.372 ms`, while p95 moved from `175.608 ms` to `548.005 ms`.
Without the incumbent upper-bound gate, an earlier capture took `13.993 s` rather than `2.525 s`
and produced the same one-wei-scale benefit.

The experiment therefore rejects unconditional pair-face certification as a production default.
The exact solver is useful as a theorem-bearing fallback, but its present tail cost is not justified
by the observed economic gain. Closing the general maximal-frontier theorem still requires either
a cheap globally complete allocator over support three and four, or a substantially sharper sound
certificate that identifies the rare face requiring exact search before entering branch-and-bound.

Artifacts are `artifacts/benchmarks/runs/certified-pair-face-closure-100/report.md` and
`artifacts/benchmarks/runs/certified-pair-face-bound-100/report.md`.

### Allocation support and bound-gap census

`AllocationSupportAndBoundGapAuditV1` observes three- and four-path maximal frontiers without
changing the candidate returned by normal routing. For each frontier it records the heuristic's
active support, the monotone full-input bound and gap, exact pair-face completion status, the best
certified pair result, per-path input/output/gas, and certificate runtime.

On 100 live orders at block `25,950,238`, the audit observed 172 qualifying frontiers: 100 of width
three and 72 of width four.

| final heuristic support | frontiers | share |
|---:|---:|---:|
| 1 | 103 | 59.9% |
| 2 | 36 | 20.9% |
| 3 | 31 | 18.0% |
| 4 | 2 | 1.2% |

Thus 139/172 (`80.8%`) of the heuristic results already lay on a one- or two-dimensional face;
33/172 (`19.2%`) retained support three or four. This is not a proof that the global optimum has
the same support, but it argues against beginning with an unconditional four-dimensional exact
solver.

Current certificate coverage is the stronger blocker. Only 12 frontiers contained any pair for
which the existing exact solver returned `Complete`; 157 had every pair report `Unsupported`, and
three encountered a pair budget exhaustion. Among the supported observations, the best certified
pair beat the heuristic once, tied it five times, and lost to it six times. Pair auditing consumed
`3.684 s` in total; the median query group cost was only `0.055 ms`, but the slow tail dominated.

The full-input upper bound is too weak to select the rare useful face. The median
`(upper - heuristic) / heuristic` ratio was `2.02`, with p95 `3.07`: the bound commonly sat around
three times the achieved value. It is sound, but not discriminative enough to guide expensive
multidimensional search.

The next certificate work should therefore expand exact pair support—particularly passive V4—and
tighten per-path interval envelopes before attempting general support-three/four branch-and-bound.
Only after broader pair coverage can a second census distinguish genuine high-support optima from
coordinate-ascent artifacts.

The routing report is `artifacts/benchmarks/runs/allocation-support-audit-100/report.md`; the
analysis trace was captured separately because this audit is intentionally not part of production
reporting.

#### Passive-V4 monotonicity certificate probe

The first passive-V4 extension used only the property available without private simulator fields.
For a pair split with right-path flow `x in [lo, hi]`, monotonicity gives

\[
Q_L(T-x)+Q_R(x)\le Q_L(T-lo)+Q_R(hi).
\]

Ignoring gas preserves this as a net-output upper bound. The implementation admits only V4 states
with no hook or the zero hook address, and an exhaustive small-integer-domain regression confirmed
that every interval bound covered the actual replay maximum and that branch-and-bound returned the
global optimum.

The live computation result was negative. In the first 16 audited three-path frontiers, every one
of the three V4-containing pair solves reached the 16,384-interval budget rather than completing.
Each frontier group cost roughly `1.5–7.5 s`. The extension changed the dominant result from
`Unsupported` to `BudgetExceeded`, not to a usable certificate. The run was stopped after the
failure repeated rather than consuming the full 100-order campaign.

Accordingly, passive-V4 pair certification remains gated by the analysis-only
`FYND_PASSIVE_V4_PAIR_CERTIFICATE` environment variable and does not affect normal routing. A useful
V4 certificate needs exact access to the pool's initial/terminal square-root price and fee so that
the V3-style marginal envelope can replace this loose monotone box bound.

#### Exact passive-V4 marginal-envelope probe

A follow-up prototype patched Tycho `0.372.0` locally with a read-only accessor returning the exact
`sqrtPriceX96` and direction-adjusted fee for hook-free, static-fee V4 states. Hooked pools and
dynamic-fee pools returned `Unsupported`. The router then composed the same exact rational
initial/terminal marginal envelope used for V3 paths. An exhaustive small-domain regression still
recovered the exact two-path optimum.

This tighter local bound was nevertheless insufficient at router scale. On one live Ethereum
capture at block `25,950,413`, using 2,481 pools and 20 dataset orders with one worker and a
five-second per-solve timeout, the certified-pair-face configuration timed out on `20/20` orders
(`100.040 s` total). The ordinary multiscale configuration solved `11/20` (`64.312 s` total), while
the Bellman--Ford baseline solved `20/20` (`39 ms` total). The report is
`artifacts/benchmarks/runs/passive-v4-tight-envelope-20/report.md`.

This rejects the narrow hypothesis that exposing passive V4's exact spot state is sufficient to
make exhaustive certified face closure practical. The accessor removes an information barrier,
but the remaining cost is still dominated by the number of portfolio faces and integer intervals
visited. The prototype therefore remains out of production. A future exact design must gate face
construction before allocation search or derive a substantially stronger interval certificate;
simply importing V3's marginal envelope does not solve the outer combinatorial problem.
