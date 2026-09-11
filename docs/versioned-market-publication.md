# Versioned market publication

The live Ethereum feed can deliver ordered per-block deltas through a single-producer,
single-consumer circular buffer. The producer writes only completed update messages; a dedicated
market-builder consumes them and applies them to a private next-block state.

Once every delta for a block has been applied, the builder atomically publishes an immutable,
block-labelled market snapshot. Quote workers retain the exact snapshot they acquired, while new
quotes immediately see the newly published version. Slow readers therefore do not delay market
updates, and no quote can observe a mixture of two blocks.

```text
Tycho/Ethereum feed
        |
        | ordered block deltas
        v
SPSC circular buffer
        |
        v
single market-builder
        |
        | atomic publication
        v
immutable Snapshot N
        +-- quote worker 1
        +-- quote worker 2
        `-- quote worker 3
```

The circular buffer solves communication between ingestion and construction. Immutable snapshots
solve concurrent access by quote workers. The design still requires acquire/release publication,
safe lifetime management for retired snapshots, and an explicit ring-full recovery policy. A safe
initial overflow policy is to mark the consumer out of sync and request a fresh authoritative
snapshot rather than silently dropping deltas.

Before replacing the current `RwLock`, benchmark lock hold/wait time and compare it with an
`ArcSwap` reference implementation. Measure quote p50/p99 latency, block-publication latency,
stale-block distance, allocations, retained snapshots, and memory usage.
