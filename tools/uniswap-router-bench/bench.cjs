#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const { parse } = require('csv-parse/sync');
const { ethers } = require('ethers');
const JSBI = require('jsbi');
const { AlphaRouter, SwapType, TO_PROTOCOL } = require('@uniswap/smart-order-router');
const { CurrencyAmount, Ether, Percent, Token, TradeType } = require('@uniswap/sdk-core');
const { Pool } = require('@uniswap/v3-sdk');

const ZERO = '0x0000000000000000000000000000000000000000';
const CHAIN_ID = 1;
const HYBRID_CONFIG = process.env.FYND_BENCH_CONFIG || 'path_frank_wolfe_d2';
const ORDER_FILTER = process.env.FYND_BENCH_ORDER;
const MEASURE_ACTUAL_GAS = process.env.UNISWAP_ACTUAL_GAS === '1';
const SIMULATION_SENDER = '0x0000000000000000000000000000000000000001';
const ERC20 = new ethers.utils.Interface([
  'function balanceOf(address) view returns (uint256)',
  'function allowance(address,address) view returns (uint256)',
]);
const PROBE_SENTINEL = ethers.utils.hexZeroPad('0xdeadbeef', 32);
// Keep packed high-bit token flags (for example USDC's blacklist bit) clear.
const LARGE_TOKEN_WORD = `0x${'00'.repeat(8)}${'ff'.repeat(24)}`;
const MAX_NATIVE_BALANCE = `0x${'ff'.repeat(32)}`;
const OZ_V5_BALANCES_NS = '0x52c63247e1f47db19d5ce0460030c497f067ca4cebf71ba98eeadabe20bace00';
const OZ_V5_ALLOWANCES_NS = '0x52c63247e1f47db19d5ce0460030c497f067ca4cebf71ba98eeadabe20bace01';

function mappingSlot(address, position) {
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'uint256'], [address, position])
  );
}

function nestedMappingSlot(owner, spender, position) {
  const inner = mappingSlot(owner, position);
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'bytes32'], [spender, inner])
  );
}

function mappingSlotAtBase(address, base) {
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'bytes32'], [address, base])
  );
}

function nestedMappingSlotAtBase(owner, spender, base) {
  const inner = mappingSlotAtBase(owner, base);
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'bytes32'], [spender, inner])
  );
}

async function findStorageSlot(provider, blockTag, token, calldata, slots) {
  for (const slot of slots) {
    const overrides = { [token]: { stateDiff: { [slot]: PROBE_SENTINEL } } };
    const result = await provider.send('eth_call', [{ to: token, data: calldata }, blockTag, overrides]);
    if (ethers.BigNumber.from(result).eq(PROBE_SENTINEL)) return slot;
  }
  throw new Error(`could not locate ERC-20 storage slot for ${token}`);
}

async function estimateActualSorGas(provider, blockNumber, tokenIn, amountIn, route) {
  if (!route.methodParameters) throw new Error('SOR did not return executable method parameters');
  const sender = SIMULATION_SENDER;
  const spender = route.methodParameters.to;
  const blockTag = ethers.utils.hexValue(blockNumber);
  const balanceCall = ERC20.encodeFunctionData('balanceOf', [sender]);
  const allowanceCall = ERC20.encodeFunctionData('allowance', [sender, spender]);
  const balanceSlot = await findStorageSlot(
    provider,
    blockTag,
    tokenIn.address,
    balanceCall,
    [...Array(21).keys()].map((position) => mappingSlot(sender, position)).concat(
      mappingSlotAtBase(sender, OZ_V5_BALANCES_NS)
    )
  );
  const allowanceSlot = await findStorageSlot(
    provider,
    blockTag,
    tokenIn.address,
    allowanceCall,
    [...Array(21).keys()].map((position) => nestedMappingSlot(sender, spender, position)).concat(
      nestedMappingSlotAtBase(sender, spender, OZ_V5_ALLOWANCES_NS)
    )
  );
  const overrides = {
    [sender]: { balance: MAX_NATIVE_BALANCE },
    [tokenIn.address]: {
      stateDiff: { [balanceSlot]: LARGE_TOKEN_WORD, [allowanceSlot]: LARGE_TOKEN_WORD },
    },
  };
  const transaction = {
    from: sender,
    to: spender,
    data: route.methodParameters.calldata,
    value: route.methodParameters.value,
  };
  const result = await provider.send('eth_estimateGas', [transaction, blockTag, overrides]);
  return BigInt(result);
}

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

