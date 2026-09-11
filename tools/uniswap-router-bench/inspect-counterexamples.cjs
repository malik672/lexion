#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const { parse } = require('csv-parse/sync');
const { ethers } = require('ethers');
const JSBI = require('jsbi');
const {
  AlphaRouter,
  TO_PROTOCOL,
  routeAmountsToString,
} = require('@uniswap/smart-order-router');
const { CurrencyAmount, Ether, Token, TradeType } = require('@uniswap/sdk-core');

const ZERO = '0x0000000000000000000000000000000000000000';
const CHAIN_ID = 1;
const HYBRID_CONFIG = 'path_frank_wolfe_d2';

function die(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}

function currency(address, metadata) {
  if (address.toLowerCase() === ZERO) return Ether.onChain(CHAIN_ID);
  const item = metadata[address.toLowerCase()];
  if (!item) throw new Error(`missing token metadata for ${address}`);
  const [symbol, decimals] = item;
  return new Token(CHAIN_ID, address, Number(decimals), symbol);
}

function walk(object, visit) {
  if (object == null || typeof object !== 'object') return;
  for (const [key, value] of Object.entries(object)) {
    visit(key, value);
    if (value != null && typeof value === 'object') walk(value, visit);
  }
}

function findConfigRoute(line, configName) {
  const direct = [line.routes, line.configs, line.results, line.algorithms];
  for (const container of direct) {
    if (container && typeof container === 'object' && container[configName]) return container[configName];
  }
  let found = null;
  walk(line, (key, value) => {
    if (found == null && key === configName && value && typeof value === 'object') found = value;
  });
  return found;
}

function loadFyndRoutes(runDir) {
  const byOrder = new Map();
  const routePath = path.join(runDir, 'routes.jsonl');
  for (const raw of fs.readFileSync(routePath, 'utf8').split(/\r?\n/)) {
    if (!raw.trim()) continue;
    const line = JSON.parse(raw);
    const id = line.order?.id == null ? null : String(line.order.id);
    if (id == null) continue;
    const route = findConfigRoute(line, HYBRID_CONFIG);
    if (route) byOrder.set(id, route);
  }
  return byOrder;
}

function symbol(address, metadata) {
  if (!address) return '?';
  if (String(address).toLowerCase() === ZERO) return 'ETH';
  return metadata[String(address).toLowerCase()]?.[0] || String(address);
}

function fyndEdges(route, metadata) {
  return (route?.edges || []).map((edge) => ({
    protocol: edge.protocol,
    pool: edge.component_id || edge.pool,
    token_in: symbol(edge.token_in, metadata),
    token_out: symbol(edge.token_out, metadata),
    token_in_address: edge.token_in,
    token_out_address: edge.token_out,
    amount_in: edge.amount_in,
    amount_out: edge.amount_out,
    gas: edge.gas,
    split: edge.split,
  }));
}

function uniswapLegs(uni) {
  return (uni.route || []).map((entry) => ({
    percent: entry.percent,
    protocol: entry.protocol,
    amount_in: entry.amount?.quotient?.toString?.() || null,
    quote_out: entry.quote?.quotient?.toString?.() || null,
    gas_estimate: entry.gasEstimate?.toString?.() || null,
    pools: entry.poolIdentifiers || [],
    token_path: (entry.tokenPath || []).map((token) => ({
      symbol: token.symbol,
      address: token.address || 'native',
    })),
    text: entry.toString?.() || null,
  }));
}

