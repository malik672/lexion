# Lexion

Lexion is a small local Ethereum route optimizer for Uniswap V2, V3, and passive V4 liquidity.
It consumes a live Tycho snapshot, discovers bounded paths, replays them through exact integer pool
simulators, constructs compatible path portfolios, and optimizes the input allocation.

## Install

```bash
git clone <repository-url> lexion
cd lexion
cp .env.example .env
# Add your Tycho key, RPC URL, wallet, pair, and amount to .env.
cargo build --release
```

## Quote locally

Amounts are raw token units. This example quotes 1,000 USDC into WETH:

```bash
./target/release/lexion quote \
  --token-in 0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48 \
  --token-out 0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2 \
  --amount 1000000000 \
  --max-hops 2
```

When `TOKEN_IN`, `TOKEN_OUT`, and `AMOUNT` are configured in `.env`, the same quote is simply:

```bash
./target/release/lexion quote
```

The result includes the captured block, gross output, simulator gas estimate, solve latency, input
allocation, and ordered Tycho component IDs for every selected path. If you have an output-token
gas conversion, pass it as an exact rational using `--gas-cost-numerator` and
`--gas-cost-denominator`; otherwise net output is deliberately reported as unavailable.

## Produce an unsigned swap transaction

`swap` converts the selected plan into Tycho Router calldata and validates it with a same-block
`eth_call` before printing it:

```bash
./target/release/lexion swap \
  --token-in 0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48 \
  --token-out 0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2 \
  --amount 1000000000 \
  --sender 0xYourAddress \
  --slippage-bps 30 \
  --rpc-url https://your-ethereum-rpc
```

The sender must have sufficient balance and must approve the printed `approval spender`. The CLI
never reads private keys, signs, or broadcasts. Pass the printed `to`, `value`, and `data` to a
wallet after reviewing them.

With the personal defaults filled in `.env`, transaction production is:

```bash
./target/release/lexion swap
```

Every command-line flag overrides its matching `.env` value. Private keys do not belong in this
file and Lexion never requests one.

The first encoder intentionally rejects native-token input and portfolios that share a pool. Those
cases require additional balance-flow or merged-state rules; refusing them is safer than emitting
calldata that was not equivalent to the simulated route.

## Scope

- Ethereum live market data through Tycho.
- Uniswap V2, V3, and simulator-supported passive V4 pools.
- At most two hops by default.
- Immutable per-block snapshots and lock-free reader publication.
- Exact simulator replay; no promise that every arbitrary V4 hook is executable or certifiable.

## Development

```bash
cargo test
./scripts/parity-original.sh --details
```

The retained experimental results, methodology, and limitations are documented in
[`BENCHMARKS.md`](BENCHMARKS.md).

For a concise engineering case study covering the problem, architecture, decisions, evidence, and
lessons, see [`PORTFOLIO.md`](PORTFOLIO.md).

## License

MIT
