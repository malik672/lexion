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
DEFAULT_FALLBACK_RPC="https://reth-ethereum.ithaca.xyz/rpc"

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

rpc_works() {
  local url="$1"
  local body
  body="$(curl -sS --max-time 10 -H 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
    "$url" 2>/dev/null || true)"
  [[ "$body" == *'"result":"0x'* ]]
}

# The normal Fynd RPC may be a metered/provider URL. SOR performs many eth_call/multicall requests,
# so validate it before starting 31 orders. UNISWAP_RPC_URL can explicitly select a benchmark RPC;
# otherwise try RPC_URL and then the public Reth endpoint already used by the repo's dev environment.
PRIMARY_RPC="${UNISWAP_RPC_URL:-${RPC_URL:-}}"
FALLBACK_RPC="${FORK_RPC_URL:-$DEFAULT_FALLBACK_RPC}"
if [[ -n "$PRIMARY_RPC" ]] && rpc_works "$PRIMARY_RPC"; then
  export UNISWAP_RPC_URL="$PRIMARY_RPC"
  echo "Uniswap RPC: primary endpoint passed preflight"
elif [[ "$FALLBACK_RPC" != "$PRIMARY_RPC" ]] && rpc_works "$FALLBACK_RPC"; then
  export UNISWAP_RPC_URL="$FALLBACK_RPC"
  echo "warning: primary RPC failed preflight; using repo fallback Reth endpoint" >&2
else
  echo "error: no usable Ethereum RPC for Uniswap SOR" >&2
  echo "set UNISWAP_RPC_URL in .env to a mainnet RPC that permits eth_call/multicall" >&2
  exit 1
fi

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
