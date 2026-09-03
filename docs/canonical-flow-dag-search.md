# Canonical Flow-DAG Search for Exact DEX Routing

## Status

Research design. This is intentionally separate from the production hybrid V2/V3 refinement path.

The current router improves a fast incumbent by searching a bounded family of pool-disjoint paths and replaying candidate allocations with the actual pool simulators. This note describes the deeper exact-search direction that motivated the work: search canonical future-observable flow states instead of ordered swap histories.

## 1. Problem statement

A naive exact symbolic router can represent a candidate by the ordered sequence of swap actions used to construct it. That representation contains avoidable history.

Consider two independent branches:

```text
A -> B -> T
A -> C -> T
```

The linearizations

```text
A->B, B->T, A->C, C->T
A->C, C->T, A->B, B->T
```

may describe the same future routing problem when the branches use disjoint mutable pools and neither consumes flow produced by the other.

The search object should therefore be the dependency topology and its future-relevant state, not the arbitrary order in which independent actions were discovered.

## 2. Independence relation

Let `S` be a routing state and `a`, `b` two enabled swap transitions.

A conservative initial independence relation is:

```text
a I b
```

only when all of the following hold:

1. `a` and `b` mutate disjoint pools/components;
2. neither transition consumes token flow produced by the other;
3. neither transition changes a resource or constraint observed by the other;
4. both execution orders are legal from `S`;
5. after canonicalization, both orders produce the same future-observable routing state and objective contribution.

The desired commutation law is:

```text
C(step(step(S, a), b)) == C(step(step(S, b), a))
```

where `C` is the canonicalizer.

The relation must be conservative. Failing to recognize an independent pair only loses pruning. Incorrectly declaring dependent swaps independent can remove the optimal route.

## 3. Trace quotient

Let a history be an ordered transition sequence `h`.

Generate an equivalence relation by commuting adjacent independent actions:

```text
u a b v ~ u b a v    when a I b
```

The exact search should enumerate the quotient:

```text
Histories / ~
```

rather than every history.

A primary correctness target is:

```text
Opt(Histories) == Opt(Histories / ~)
```

under the defined simulator, gas model, single-use/resource rules, and order semantics.

## 4. Canonical state

A candidate state should retain only information capable of changing legal future transitions or the final score.

A first representation is:

```rust
struct TopologyKey {
    used_pools: PoolMask,
    active_token_support: TokenMask,
    flow_dag: FlowDagId,
}

struct SymbolicRouteState {
    key: TopologyKey,
    balances: Vec<ExprId>,
    constraints: ConstraintSetId,
    gas: GasState,
}
```

The exact key will evolve with counterexamples. In particular, stateful shared-pool execution, gas activation semantics, and route-encoding constraints must not be projected away if they can affect future legality or value.

### Canonicalization rules

At minimum:

- sort independent sibling branches by a stable structural key;
- assign canonical edge and variable numbers after sorting;
- hash-cons expressions and constraints;
- remove action-history fields that do not affect future behavior;
- retain dependency order where swaps do not commute.

The canonicalizer is part of correctness, not merely a hashing optimization.

## 5. Expression interning

Symbolic amount expressions should be stored in an arena:

```rust
enum ExprNode {
    Const(BigInt),
    Var(VarId),
    Add(ExprId, ExprId),
    Sub(ExprId, ExprId),
    Quote(PoolId, TokenId, ExprId),
}
```

Useful normalizations include:

- canonical operand order for commutative `Add`;
- constant folding;
- zero elimination;
- structural hash-consing;
- canonical alpha-renaming after DAG canonicalization.

The goal is that two equivalent topologies reached by different discovery orders use the same expression identities.

## 6. Forward and backward productive state

Forward search computes states reachable from the order entry condition:

```text
F = { q | q0 ->* q }
```

But local legality is not enough. A partial route can be reachable while no legal completion can consume all active non-target balances into the destination.

Define a backward/productive set:

```text
R = { q | q ->* terminal }
```

The useful exact search space is:

```text
F ∩ R
```

The production oracle may be an exact set, a bounded exact perimeter, or a conservative over-approximation. Safety requires:

