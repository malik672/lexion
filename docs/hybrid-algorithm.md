# Hybrid exact-replay routing

## Method

Path discovery is fast but a single route can leave output on the table when liquidity is split across independent pools. The hybrid treats the native solver's answer as a safe floor. It obtains candidate V2, V3, and mixed paths, considers bounded disjoint allocations, and replays each portfolio through the actual pool simulators in execution order.

Only integer simulator outputs are compared. Approximate scoring may rank paths, but it cannot by itself select the final route.

## Acceptance

Candidates are compared after the same gas model. A strict improvement is required, which makes the hybrid a non-regressive refiner for a fixed snapshot and candidate family.

## Limits

The search is deliberately bounded; it does not claim global optimality. It is also a quote optimizer, not an execution system: real deployment must handle changing state, transaction construction, gas estimation, reverts, latency, and MEV.
