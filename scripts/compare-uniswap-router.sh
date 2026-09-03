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

if [[ -z "${RPC_URL:-}" ]]; then
  echo "error: RPC_URL is not set (put it in .env)" >&2
  exit 1
fi
if [[ ! -f "$RUN_DIR/orders.csv" || ! -f "$RUN_DIR/report.md" ]]; then
  echo "error: $RUN_DIR must contain orders.csv and report.md from a live benchmark" >&2
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  echo "error: npm is required" >&2
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