async function main() {
  const runDir = process.argv[2] || 'artifacts/benchmarks/runs/v2-v3-live';
  const rpcUrl = process.env.UNISWAP_RPC_URL;
  if (!rpcUrl) die('UNISWAP_RPC_URL is not set');

  const comparisonPath = path.join(runDir, 'uniswap-sor-comparison.json');
  if (!fs.existsSync(comparisonPath)) die(`${comparisonPath} does not exist; run the comparator first`);

  const comparison = JSON.parse(fs.readFileSync(comparisonPath, 'utf8'));
  const losses = (comparison.results || []).filter((r) => r.gross_winner === 'uniswap');
  if (!losses.length) die('comparison contains no gross Uniswap wins');

  const rows = parse(fs.readFileSync(path.join(runDir, 'orders.csv'), 'utf8'), {
    columns: true,
    skip_empty_lines: true,
  });
  const rowsByOrder = new Map(
    rows
      .filter((row) => row.config === HYBRID_CONFIG && row.solved === 'true')
      .map((row) => [String(row.order), row])
  );
  const metadata = JSON.parse(
    fs.readFileSync(path.resolve(__dirname, '../../fynd-core/benches/tokens.json'), 'utf8')
  );
  const fyndRoutes = loadFyndRoutes(runDir);

  const gasPriceWei = ethers.utils.parseUnits(String(comparison.gas_price_gwei), 'gwei');
  const provider = new ethers.providers.JsonRpcProvider(rpcUrl, CHAIN_ID);
  const gasPriceProvider = { async getGasPrice() { return { gasPriceWei }; } };
  const router = new AlphaRouter({ chainId: CHAIN_ID, provider, gasPriceProvider });
  const protocols = [TO_PROTOCOL('v2'), TO_PROTOCOL('v3'), TO_PROTOCOL('mixed')];
  const reports = [];

  console.log(`Inspecting ${losses.length} gross Uniswap counterexample(s) at block ${comparison.block_number}\n`);

  for (const prior of losses) {
    const row = rowsByOrder.get(String(prior.order));
    const fynd = fyndRoutes.get(String(prior.order));
    if (!row || !fynd) {
      console.log(`order ${prior.order}: missing local Fynd detail`);
      continue;
    }

    const tokenIn = currency(row.token_in, metadata);
    const tokenOut = currency(row.token_out, metadata);
    const amount = CurrencyAmount.fromRawAmount(tokenIn, JSBI.BigInt(row.amount_in));

    console.log('='.repeat(100));
    console.log(`${tokenIn.symbol} -> ${tokenOut.symbol} | order ${row.order} | prior gross ${prior.gross_bps_fynd_vs_uniswap?.toFixed?.(4) ?? prior.gross_bps_fynd_vs_uniswap} bps Fynd-vs-Uni`);
    console.log(`input raw: ${row.amount_in}`);

    const uni = await router.route(amount, tokenOut, TradeType.EXACT_INPUT, undefined, {
      blockNumber: comparison.block_number,
      protocols,
      maxSwapsPerPath: 2,
      minSplits: 1,
      maxSplits: 4,
      distributionPercent: 5,
      forceCrossProtocol: false,
      useCachedRoutes: false,
    });
    if (!uni) {
      console.log('Uniswap: no route on forensic re-quote');
      continue;
    }

    const fyndGross = BigInt(fynd.amount_out);
    const uniGross = BigInt(uni.quote.quotient.toString());
    const delta = fyndGross - uniGross;

    console.log('\nFYND');
    console.log(`gross raw: ${fyndGross}`);
    console.log(`net raw:   ${fynd.amount_out_net_gas}`);
    console.log(`gas units: ${fynd.gas ?? 'n/a'}`);
    for (const edge of fyndEdges(fynd, metadata)) {
      console.log(`  ${edge.protocol} ${edge.token_in}->${edge.token_out} pool=${edge.pool}`);
      console.log(`    in=${edge.amount_in} out=${edge.amount_out} split=${edge.split} gas=${edge.gas}`);
    }

    console.log('\nUNISWAP');
    console.log(`gross raw: ${uniGross}`);
    console.log(`net raw:   ${uni.quoteGasAdjusted.quotient.toString()}`);
    console.log(`gas units: ${uni.estimatedGasUsed.toString()}`);
    console.log(`route: ${routeAmountsToString(uni.route)}`);
    for (const leg of uniswapLegs(uni)) {
      console.log(`  ${leg.percent}% ${leg.protocol} amount=${leg.amount_in} quote=${leg.quote_out} gas=${leg.gas_estimate}`);
      console.log(`    path=${leg.token_path.map((t) => t.symbol).join('->')}`);
      console.log(`    pools=${leg.pools.join(',')}`);
    }

    console.log(`\nGROSS DELTA (Fynd - Uniswap): ${delta} raw`);
    if (prior.uniswap_gross && String(prior.uniswap_gross) !== uniGross.toString()) {
      console.log(`warning: forensic re-quote changed from prior Uniswap gross ${prior.uniswap_gross}`);
    }

    reports.push({
      order: row.order,
      pair: `${tokenIn.symbol}->${tokenOut.symbol}`,
      amount_in: row.amount_in,
      prior,
      fynd: {
        gross: fyndGross.toString(),
        net: fynd.amount_out_net_gas,
        gas: fynd.gas,
        edges: fyndEdges(fynd, metadata),
      },
      uniswap: {
        gross: uniGross.toString(),
        net: uni.quoteGasAdjusted.quotient.toString(),
        gas: uni.estimatedGasUsed.toString(),
        route_text: routeAmountsToString(uni.route),
        legs: uniswapLegs(uni),
      },
      gross_delta_fynd_minus_uniswap: delta.toString(),
    });
  }

  const outputPath = path.join(runDir, 'uniswap-counterexamples.json');
  fs.writeFileSync(outputPath, JSON.stringify({ block_number: comparison.block_number, reports }, null, 2) + '\n');
  console.log(`\nSaved forensic report: ${outputPath}`);
}

main().catch((error) => die(error.stack || error.message));