```text
R_true subset_of R_computed
```

Extras reduce pruning efficiency. Missing a truly completable state is unsound.

## 7. Joint completion feasibility

Ordinary per-token reachability is too weak under finite pool resources.

Example:

```text
balance(B) --\
              one remaining single-use pool --> T
balance(C) --/
```

`B` and `C` may each be individually reachable to `T`, while no joint completion exists.

A first memoized feasibility oracle can be keyed by:

```text
(used_pools, positive_token_support, swaps_remaining)
```

and answer:

> Is there a legal remaining flow topology that consumes every active non-target balance into the target using the remaining resources?

This should run before expensive symbolic transition construction whenever its abstraction is sound.

A stronger implementation may require token-flow multiplicity, direction, component capabilities, or a matching/flow certificate rather than support bits alone.

## 8. Amount solving conditioned on topology

Topology search and amount optimization should be separated where possible.

For a pool-disjoint Uniswap V2 path, continuous output has the form:

```text
q(x) = a*x / (b + c*x)
```

and composition of V2 hops stays in the same family.

For a fixed set of compatible disjoint paths, solve:

```text
maximize sum_i q_i(x_i)
subject to sum_i x_i = A
           x_i >= 0
```

The continuous optimum satisfies marginal equality on active paths:

```text
q_i'(x_i) = lambda
```

This provides:

1. a cheap candidate allocation;
2. an upper bound for branch-and-bound;
3. a tiny integer neighborhood for exact replay.

For V3 or other opaque/stateful simulators, retain simulator-backed optimization rather than forcing a false analytic model.

For shared-prefix/shared-suffix DAGs, amount solving must respect the actual merged execution semantics. Independent-path output sums are not valid when branches mutate shared pools.

## 9. Exact replay remains authoritative

Every optimized candidate must ultimately be evaluated under the same execution semantics used to build the returned route:

```text
canonical topology
    -> amount proposal
    -> integer allocation
    -> exact pool-state replay
    -> gas-adjusted executable score
```

Analytic curves, interval bounds, and symbolic expressions are search machinery. They are not authoritative quote semantics unless proved equivalent to the simulator on the admitted domain.

## 10. Incumbent-guided exact search

Use a strong cheap incumbent before exact exploration:

```text
native general router
    -> current hybrid refinement
    -> local amount polish
    -> incumbent B
```

Then prune any partial canonical state with a sound upper bound `UB` satisfying:

```text
UB(state) <= B
```

The exact search only needs to discover a route that beats the incumbent, not rediscover obvious baseline quality from scratch.

## 11. Research obligations

Before claiming exactness or optimality, establish:

### Quotient soundness

If two histories canonicalize to the same key, they have the same legal continuation language and continuation scoring semantics.

### Independence soundness

Every declared independent swap pair commutes under the admitted state and resource model.

### Backward pruning soundness

Any state rejected by the completion oracle is truly unable to reach a legal terminal route.

### Amount-bound soundness

Any continuous or interval upper bound dominates every integer/simulator realization represented by the partial topology.

### Replay consistency

The scorer and emitted route use the same pool-state transition semantics, gas model, and rounding rules.

## 12. Measurement plan

Instrument the exact search with:

- generated ordered histories;
- unique canonical states;
- histories merged by canonicalization;
- states rejected by backward feasibility;
- terminal topologies;
- amount optimizations invoked;
- simulator replay count;
- p50/p95/p99 solve time;
- incumbent wins/ties;
- exact-optimum agreement on small exhaustive instances.

The key experiment is not merely quote quality. It is state-space collapse:

```text
ordered-history search
vs
canonical-flow-DAG quotient
vs
canonical quotient + backward productive pruning
```

on identical small instances where the optimum is independently known.

## 13. Relationship to the current hybrid

The existing V2/V3 hybrid is not discarded by this design. It provides:

- a practical route-quality baseline;
- a strong incumbent;
- exact simulator/replay infrastructure;
- analytic V2 path composition;
- a bounded environment in which to validate topology and amount-search ideas.

The canonical flow-DAG solver should initially live behind an experimental feature/configuration and be evaluated against the current hybrid before any production integration.