function uniswapRouteSummary(route) {
  if (!route || !Array.isArray(route.route)) return '';
  return route.route
    .map((entry) => {
      const percent = entry.percent != null ? `${entry.percent}%` : '?%';
      return `${percent}:${protocolLabel(entry)}`;
    })
    .join('|');
}

function uniswapRouteDetails(route) {
  if (!route || !Array.isArray(route.route)) return [];
  return route.route.map((entry) => ({
    percent: entry.percent == null ? null : entry.percent,
    protocol: protocolLabel(entry),
    pools: Array.isArray(entry.route && entry.route.pools)
      ? entry.route.pools.map((pool) => ({
          address:
            pool.address ||
            pool.liquidityToken?.address ||
            (pool.token0 && pool.token1 && pool.fee != null
              ? Pool.getAddress(pool.token0, pool.token1, pool.fee)
              : null),
          token0: pool.token0?.address || null,
          token1: pool.token1?.address || null,
          fee: pool.fee == null ? null : pool.fee,
        }))
      : [],
    token_path: Array.isArray(entry.route && entry.route.tokenPath)
      ? entry.route.tokenPath.map((token) => token.address || 'ETH')
      : [],
  }));
}

function fyndRouteSummary(route) {
  if (!route || !Array.isArray(route.edges)) return '';
  return route.edges
    .map((edge) => {
      const protocol = edge.protocol || 'unknown';
      const pool = edge.component_id || edge.pool || '';
      return pool ? `${protocol}:${pool}` : protocol;
    })
    .join('|');
}

function findConfigRoute(line, configName) {
  const directContainers = [line.routes, line.configs, line.results, line.algorithms];
  for (const container of directContainers) {
    if (container && typeof container === 'object' && container[configName]) return container[configName];
  }
  let found = null;
  walk(line, (key, value) => {
    if (found == null && key === configName && value && typeof value === 'object') found = value;
  });
  return found;
}

function loadFyndRoutes(runDir) {
  const routesPath = path.join(runDir, 'routes.jsonl');
  if (!fs.existsSync(routesPath)) throw new Error(`${routesPath} does not exist`);
  const byOrder = new Map();
  for (const rawLine of fs.readFileSync(routesPath, 'utf8').split(/\r?\n/)) {
    if (!rawLine.trim()) continue;
    const line = JSON.parse(rawLine);
    const orderId = line.order && line.order.id != null ? String(line.order.id) : null;
    if (orderId == null) continue;
    const route = findConfigRoute(line, HYBRID_CONFIG);
    if (route) byOrder.set(orderId, route);
  }
  return byOrder;
}

function winner(a, b) {
  return a > b ? 'fynd' : a < b ? 'uniswap' : 'tie';
}

function abs(value) {
  return value < 0n ? -value : value;
}

function bpsDelta(a, b) {
  if (b === 0n) return null;
  // Preserve enough precision for tiny differences without converting the raw amounts to f64.
  const scaled = ((a - b) * 100000000n) / b;
  return Number(scaled) / 10000;
}

function formatBps(value) {
  if (value == null || !Number.isFinite(value)) return 'n/a';
  const sign = value > 0 ? '+' : '';
  return `${sign}${value.toFixed(4)}bps`;
}

function bump(score, outcome) {
  if (outcome === 'fynd') score.fynd++;
  else if (outcome === 'uniswap') score.uniswap++;
  else score.tie++;
}

