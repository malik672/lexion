# V3 allocation as structured concave optimization

## Status

Research note. This formalizes the current pool-disjoint V3/mixed amount-allocation problem and records a falsification-driven replacement direction for the existing golden-section coordinate search. It is not yet a production exactness claim.

## 1. Fixed-topology problem

Fix a set of pool-disjoint source-to-target paths `P_1..P_n` and a total exact-input order amount `A`.

For path `i`, define

```text
Q_i(x) = exact output returned by simulator replay for input x
```

with `Q_i(0)=0`.

The gross amount-allocation problem is

```text
maximize    sum_i Q_i(x_i)
subject to  sum_i x_i = A
            x_i >= 0
```

Pool-disjointness is important: each path can be replayed from the same entry market snapshot without another selected path mutating its pools. Shared-pool topologies require the merged execution model instead and are outside this fixed-path formulation.

Gas is kept outside the structural argument below. A selected portfolio is still accepted only after exact route construction and post-gas comparison. Tick-dependent gas can introduce additional discontinuities, so a future net-objective optimizer must model it explicitly rather than inherit the gross-output concavity argument silently.

## 2. Continuous V3 structure

Uniswap V3 is piecewise constant-product rather than an arbitrary black box. During an exact-input swap the pool price moves monotonically against the trader. Crossing an initialized tick changes active liquidity and therefore curvature, but it does not reset the execution price to a better price for the same swap direction.

Consequently the continuous exact-input output function of one pool is increasing and concave. Its marginal output is non-increasing as input grows.

For a multi-hop path

```text
Q(x) = q_h(q_{h-1}(...q_1(x)))
```

where every hop function is increasing and concave. Composition of an increasing concave function with a concave function is concave, so a fixed V3/V2 path retains continuous concavity.

Therefore for the continuous relaxation of a pool-disjoint portfolio, every active path at an interior optimum obeys the KKT condition

```text
Q_i'(x_i) = lambda
```

with inactive paths satisfying

```text
Q_i'(0) <= lambda.
```

This is the same economic condition exploited analytically by the V2 allocator; V3 differs because `Q_i` has no single three-parameter closed form across tick crossings.

## 3. Pairwise reduction

For two paths with fixed combined allocation `K`, write

```text
F(x) = Q_L(x) + Q_R(K - x),   0 <= x <= K.
```

For the continuous relaxation,

```text
F'(x) = Q_L'(x) - Q_R'(K-x).
```

`Q_L'(x)` is non-increasing in `x`. Because `K-x` decreases as `x` grows, `Q_R'(K-x)` is non-decreasing in `x`. Hence `F'(x)` is non-increasing and `F` is concave.

The continuous pair optimum is therefore one of:

1. `x=0`;
2. `x=K`;
3. a point where `Q_L'(x)=Q_R'(K-x)`;
4. a tick-regime boundary where the derivative changes discontinuously.

This gives a structural search target. We do not need arbitrary objective probes over the whole interval.

## 4. Simulator marginal oracle

The simulator already returns each hop's post-swap state. For a replayed allocation `x`, the terminal marginal of a path can be obtained from the post-swap spot price of every hop and the chain rule:

```text
M_path(x) = product_h M_h(post_state_h(x)).
```

where `M_h` is the fee-adjusted marginal exchange rate exposed by the protocol simulation state.

For a pair, the sign of

```text
D(x) = M_L(x) - M_R(K-x)
```

indicates the direction in which the continuous allocation should move:

```text
D(x) > 0  => move input from R to L
D(x) < 0  => move input from L to R
```

Because the continuous path marginals are monotone, bisection on `D(x)` locates the continuous optimum without assuming one global constant-product formula.

## 5. Why this is not already an exact integer solver

Tycho/Uniswap execution uses integer arithmetic and per-step rounding. Let the simulator-visible output be

```text
Q_i^Z(x) = integer replay output.
```

Even when the underlying continuous envelope `Q_i` is concave, flooring can create small local marginal oscillations. In general

```text
floor(Q(x+1)) - floor(Q(x))
```

need not be non-increasing.

Therefore the tempting statement

```text
integer V3 output is discretely concave, so ternary/binary search is globally exact
```

is not justified.

