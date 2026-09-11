# What remains

Lexion already has a working hybrid router, exact integer replay, a resource-aware trace quotient,
maximal-frontier enumeration, bounded-quality elimination, protocol-normalized external replay, and
a formal account of the parts that are actually proved. The remaining work is about closing the
distance between an experimentally strong system and a defensible production/research result.

## 1. Keep theorem and implementation aligned

- [x] Mark the five maximal-frontier theorem assumptions beside the production paths that establish
  or fail to establish them.
- [x] State explicitly that coordinate ascent is a heuristic fallback and does not establish the
  global allocation-optimality premise.
- [ ] Replace remaining prose-level links with stable code anchors once the production API settles.
- [ ] Add a CI check or review checklist preventing a theorem claim from silently expanding when an
  implementation fallback changes.

## 2. Freeze the admitted protocol families

- [x] Define V2, V3, passive V4, hooked V4, wrappers, and opaque simulator-backed protocols in the
  formalization.
- [x] Keep nonzero-hook V4 pools routable when their simulator succeeds while excluding them from
  certificates.
- [ ] Admit a hook family only after proving its monotonicity, gas behavior, feasible input domain,
  and complete mutable-resource footprint.
- [ ] Replace the experimental passive-V4 monotonicity bound with a useful certificate or retain
  `Unsupported`; the exact marginal-state probe still timed out on 20/20 live orders.

## 3. Maintain the ablation matrix

- [x] Compare ordered-history search, canonical quotient search, maximal-frontier search, exact
  zero-bps pruning, and 0.01-bps bounded-quality pruning on one identical live capture.
- [x] Record the observed `176.150 s -> 4.756 s` end-to-end reduction on the 500-order matrix.
- [ ] Repeat the full matrix on multiple pinned captures and publish paired confidence intervals.
- [ ] Separate structural-search time, simulator replay time, allocation time, and API overhead.

## 4. Repeat external replay

- [x] Compare executable routes at identical captured blocks and normalize both routes through the
  same gas/replay machinery.
- [ ] Expand to more blocks and independent order samples.
- [ ] Report paired confidence intervals, per-pair concentration, failure coverage, and stale-state
  sensitivity.
- [ ] Treat improvements of at most $0.02 as economic ties in public comparisons.

## 5. Add stronger baselines

- [x] Compare against Uniswap AlphaRouter, ParaSwap, and CoW where their products are comparable.
- [x] Keep CoW's solver/settlement product separate from same-chain executable-router claims.
- [ ] Add 0x and 1inch when reproducible authenticated API access and block-consistent executable
  replay are available.
- [ ] Preserve equal-universe comparisons separately from unrestricted routing-universe results.

## 6. Publish counterexamples

- [x] Record aggregate concavity failures and the exact integer/tick model behind them.
- [ ] Publish minimal, replayable examples showing rounding, tick crossings, gas activation, and
  hook behavior defeating continuous or differentiability assumptions.
- [ ] Include pool state, input domain, integer outputs, and the violated inequality for each case.

## 7. Freeze terminology

Use these terms in theorem statements and technical claims:

- **trace quotient** for merging histories with the same future-observable routing state;
- **maximal-frontier cover** for representing compatible subsets through zero-flow paths;
- **simulator-bound certificate** for bounds whose validity is restricted to the admitted simulator
  transition family;
- **bounded-quality elimination** for the 0.01-bps rule.

“Sacred Timeline” remains an engineering nickname, not a mathematical term.

## 8. Seek adversarial review

- [ ] Ask reviewers to identify prior systems using the same future-sufficient state quotient.
- [ ] Ask for a counterexample where quotient-equivalent states have different feasible futures.
- [ ] Ask for a simulator completion exceeding one of our certified upper bounds.
- [ ] Ask whether maximal-frontier coverage fails under any admitted resource interaction.
- [ ] Publish discovered counterexamples and narrow the theorem rather than hiding unsupported cases.

## Immediate engineering order

1. Stabilize the Tycho dependency versions; the current loose `>=0.372.0` simulation dependency can
   resolve alongside execution `0.372.0` and create incompatible duplicate Tycho types.
2. Add a cheap, sound pre-allocation face bound. Exact passive-V4 marginal state alone did not tame
   pair-face enumeration.
3. Run the multi-block ablation and external-replay campaigns.
4. Extract and publish minimal non-concavity counterexamples.
5. Request adversarial theorem and implementation review.
