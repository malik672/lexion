#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const { parse } = require('csv-parse/sync');
const { ethers } = require('ethers');
const JSBI = require('jsbi');
const { AlphaRouter } = require('@uniswap/smart-order-router');
const { CurrencyAmount, Ether, Token, TradeType } = require('@uniswap/sdk-core');
const { Protocol } = require('@uniswap/router-sdk');

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

function contextFromRunJson(run) {
  let blockNumber = null;
  let gasPriceGwei = null;
  let gasPriceWei = null;

  walk(run, (key, value) => {
    const normalized = key.toLowerCase();
    if (
      blockNumber == null &&
      (normalized === 'block' || normalized === 'block_number' || normalized === 'blocknumber') &&
      /^\d+$/.test(String(value))
    ) {
      blockNumber = Number(value);
    }

    if (gasPriceGwei == null && /gas.*price.*gwei|gas_price_gwei/.test(normalized)) {
      const candidate = Number(value);
      if (Number.isFinite(candidate) && candidate >= 0) gasPriceGwei = String(value);
    }

    if (gasPriceWei == null && /gas.*price.*wei|gas_price_wei|market_gas_price/.test(normalized)) {
      const text = String(value);
      if (/^\d+$/.test(text)) gasPriceWei = ethers.BigNumber.from(text);
    }
  });

  if (gasPriceGwei != null && gasPriceWei == null) {
    gasPriceWei = ethers.utils.parseUnits(gasPriceGwei, 'gwei');
  }
  if (gasPriceWei != null && gasPriceGwei == null) {
    gasPriceGwei = ethers.utils.formatUnits(gasPriceWei, 'gwei');
  }

  if (blockNumber == null || gasPriceWei == null) return null;
  return { blockNumber, gasPriceGwei, gasPriceWei };
}

function contextFromReport(reportText) {
  const block =
    reportText.match(/live[^\n\r]*?block[^0-9]*(\d{7,})/i) ||
    reportText.match(/block[^0-9]*(\d{7,})/i);
  const gas =
    reportText.match(/gas[^\n\r]*?price[^0-9]*([0-9]+(?:\.[0-9]+)?)\s*gwei/i) ||
    reportText.match(/([0-9]+(?:\.[0-9]+)?)\s*gwei/i);
  if (!block || !gas) return null;
  return {
    blockNumber: Number(block[1]),
    gasPriceGwei: gas[1],
    gasPriceWei: ethers.utils.parseUnits(gas[1], 'gwei'),
  };
}

function parseRunContext(runDir) {
  const runPath = path.join(runDir, 'run.json');
  if (fs.existsSync(runPath)) {
    try {
      const context = contextFromRunJson(JSON.parse(fs.readFileSync(runPath, 'utf8')));
      if (context) return context;
    } catch (error) {
      console.warn(`warning: could not parse ${runPath}: ${error.message}`);
    }
  }

  const reportPath = path.join(runDir, 'report.md');
  if (fs.existsSync(reportPath)) {
    const context = contextFromReport(fs.readFileSync(reportPath, 'utf8'));
    if (context) return context;
  }

  throw new Error(
    `could not find captured live block and gas price in ${runPath} or ${reportPath}`
  );
}

function protocolLabel(entry) {
  if (entry && typeof entry.protocol === 'string') return entry.protocol;
  const name = entry && entry.constructor && entry.constructor.name;
  return name || 'unknown';
}

function routeSummary(route) {
  if (!route || !Array.isArray(route.route)) return '';
  return route.route
    .map((entry) => {
      const percent = entry.percent != null ? `${entry.percent}%` : '?%';
      return `${percent}:${protocolLabel(entry)}`;
    })
    .join('|');
}