This was falsified in the prototype harness: randomized piecewise-concave tick-like curves with integer flooring produced cases where unimodal ternary narrowing discarded the true integer optimum.

The compiler lesson applies directly: preserve the strong symbolic/continuous structure, but replay the physical discrete semantics before claiming correctness.

## 6. Proposed solver

For each selected pair `(L,R)` with combined amount `K`:

```text
1. Find the continuous marginal-equality point x* using simulator-derived terminal marginals.
2. Include the endpoints 0 and K.
3. Include detected tick/regime boundaries when cheaply available.
4. Exact-replay an integer neighborhood around x* and each relevant boundary.
5. Keep only a strict exact-output improvement for the pair.
6. Repeat pair exchanges until no pair improves or the search budget expires.
```

The first implementation can omit explicit tick-boundary discovery and use a conservative exact-replay neighborhood around `x*`. A stronger version should exploit simulator residue such as per-hop gas/step signatures to locate regime changes and add them as mandatory candidates.

### Pair pseudocode

```text
pair_total = x_L + x_R
lo = 0
hi = pair_total

while marginal-search budget remains:
    mid = midpoint(lo, hi)
    m_L = terminal_marginal(L, mid)
    m_R = terminal_marginal(R, pair_total - mid)

    if m_L > m_R:
        lo = mid
    else:
        hi = mid

center = midpoint(lo, hi)

candidates = exact integer points around center
candidates += {0, pair_total}
candidates += discovered regime boundaries

return argmax_x exact_replay(L,x) + exact_replay(R,pair_total-x)
```

## 7. Multi-path correctness level

For the continuous separable-concave problem, pairwise exact coordinate maximization until no pair improves is sufficient for a global optimum: any non-optimal feasible point admits an improving feasible direction, and separability decomposes such a direction into pairwise exchanges.

For integer replay, we do not currently claim that the local replay neighborhood proves global discrete optimality. The production-safe claim remains one-sided:

```text
any candidate we accept has been exactly replayed and is better than the previous allocation under the simulator;
we may still miss a better integer allocation.
```

A future exact theorem needs either:

- a proof of an appropriate discrete-concavity class for the actual V3 integer transition relation; or
- a sound bound on rounding error that proves all integer points outside the replay neighborhoods cannot win; or
- exact regime-wise integer optimization using the protocol's tick arithmetic.

## 8. Falsification experiment

A small synthetic harness models paths as piecewise-concave marginal-price curves with downward marginal changes at tick-like boundaries, then floors final outputs to introduce integer noise. For each randomized two-path instance it compares candidate algorithms against exhaustive enumeration of every integer split.

In one seeded 3,000-instance run:

```text
16-evaluation golden search + tiny local replay:
    misses: 787 / 3000
    maximum observed output gap: 3 units

continuous marginal equality + exact +/-32 replay:
    misses: 0 / 3000
    maximum observed output gap: 0 units
```

This is evidence about the proposed search shape, not evidence about live Tycho V3 states. The next required experiment is the same comparison on real/snapshotted `UniswapV3State` objects at deliberately small bounded notionals where exhaustive integer enumeration is feasible.

## 9. Production experiment gate

Do not replace the current V3 search solely from the synthetic result. Gate integration on a differential corpus:

```text
for each small V3/V2-V3 portfolio:
    exhaustive = argmax over every integer split
    current    = existing golden-section result
    proposed   = marginal-equality + replay polish

record:
    proposed == exhaustive
    current == exhaustive
    simulator calls
    solve time
    tick crossings
    output gap when not exact
```

Only after the proposed solver demonstrates no observed losses against exhaustive replay on the admitted corpus should it become the default. Any counterexample becomes a new abstraction/refinement case, exactly as in the compiler symbolic-transition work.

## 10. Relationship to canonical flow-DAG search

This solves the amount problem conditioned on a fixed pool-disjoint topology. It composes naturally with canonical topology search:

```text
canonical flow DAG
    -> identify independent path allocation subproblem
    -> marginal-equality V3/V2 amount proposal
    -> exact integer replay
    -> incumbent comparison
```

The topology quotient controls combinatorial route history. The marginal solver controls continuous amount search. Exact replay remains the common physical arbiter.
