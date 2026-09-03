#!/usr/bin/env bash
# Compare a completed live Fynd hybrid benchmark against Uniswap's open-source Smart Order Router.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ -f "$REPO_ROOT/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$REPO_ROOT/.env"
  set +a
fi

RUN_DIR="${1:-bench-results/v2-v3-live}"
TOOL_DIR="$REPO_ROOT/tools/uniswap-router-bench"
DEPS_STAMP="$TOOL_DIR/.deps-version"
EXPECTED_DEPS="sor-4.31.10-release-graph-v1"
PUBLIC_ARCHIVE_RPC="https://ethereum-rpc.blockreq.com/v1/rpc/public"

if [[ ! -f "$RUN_DIR/orders.csv" || ! -f "$RUN_DIR/report.md" ]]; then
  echo "error: $RUN_DIR must contain orders.csv and report.md from a live benchmark" >&2
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  echo "error: npm is required" >&2
  exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl is required" >&2
  exit 1
fi

# Read the benchmark's captured block and convert it to the JSON-RPC quantity form. This lets the
# preflight prove historical state access instead of accepting an RPC that only serves latest state.
CAPTURED_BLOCK="$(grep -Eio 'live ethereum block[[:space:]]+[0-9]+' "$RUN_DIR/report.md" | head -1 | grep -Eo '[0-9]+$' || true)"
if [[ -z "$CAPTURED_BLOCK" && -f "$RUN_DIR/run.json" ]]; then
  CAPTURED_BLOCK="$(python3 - "$RUN_DIR/run.json" <<'PY'
import json, re, sys
obj=json.load(open(sys.argv[1]))
def walk(x):
    if isinstance(x, dict):
        for k,v in x.items():
            if k.lower() in {'block','block_number','blocknumber'} and re.fullmatch(r'\d+', str(v)):
                print(v); raise SystemExit
            walk(v)
    elif isinstance(x, list):
        for v in x: walk(v)
walk(obj)
PY
)"
fi
if [[ -z "$CAPTURED_BLOCK" ]]; then
  echo "error: could not determine captured block for RPC archive preflight" >&2
  exit 1
fi
CAPTURED_BLOCK_HEX="$(printf '0x%x' "$CAPTURED_BLOCK")"

rpc_works_for_benchmark() {
  local url="$1"
  local latest historical

  latest="$(curl -sS --max-time 10 -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
    "$url" 2>/dev/null || true)"
  [[ "$latest" == *'"result":"0x'* ]] || return 1

  # WETH decimals() at the captured block. A successful 32-byte return proves this endpoint can
  # execute eth_call against the historical state SOR will query.
  historical="$(curl -sS --max-time 15 -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"eth_call\",\"params\":[{\"to\":\"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2\",\"data\":\"0x313ce567\"},\"$CAPTURED_BLOCK_HEX\"]}" \
    "$url" 2>/dev/null || true)"
  [[ "$historical" == *'"result":"0x'* ]]
}

# Prefer an explicitly supplied archive RPC. Otherwise use a public endpoint that advertises
# Ethereum archive history. Never silently reuse RPC_URL: the normal Fynd RPC may pass a cheap
# latest-state call but reject SOR's historical eth_call/multicall workload under load.
if [[ -n "${UNISWAP_RPC_URL:-}" ]]; then
  RPC_CANDIDATES=("$UNISWAP_RPC_URL")
else
  RPC_CANDIDATES=("$PUBLIC_ARCHIVE_RPC")
fi

SELECTED_RPC=""
for candidate in "${RPC_CANDIDATES[@]}"; do
  echo "Checking Uniswap RPC archive access ..."
  if rpc_works_for_benchmark "$candidate"; then
    SELECTED_RPC="$candidate"
    break
  fi
done

if [[ -z "$SELECTED_RPC" ]]; then
  echo "error: no RPC passed historical eth_call preflight at block $CAPTURED_BLOCK" >&2
  echo "set UNISWAP_RPC_URL in .env to an archive-capable Ethereum mainnet RPC" >&2
  exit 1
fi
export UNISWAP_RPC_URL="$SELECTED_RPC"
echo "Uniswap RPC: archive preflight passed at block $CAPTURED_BLOCK"

# Keep this legacy dependency stack on an LTS Node runtime.
NODE20=(npx --yes node@20)

# SOR 4.31.10 was released/tested with sdk-core 7.10.1, router-sdk 2.3.5,
# v2-sdk 4.17.0 and v3-sdk 3.27.0. A normal install years later can resolve
# newer packages through SOR's caret ranges, or preserve a stale lock generated
# by an older revision of this harness. Rebuild once whenever our pinned graph changes.
if [[ ! -f "$DEPS_STAMP" ]] || [[ "$(cat "$DEPS_STAMP")" != "$EXPECTED_DEPS" ]]; then
  echo "Rebuilding pinned Uniswap SOR dependency graph ..."
  rm -rf "$TOOL_DIR/node_modules" "$TOOL_DIR/package-lock.json"
  npm install --prefix "$TOOL_DIR" --no-audit --no-fund >/dev/null
  printf '%s\n' "$EXPECTED_DEPS" > "$DEPS_STAMP"
else
  echo "Checking pinned Uniswap Smart Order Router benchmark dependencies ..."
  npm install --prefix "$TOOL_DIR" --no-audit --no-fund >/dev/null
fi

echo "Installed Uniswap SDK graph:"
npm ls --prefix "$TOOL_DIR" --depth=1 \
  @uniswap/smart-order-router @uniswap/sdk-core @uniswap/router-sdk @uniswap/v2-sdk @uniswap/v3-sdk \
  2>/dev/null | sed -n '1,20p' || true

echo "Running same-block Fynd vs Uniswap SOR comparison (Node 20) ..."
"${NODE20[@]}" "$TOOL_DIR/bench.cjs" "$RUN_DIR"