async function main() {
  const runDir = process.argv[2];
  if (!runDir) die('usage: node bench.cjs <bench-results/run-dir>');
  const rpcUrl = process.env.RPC_URL;
  if (!rpcUrl) die('RPC_URL is not set');

  const ordersPath = path.join(runDir, 'orders.csv');
  if (!fs.existsSync(ordersPath)) die(`${ordersPath} does not exist`);

  const tokenPath = path.resolve(__dirname, '../../fynd-core/benches/tokens.json');
  const metadata = JSON.parse(fs.readFileSync(tokenPath, 'utf8'));
  const rows = parse(fs.readFileSync(ordersPath, 'utf8'), { columns: true, skip_empty_lines: true });
  const hybridRows = rows.filter((row) => row.config === HYBRID_CONFIG && row.solved === 'true');
  if (!hybridRows.length) die(`no solved ${HYBRID_CONFIG} rows in ${ordersPath}`);

  const context = parseRunContext(runDir);
  const provider = new ethers.providers.JsonRpcProvider(rpcUrl, CHAIN_ID);
  const gasPriceProvider = {
    async getGasPrice() {
      return { gasPriceWei: context.gasPriceWei };
    },
  };
  const router = new AlphaRouter({ chainId: CHAIN_ID, provider, gasPriceProvider });

  console.log(`Uniswap SOR ${require('@uniswap/smart-order-router/package.json').version}`);
  console.log(`block:      ${context.blockNumber}`);
  console.log(`gas price:  ${context.gasPriceGwei} gwei`);
  console.log('protocols:  V2,V3,MIXED');
  console.log('max hops:   2');
  console.log(`orders:     ${hybridRows.length}\n`);

  const results = [];
  let wins = 0;
  let ties = 0;
  let losses = 0;
  let unavailable = 0;

  for (let index = 0; index < hybridRows.length; index++) {
    const row = hybridRows[index];
    const tokenIn = currency(row.token_in, metadata);
    const tokenOut = currency(row.token_out, metadata);
    const amount = CurrencyAmount.fromRawAmount(tokenIn, JSBI.BigInt(row.amount_in));
    const started = process.hrtime.bigint();

    try {
      const uni = await router.route(amount, tokenOut, TradeType.EXACT_INPUT, undefined, {
        blockNumber: context.blockNumber,
        protocols: [Protocol.V2, Protocol.V3, Protocol.MIXED],
        maxSwapsPerPath: 2,
        minSplits: 1,
        maxSplits: 4,
        distributionPercent: 5,
        forceCrossProtocol: false,
        useCachedRoutes: false,
      });
      const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6;
      if (!uni) {
        unavailable++;
        console.log(`[${index + 1}/${hybridRows.length}] ${tokenIn.symbol}->${tokenOut.symbol}: Uniswap no route`);
        results.push({ order: row.order, status: 'uniswap_no_route' });
        continue;
      }

      const uniGross = BigInt(uni.quote.quotient.toString());
      const uniNet = BigInt(uni.quoteGasAdjusted.quotient.toString());
      const fyndGross = BigInt(row.amount_out || row.net_out);
      const fyndNet = BigInt(row.net_out);
      const outcome = fyndNet > uniNet ? 'fynd' : fyndNet < uniNet ? 'uniswap' : 'tie';
      if (outcome === 'fynd') wins++;
      else if (outcome === 'uniswap') losses++;
      else ties++;

      const delta = fyndNet - uniNet;
      console.log(
        `[${index + 1}/${hybridRows.length}] ${tokenIn.symbol}->${tokenOut.symbol}: ` +
          `${outcome === 'fynd' ? 'FYND +' : outcome === 'uniswap' ? 'UNI +' : 'TIE '} ${delta < 0n ? -delta : delta} raw ` +
          `(uni ${elapsedMs.toFixed(0)}ms)`
      );

      results.push({
        order: row.order,
        token_in: row.token_in,
        token_out: row.token_out,
        amount_in: row.amount_in,
        fynd_gross: fyndGross.toString(),
        fynd_net: fyndNet.toString(),
        uniswap_gross: uniGross.toString(),
        uniswap_net: uniNet.toString(),
        uniswap_gas: uni.estimatedGasUsed.toString(),
        uniswap_gas_price_wei: context.gasPriceWei.toString(),
        delta_net: delta.toString(),
        winner: outcome,
        uniswap_elapsed_ms: elapsedMs,
        uniswap_route: routeSummary(uni),
      });
    } catch (error) {
      unavailable++;
      const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6;
      console.log(`[${index + 1}/${hybridRows.length}] ${tokenIn.symbol}->${tokenOut.symbol}: Uniswap error: ${error.message}`);
      results.push({ order: row.order, status: 'uniswap_error', error: error.message, uniswap_elapsed_ms: elapsedMs });
    }
  }

  const outputPath = path.join(runDir, 'uniswap-sor-comparison.json');
  fs.writeFileSync(
    outputPath,
    JSON.stringify(
      {
        block_number: context.blockNumber,
        gas_price_gwei: context.gasPriceGwei,
        uniswap_sor_version: require('@uniswap/smart-order-router/package.json').version,
        protocols: ['V2', 'V3', 'MIXED'],
        max_swaps_per_path: 2,
        max_splits: 4,
        distribution_percent: 5,
        compared: wins + ties + losses,
        fynd_wins: wins,
        ties,
        uniswap_wins: losses,
        unavailable,
        results,
      },
      null,
      2
    ) + '\n'
  );

  console.log('\nFynd hybrid vs Uniswap Smart Order Router');
  console.log(`compared:       ${wins + ties + losses}`);
  console.log(`Fynd wins:      ${wins}`);
  console.log(`ties:           ${ties}`);
  console.log(`Uniswap wins:   ${losses}`);
  console.log(`Uniswap misses: ${unavailable}`);
  console.log(`details:        ${outputPath}`);
}

main().catch((error) => die(error.stack || error.message));
