#!/usr/bin/env bash
# Compare native Path Frank-Wolfe with the exact Uniswap V2/V3 refinement layer.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ORDERS="${1:-2000}"
NAME="${2:-exact-v2-$(date +%Y%m%d-%H%M%S)}"
SEED="${3:-$(date +%s)}"
AMOUNT_MULTIPLIER="${AMOUNT_MULTIPLIER:-1}"
shift "$(( $# < 3 ? $# : 3 ))"

# Keep the convenient historical trailing `live` shorthand, but translate it
# into the clap form the benchmark actually accepts: `--market live`.
BENCH_ARGS=()
IS_LIVE=0
for argument in "$@"; do
  if [[ "$argument" == "live" ]]; then
    IS_LIVE=1
  else
    BENCH_ARGS+=("$argument")
  fi
done
if [[ $IS_LIVE -eq 1 ]]; then
  BENCH_ARGS+=(--market live)
fi

FIXTURE="fynd-core/tests/fixtures/market_recording.json.zst"
FULL_TRADES="aggregator_trades_50k_1k_usd.json"
SAMPLE_TRADES="tools/benchmark/src/trades_sample.json"

if [[ $IS_LIVE -eq 0 ]]; then
  if [[ ! -f "$FIXTURE" ]]; then
    echo "error: recorded market fixture is absent: $FIXTURE" >&2
    echo "run this comparison live instead by appending: live" >&2
    exit 1
  fi
  if head -n 1 "$FIXTURE" | grep -q 'git-lfs.github.com'; then
    echo "error: $FIXTURE is a Git LFS pointer, not the recorded market" >&2
    echo "install Git LFS and run: git lfs pull" >&2
    exit 1
  fi
fi

if [[ -f "$FULL_TRADES" ]]; then
  TRADES="$FULL_TRADES"
else
  TRADES="$SAMPLE_TRADES"
  echo "note: full trade corpus is absent; using the bundled 50-trade sample"
fi

mkdir -p target/exact-v2-samples
SHUFFLED_TRADES="target/exact-v2-samples/$NAME-$SEED.json"
python3 - "$TRADES" "$SHUFFLED_TRADES" "$SEED" "$AMOUNT_MULTIPLIER" <<'PY'
import json
import random
import sys
from decimal import Decimal

source, destination, seed = sys.argv[1], sys.argv[2], int(sys.argv[3])
multiplier = Decimal(sys.argv[4])
with open(source) as file:
    trades = json.load(file)
random.Random(seed).shuffle(trades)
if multiplier != 1:
    for trade in trades:
        for order in trade["orders"]:
            order["amount"] = str(max(1, int(Decimal(order["amount"]) * multiplier)))
with open(destination, "w") as file:
    json.dump(trades, file, separators=(",", ":"))
PY
echo "random seed: $SEED"
echo "amount multiplier: $AMOUNT_MULTIPLIER"

./scripts/bench.sh \
  --name "$NAME" \
  --orders "$ORDERS" \
  --trades "$SHUFFLED_TRADES" \
  --jobs 1 \
  --configs path_frank_wolfe_native_d2,path_frank_wolfe_d2 \
  ${BENCH_ARGS[@]+"${BENCH_ARGS[@]}"}

REPORT="bench-results/$NAME/report.md"
ORDERS_CSV="bench-results/$NAME/orders.csv"
echo
echo "Native Fynd vs Fynd + exact V2/V3 refinement:"
echo "  $REPORT"
echo
python3 - "$ORDERS_CSV" fynd-core/benches/tokens.json <<'PY'
import csv
import statistics
import sys

native_name = "path_frank_wolfe_native_d2"
hybrid_name = "path_frank_wolfe_d2"
rows = {}
with open(sys.argv[1], newline="") as file:
    for row in csv.DictReader(file):
        if row["config"] in (native_name, hybrid_name):
            rows.setdefault(row["order"], {})[row["config"]] = row

with open(sys.argv[2]) as file:
    token_metadata = {address.lower(): value for address, value in __import__("json").load(file).items()}

wins = ties = losses = native_only = hybrid_only = 0
total_delta = 0
native_times = []
hybrid_times = []
winning_rows = []
for pair in rows.values():
    native = pair.get(native_name)
    hybrid = pair.get(hybrid_name)
    if not native or not hybrid:
        continue
    native_ok = native["solved"] == "true"
    hybrid_ok = hybrid["solved"] == "true"
    if native_ok:
        native_times.append(int(native["elapsed_us"]))
    if hybrid_ok:
        hybrid_times.append(int(hybrid["elapsed_us"]))
    if native_ok and hybrid_ok:
        delta = int(hybrid["net_out"]) - int(native["net_out"])
        total_delta += delta
        if delta > 0:
            wins += 1
            winning_rows.append((native, hybrid, delta))
        elif delta < 0:
            losses += 1
        else:
            ties += 1
    elif native_ok:
        native_only += 1
    elif hybrid_ok:
        hybrid_only += 1

def median(values):
    return f"{statistics.median(values):.1f}" if values else "n/a"

print(f"orders compared: {wins + ties + losses}")
print(f"hybrid wins:     {wins}")
print(f"ties:            {ties}")
print(f"hybrid losses:   {losses}")
print(f"hybrid only:     {hybrid_only}")
print(f"native only:     {native_only}")
print(f"net output gain: {total_delta}")
print(f"native median:   {median(native_times)} us")
print(f"hybrid median:   {median(hybrid_times)} us")
if winning_rows:
    print("\nHybrid wins:")
    for native, hybrid, delta in winning_rows:
        token_in = token_metadata.get(native["token_in"].lower(), [native["token_in"][:10], 0])
        token_out = token_metadata.get(native["token_out"].lower(), [native["token_out"][:10], 0])
        input_amount = int(native["amount_in"]) / 10 ** token_in[1]
        output_gain = delta / 10 ** token_out[1]
        print(
            f"  {input_amount:.8g} {token_in[0]} -> {token_out[0]}: "
            f"+{output_gain:.12g} {token_out[0]} net"
        )
PY