async function main() {
  const runDir = process.argv[2];
  if (!runDir) die('usage: node bench.cjs <artifacts/benchmarks/runs/run-dir>');
  const rpcUrl = process.env.UNISWAP_RPC_URL || process.env.RPC_URL;
  if (!rpcUrl) die('UNISWAP_RPC_URL/RPC_URL is not set');

  const ordersPath = path.join(runDir, 'orders.csv');
  if (!fs.existsSync(ordersPath)) die(`${ordersPath} does not exist`);

  const tokenPath = path.resolve(__dirname, '../../fynd-core/benches/tokens.json');
  const metadata = JSON.parse(fs.readFileSync(tokenPath, 'utf8'));
  const rows = parse(fs.readFileSync(ordersPath, 'utf8'), { columns: true, skip_empty_lines: true });
  const hybridRows = rows.filter(
    (row) =>
      row.config === HYBRID_CONFIG &&
      row.solved === 'true' &&
      (!ORDER_FILTER || row.order === ORDER_FILTER)
  );
  if (!hybridRows.length) die(`no solved ${HYBRID_CONFIG} rows in ${ordersPath}`);

  const fyndRoutes = loadFyndRoutes(runDir);
  const context = parseRunContext(runDir);
  const provider = new ethers.providers.JsonRpcProvider(rpcUrl, CHAIN_ID);
  const simulationProvider = new ethers.providers.JsonRpcProvider(
    process.env.ACTUAL_GAS_RPC_URL || rpcUrl,
    CHAIN_ID
  );
  const gasPriceProvider = {
    async getGasPrice() {
      return { gasPriceWei: context.gasPriceWei };
    },
  };
  const router = new AlphaRouter({ chainId: CHAIN_ID, provider, gasPriceProvider });
  const protocols = [TO_PROTOCOL('v2'), TO_PROTOCOL('v3'), TO_PROTOCOL('mixed')];

  console.log(`Uniswap SOR ${require('@uniswap/smart-order-router/package.json').version}`);
  console.log(`node:       ${process.version}`);
  console.log(`block:      ${context.blockNumber}`);
  console.log(`gas price:  ${context.gasPriceGwei} gwei`);
  console.log('protocols:  V2,V3,MIXED');
  console.log('max hops:   2');
  console.log(`orders:     ${hybridRows.length}\n`);

  const results = [];
  const grossScore = { fynd: 0, tie: 0, uniswap: 0 };
  const netScore = { fynd: 0, tie: 0, uniswap: 0 };
  let unavailable = 0;
  let missingFyndDetail = 0;

  for (let index = 0; index < hybridRows.length; index++) {
    const row = hybridRows[index];
    const tokenIn = currency(row.token_in, metadata);
    const tokenOut = currency(row.token_out, metadata);
    const amount = CurrencyAmount.fromRawAmount(tokenIn, JSBI.BigInt(row.amount_in));
    const fyndRoute = fyndRoutes.get(String(row.order));
    const started = process.hrtime.bigint();

    if (!fyndRoute || !fyndRoute.amount_out || !fyndRoute.amount_out_net_gas) {
      missingFyndDetail++;
      console.log(
        `[${index + 1}/${hybridRows.length}] ${tokenIn.symbol}->${tokenOut.symbol}: ` +
          `missing Fynd gross/gas detail in routes.jsonl`
      );
      results.push({ order: row.order, status: 'missing_fynd_route_detail' });
      continue;
    }

    try {
      const block = await provider.getBlock(context.blockNumber);
      const swapConfig = MEASURE_ACTUAL_GAS
        ? {
            type: SwapType.SWAP_ROUTER_02,
            recipient: SIMULATION_SENDER,
            slippageTolerance: new Percent(50, 10_000),
            deadline: block.timestamp + 1_200,
          }
        : undefined;
      const uni = await router.route(amount, tokenOut, TradeType.EXACT_INPUT, swapConfig, {
        blockNumber: context.blockNumber,
        protocols,
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

      const fyndGross = BigInt(fyndRoute.amount_out);
      const fyndNet = BigInt(fyndRoute.amount_out_net_gas);
      const fyndGasUnits = fyndRoute.gas != null ? BigInt(fyndRoute.gas) : null;
      const fyndGasCostQuote = fyndGross - fyndNet;

      const uniGross = BigInt(uni.quote.quotient.toString());
      const uniNet = BigInt(uni.quoteGasAdjusted.quotient.toString());
      const uniGasUnits = BigInt(uni.estimatedGasUsed.toString());
      const uniActualGasUnits = MEASURE_ACTUAL_GAS
        ? await estimateActualSorGas(
            simulationProvider,
            context.blockNumber,
            tokenIn,
            amount.quotient.toString(),
            uni
          )
        : null;
      const uniGasCostQuote = uniGross - uniNet;

      const grossWinner = winner(fyndGross, uniGross);
      const netWinner = winner(fyndNet, uniNet);
      bump(grossScore, grossWinner);
      bump(netScore, netWinner);

      const grossDelta = fyndGross - uniGross;
      const netDelta = fyndNet - uniNet;
      const gasCostDelta = fyndGasCostQuote - uniGasCostQuote;
      const grossBps = bpsDelta(fyndGross, uniGross);
      const netBps = bpsDelta(fyndNet, uniNet);

      const grossLabel = grossWinner === 'tie' ? 'TIE' : grossWinner === 'fynd' ? 'FYND' : 'UNI';
      const netLabel = netWinner === 'tie' ? 'TIE' : netWinner === 'fynd' ? 'FYND' : 'UNI';
      console.log(
        `[${index + 1}/${hybridRows.length}] ${tokenIn.symbol}->${tokenOut.symbol}: ` +
          `gross=${grossLabel} ${formatBps(grossBps)} | net=${netLabel} ${formatBps(netBps)} | ` +
          `gasCostDelta=${gasCostDelta.toString()} raw (uni ${elapsedMs.toFixed(0)}ms)`
      );

      results.push({
        order: row.order,
        token_in: row.token_in,
        token_out: row.token_out,
        amount_in: row.amount_in,
        fynd_gross: fyndGross.toString(),
        fynd_net: fyndNet.toString(),
        fynd_gas_units: fyndGasUnits == null ? null : fyndGasUnits.toString(),
        fynd_gas_cost_quote_raw: fyndGasCostQuote.toString(),
        fynd_route: fyndRouteSummary(fyndRoute),
        uniswap_gross: uniGross.toString(),
        uniswap_net: uniNet.toString(),
        uniswap_gas_units: uniGasUnits.toString(),
        uniswap_actual_gas_units: uniActualGasUnits == null ? null : uniActualGasUnits.toString(),
        uniswap_gas_cost_quote_raw: uniGasCostQuote.toString(),
        uniswap_gas_price_wei: context.gasPriceWei.toString(),
        uniswap_route: uniswapRouteSummary(uni),
        uniswap_route_details: uniswapRouteDetails(uni),
        gross_delta_fynd_minus_uniswap: grossDelta.toString(),
        net_delta_fynd_minus_uniswap: netDelta.toString(),
        gas_cost_delta_fynd_minus_uniswap_quote_raw: gasCostDelta.toString(),
        gross_bps_fynd_vs_uniswap: grossBps,
        net_bps_fynd_vs_uniswap: netBps,
        gross_winner: grossWinner,
        net_winner: netWinner,
        winner_changed_after_gas: grossWinner !== netWinner,
        absolute_gross_delta_raw: abs(grossDelta).toString(),
        absolute_net_delta_raw: abs(netDelta).toString(),
        uniswap_elapsed_ms: elapsedMs,
      });
    } catch (error) {
      unavailable++;
      const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6;
      console.log(`[${index + 1}/${hybridRows.length}] ${tokenIn.symbol}->${tokenOut.symbol}: Uniswap error: ${error.message}`);
      results.push({ order: row.order, status: 'uniswap_error', error: error.message, uniswap_elapsed_ms: elapsedMs });
    }
  }

  const comparable = grossScore.fynd + grossScore.tie + grossScore.uniswap;
  const changedAfterGas = results.filter((r) => r.gross_winner && r.gross_winner !== r.net_winner).length;
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
        compared: comparable,
        gross_score: grossScore,
        net_score: netScore,
        winner_changed_after_gas: changedAfterGas,
        uniswap_unavailable: unavailable,
        missing_fynd_route_detail: missingFyndDetail,
        results,
      },
      null,
      2
    ) + '\n'
  );

  console.log('\nFynd hybrid vs Uniswap Smart Order Router');
  console.log(`comparable:                ${comparable}`);
  console.log(`gross routing quality:     Fynd ${grossScore.fynd} | tie ${grossScore.tie} | Uniswap ${grossScore.uniswap}`);
  console.log(`net after gas:             Fynd ${netScore.fynd} | tie ${netScore.tie} | Uniswap ${netScore.uniswap}`);
  console.log(`winner changed after gas:  ${changedAfterGas}`);
  console.log(`Uniswap misses/errors:     ${unavailable}`);
  console.log(`missing Fynd route detail: ${missingFyndDetail}`);
  console.log(`details:                   ${outputPath}`);
}

main().catch((error) => die(error.stack || error.message));
