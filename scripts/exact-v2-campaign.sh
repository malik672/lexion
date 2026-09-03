#!/usr/bin/env bash
# Run randomized offline/live quality comparisons and optional execution-risk audits.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ORDERS="${1:-500}"
OFFLINE_RUNS="${2:-5}"
BASE_SEED="${3:-42}"
LIVE_SNAPSHOTS="${LIVE_SNAPSHOTS:-0}"
SNAPSHOT_DELAY_SECONDS="${SNAPSHOT_DELAY_SECONDS:-30}"
EXECUTION_TRADES="${EXECUTION_TRADES:-0}"
SIZE_MULTIPLIERS="${SIZE_MULTIPLIERS:-0.01,0.1,1,10,100,1000}"
STALE_DELAYS_MS="${STALE_DELAYS_MS:-0,2000,10000}"
SKIP_OFFLINE="${SKIP_OFFLINE:-0}"
CAMPAIGN="${CAMPAIGN_NAME:-exact-v2-campaign-$(date +%Y%m%d-%H%M%S)}"
CAMPAIGN_DIR="bench-results/$CAMPAIGN"
mkdir -p "$CAMPAIGN_DIR"

echo "Campaign: $CAMPAIGN"
echo "Randomized offline runs: $OFFLINE_RUNS × $ORDERS orders per size"
echo "Amount multipliers: $SIZE_MULTIPLIERS"
IFS=',' read -r -a MULTIPLIERS <<<"$SIZE_MULTIPLIERS"
if (( SKIP_OFFLINE == 0 )); then
  for multiplier in "${MULTIPLIERS[@]}"; do
    size_name="${multiplier//./p}"
    for ((run = 0; run < OFFLINE_RUNS; run++)); do
      seed=$((BASE_SEED + run))
      name="$CAMPAIGN-offline-size-$size_name-run-$run-seed-$seed"
      AMOUNT_MULTIPLIER="$multiplier" ./scripts/compare-exact-v2.sh "$ORDERS" "$name" "$seed"
    done
  done
fi

if (( LIVE_SNAPSHOTS > 0 )); then
  : "${TYCHO_API_KEY:?set TYCHO_API_KEY for live snapshots}"
  : "${TYCHO_URL:?set TYCHO_URL for live snapshots}"
  : "${RPC_URL:?set RPC_URL for live snapshots}"
  echo "Live snapshots: $LIVE_SNAPSHOTS"
  for ((run = 0; run < LIVE_SNAPSHOTS; run++)); do
    seed=$((BASE_SEED + OFFLINE_RUNS + run))
    for multiplier in "${MULTIPLIERS[@]}"; do
      size_name="${multiplier//./p}"
      name="$CAMPAIGN-live-$run-size-$size_name-seed-$seed"
      AMOUNT_MULTIPLIER="$multiplier" ./scripts/compare-exact-v2.sh "$ORDERS" "$name" "$seed" \
        --market live --protocols uniswap_v2,uniswap_v3 --min-tvl 10
    done
    if (( run + 1 < LIVE_SNAPSHOTS )); then
      sleep "$SNAPSHOT_DELAY_SECONDS"
    fi
  done
fi

if (( EXECUTION_TRADES > 0 )); then
  : "${TYCHO_API_KEY:?set TYCHO_API_KEY for execution auditing}"
  : "${TYCHO_URL:?set TYCHO_URL for execution auditing}"
  : "${RPC_URL:?set RPC_URL for execution auditing}"
  FYND_URL="${FYND_URL:-http://localhost:3000}"
  SERVER_LOG="$CAMPAIGN_DIR/fynd-server.log"
  echo "Starting the hybrid router for encoding and dry-run settlement checks"
  cargo run --release -p fynd -- serve \
    -w scripts/exact-v2-worker-pools.toml \
    --rpc-url "$RPC_URL" --protocols uniswap_v2,uniswap_v3 --min-tvl 10 >"$SERVER_LOG" 2>&1 &
  SERVER_PID=$!
  cleanup() { kill "$SERVER_PID" 2>/dev/null || true; }
  trap cleanup EXIT
  for _ in $(seq 1 180); do
    curl -sf "$FYND_URL/v1/health" >/dev/null && break
    sleep 1
  done
  curl -sf "$FYND_URL/v1/health" >/dev/null || {
    echo "error: hybrid Fynd server did not become healthy; see $SERVER_LOG" >&2
    exit 1
  }

  IFS=',' read -r -a STALE_DELAYS <<<"$STALE_DELAYS_MS"
  for stale_delay in "${STALE_DELAYS[@]}"; do
    cargo run --release -p fynd-gas-audit -- \
      --n "$EXECUTION_TRADES" \
      --seed "$BASE_SEED" \
      --dataset aggregator_trades_50k_1k_usd.json \
      --fynd-url "$FYND_URL" \
      --rpc-url "$RPC_URL" \
      --quote-delay-ms "$stale_delay" \
      --out-dir "$CAMPAIGN_DIR/execution-audit-delay-${stale_delay}ms"
  done
  cleanup
  trap - EXIT
fi

python3 - "$CAMPAIGN" "$CAMPAIGN_DIR/quality-summary.txt" <<'PY'
import csv
import glob
import statistics
import sys

campaign, destination = sys.argv[1], sys.argv[2]
native_name = "path_frank_wolfe_native_d2"
hybrid_name = "path_frank_wolfe_d2"
wins = ties = losses = native_only = hybrid_only = 0
native_times, hybrid_times = [], []
for path in sorted(glob.glob(f"bench-results/{campaign}-*/orders.csv")):
    rows = {}
    with open(path, newline="") as file:
        for row in csv.DictReader(file):
            if row["config"] in (native_name, hybrid_name):
                rows.setdefault(row["order"], {})[row["config"]] = row
    for pair in rows.values():
        native, hybrid = pair.get(native_name), pair.get(hybrid_name)
        if not native or not hybrid:
            continue
        native_ok, hybrid_ok = native["solved"] == "true", hybrid["solved"] == "true"
        if native_ok:
            native_times.append(int(native["elapsed_us"]))
        if hybrid_ok:
            hybrid_times.append(int(hybrid["elapsed_us"]))
        if native_ok and hybrid_ok:
            delta = int(hybrid["net_out"]) - int(native["net_out"])
            wins += delta > 0
            ties += delta == 0
            losses += delta < 0
        elif native_ok:
            native_only += 1
        elif hybrid_ok:
            hybrid_only += 1

lines = [
    f"runs: {len(glob.glob(f'bench-results/{campaign}-*/orders.csv'))}",
    f"orders compared: {wins + ties + losses}",
    f"hybrid wins: {wins}",
    f"ties: {ties}",
    f"hybrid losses: {losses}",
    f"hybrid only: {hybrid_only}",
    f"native only: {native_only}",
    f"native median us: {statistics.median(native_times) if native_times else 'n/a'}",
    f"hybrid median us: {statistics.median(hybrid_times) if hybrid_times else 'n/a'}",
]
with open(destination, "w") as file:
    file.write("\n".join(lines) + "\n")
print("\nCombined quality summary:")
print("\n".join(lines))
PY

cat >"$CAMPAIGN_DIR/README.md" <<EOF
# Exact V2 routing campaign

- Offline randomized runs: $OFFLINE_RUNS
- Orders requested per run: $ORDERS
- First seed: $BASE_SEED
- Amount multipliers: $SIZE_MULTIPLIERS
- Live snapshots: $LIVE_SNAPSHOTS
- Live protocols: Uniswap V2 and Uniswap V3
- Delay between live snapshots: $SNAPSHOT_DELAY_SECONDS seconds
- Encoding/dry-run settlement trades: $EXECUTION_TRADES
- Quote-to-simulation delays: $STALE_DELAYS_MS milliseconds

The execution audit records encoding failures, simulated reverts, actual gas, and quote-to-execution
gas error. Repeated live snapshots expose sensitivity to changing/stale state. This campaign does
not simulate adversarial MEV ordering; a mainnet-fork adversarial state-perturbation harness is
still required before claiming MEV robustness.
EOF

echo "Campaign complete: $CAMPAIGN_DIR"
