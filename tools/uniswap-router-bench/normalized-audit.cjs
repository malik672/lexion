#!/usr/bin/env node

// Compare replayed Fynd quotes from `fynd-benchmark audit` with Uniswap SOR transactions.
// Both sides are charged measured gas using the same output-token-per-gas conversion.

const fs = require('fs');
const path = require('path');
const { ethers } = require('ethers');
const JSBI = require('jsbi');
const { AlphaRouter, SwapType, TO_PROTOCOL } = require('@uniswap/smart-order-router');
const { CurrencyAmount, Percent, Token, TradeType } = require('@uniswap/sdk-core');
const { UniversalRouterVersion } = require('@uniswap/universal-router-sdk');

const CHAIN_ID = 1;
const SENDER = '0x000000000000000000000000000000000000dEaD';
const ERC20 = new ethers.utils.Interface([
  'function balanceOf(address) view returns (uint256)',
  'function allowance(address,address) view returns (uint256)',
]);
const PROBE = ethers.utils.hexZeroPad('0xdeadbeef', 32);
const LARGE_WORD = `0x${'00'.repeat(8)}${'ff'.repeat(24)}`;
const LARGE_BALANCE = `0x${'ff'.repeat(32)}`;
const LARGE_PERMIT2_ALLOWANCE = ethers.utils.hexZeroPad(
  ethers.BigNumber.from(2).pow(208).sub(1).toHexString(),
  32
);
const PERMIT2 = '0x000000000022D473030F116dDEE9F6B43aC78BA3';
const BALANCES_NS = '0x52c63247e1f47db19d5ce0460030c497f067ca4cebf71ba98eeadabe20bace00';
const ALLOWANCES_NS = '0x52c63247e1f47db19d5ce0460030c497f067ca4cebf71ba98eeadabe20bace01';
const storageSlots = new Map();
const USD_PRICES = new Map([
  ['0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2', 2500],
  ['0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48', 1],
  ['0xdac17f958d2ee523a2206206994597c13d831ec7', 1],
  ['0x6b175474e89094c44da98b954eedeac495271d0f', 1],
  ['0x2260fac5e5542a773aa44fbcfedf7c193bc2c599', 95000],
  ['0x7fc66500c84a76ad7e9c93437bfc5ac33e2ddae9', 200],
  ['0x1f9840a85d5af5bf1d1762f925bdaddc4201f984', 10],
  ['0x514910771af9ca656af840dff83e8264ecf986ca', 15],
]);

function die(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}

function mappingSlot(address, position) {
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'uint256'], [address, position])
  );
}

function nestedMappingSlot(owner, spender, position) {
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'bytes32'], [spender, mappingSlot(owner, position)])
  );
}

function mappingSlotAtBase(address, base) {
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'bytes32'], [address, base])
  );
}

function nestedMappingSlotAtBase(owner, spender, base) {
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(
      ['address', 'bytes32'],
      [spender, mappingSlotAtBase(owner, base)]
    )
  );
}

function tripleMappingSlot(owner, token, spender, position) {
  const ownerSlot = mappingSlot(owner, position);
  const tokenSlot = ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'bytes32'], [token, ownerSlot])
  );
  return ethers.utils.keccak256(
    ethers.utils.defaultAbiCoder.encode(['address', 'bytes32'], [spender, tokenSlot])
  );
}

async function findStorageSlot(provider, blockTag, token, calldata, slots) {
  const key = `${blockTag}:${token.toLowerCase()}:${calldata}`;
  const cached = storageSlots.get(key);
  if (cached) return cached;
  for (const slot of slots) {
    const overrides = { [token]: { stateDiff: { [slot]: PROBE } } };
    const result = await provider.send('eth_call', [{ to: token, data: calldata }, blockTag, overrides]);
    if (ethers.BigNumber.from(result).eq(PROBE)) {
      storageSlots.set(key, slot);
      return slot;
    }
  }
  throw new Error(`could not locate ERC-20 storage slot for ${token}`);
}

async function findPermit2Slot(provider, blockTag, token, spender) {
  const permit2 = new ethers.utils.Interface([
    'function allowance(address,address,address) view returns (uint160 amount,uint48 expiration,uint48 nonce)',
  ]);
  const calldata = permit2.encodeFunctionData('allowance', [SENDER, token, spender]);
  const positions = [...Array(11).keys()];
  const key = `${blockTag}:${PERMIT2.toLowerCase()}:${calldata}`;
  const cached = storageSlots.get(key);
  if (cached) return cached;
  for (const slot of positions.map((position) =>
    tripleMappingSlot(SENDER, token, spender, position)
  )) {
    const overrides = { [PERMIT2]: { stateDiff: { [slot]: LARGE_PERMIT2_ALLOWANCE } } };
    const result = await provider.send(
      'eth_call',
      [{ to: PERMIT2, data: calldata }, blockTag, overrides]
    );
    const [amount] = permit2.decodeFunctionResult('allowance', result);
    if (amount.eq(ethers.BigNumber.from(2).pow(160).sub(1))) {
      storageSlots.set(key, slot);
      return slot;
    }
  }
  throw new Error(`could not locate Permit2 allowance slot for ${token}`);
}

async function estimateTransactionGas(
  provider,
  blockNumber,
  tokenIn,
  amountIn,
  transaction,
  usePermit2 = false
) {
  const spender = usePermit2 ? PERMIT2 : transaction.to;
  const blockTag = ethers.utils.hexValue(blockNumber);
  const balanceCall = ERC20.encodeFunctionData('balanceOf', [SENDER]);
  const allowanceCall = ERC20.encodeFunctionData('allowance', [SENDER, spender]);
  const positions = [...Array(21).keys()];
  const balanceSlot = await findStorageSlot(
    provider,
    blockTag,
    tokenIn.address,
    balanceCall,
    positions.map((position) => mappingSlot(SENDER, position)).concat(
      mappingSlotAtBase(SENDER, BALANCES_NS)
    )
  );
  const allowanceSlot = await findStorageSlot(
    provider,
    blockTag,
    tokenIn.address,
    allowanceCall,
    positions.map((position) => nestedMappingSlot(SENDER, spender, position)).concat(
      nestedMappingSlotAtBase(SENDER, spender, ALLOWANCES_NS)
    )
  );
  const overrides = {
    [SENDER]: { balance: LARGE_BALANCE },
    [tokenIn.address]: {
      stateDiff: { [balanceSlot]: LARGE_WORD, [allowanceSlot]: LARGE_WORD },
    },
  };
  if (usePermit2) {
    const permit2Slot = await findPermit2Slot(
      provider,
      blockTag,
      tokenIn.address,
      transaction.to
    );
    overrides[PERMIT2] = { stateDiff: { [permit2Slot]: LARGE_PERMIT2_ALLOWANCE } };
  }
  const tx = {
    from: SENDER,
    to: transaction.to,
    data: transaction.calldata,
    value: transaction.value,
  };
  return BigInt(await provider.send('eth_estimateGas', [tx, blockTag, overrides]));
}

function token(address, metadata) {
  const item = metadata[address.toLowerCase()];
  if (!item) throw new Error(`missing token metadata for ${address}`);
  return new Token(CHAIN_ID, address, Number(item[1]), item[0]);
}

function winner(a, b) {
  return a > b ? 'fynd' : a < b ? 'uniswap' : 'tie';
}

function approximateUsdDelta(rawDelta, outputToken) {
  const price = USD_PRICES.get(outputToken.address.toLowerCase());
  if (price == null) return null;
  return Number(ethers.utils.formatUnits(rawDelta < 0n ? -rawDelta : rawDelta, outputToken.decimals)) * price;
}

function economicWinner(fyndNet, uniswapNet, outputToken) {
  const deltaUsd = approximateUsdDelta(fyndNet - uniswapNet, outputToken);
  if (deltaUsd == null) return { winner: 'unpriced', deltaUsd: null };
  if (deltaUsd <= 0.02) return { winner: 'tie', deltaUsd };
  return { winner: winner(fyndNet, uniswapNet), deltaUsd };
}

function bump(score, value) {
  score[value]++;
}

function bps(a, b) {
  if (b === 0n) return null;
  return Number(((a - b) * 100000000n) / b) / 10000;
}

function sharedGasCost(gas, quotedCost, quotedGas) {
  if (quotedGas === 0n) throw new Error('Fynd reported zero estimated gas');
  return gas * quotedCost / quotedGas;
}

async function withTimeout(promise, timeoutMs, label) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} timed out after ${timeoutMs}ms`)), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function main() {
  const auditPath = process.argv[2];
  if (!auditPath) die('usage: node normalized-audit.cjs <fynd-audit.json>');
  const rpcUrl = process.env.UNISWAP_RPC_URL || process.env.RPC_URL;
  if (!rpcUrl) die('UNISWAP_RPC_URL or RPC_URL is required');

  const audit = JSON.parse(fs.readFileSync(auditPath, 'utf8'));
  const metadata = JSON.parse(
    fs.readFileSync(path.resolve(__dirname, '../../fynd-core/benches/tokens.json'), 'utf8')
  );
  const provider = new ethers.providers.JsonRpcProvider(rpcUrl, CHAIN_ID);
  const router = new AlphaRouter({ chainId: CHAIN_ID, provider });
  const routeTimeoutMs = Number(process.env.UNISWAP_ROUTE_TIMEOUT_MS || 180_000);
  const protocolFamilies = [
    {
      name: 'v2/v3/mixed',
      protocols: [TO_PROTOCOL('v2'), TO_PROTOCOL('v3'), TO_PROTOCOL('mixed')],
      usePermit2: false,
    },
    { name: 'v4', protocols: [TO_PROTOCOL('v4')], usePermit2: true },
  ];
  const grossScore = { fynd: 0, tie: 0, uniswap: 0 };
  const netScore = { fynd: 0, tie: 0, uniswap: 0 };
  const economicScore = { fynd: 0, tie: 0, uniswap: 0, unpriced: 0 };
  const results = [];

  for (const [index, trade] of audit.results.entries()) {
    const fynd = trade.participants.find((participant) => participant.name === 'fynd');
    if (!fynd || fynd.status !== 'success' || !fynd.eth_call_amount_out || !fynd.eth_call_gas_used) {
      results.push({ order: index, status: 'missing_replayed_fynd' });
      continue;
    }
    if (!fynd.amount_out || !fynd.amount_out_net_gas || !fynd.gas_units || !fynd.calldata || !trade.block_hash) {
      results.push({ order: index, status: 'missing_fynd_gas_conversion' });
      continue;
    }

    try {
      const block = await provider.getBlock(trade.block_hash);
      if (!block) throw new Error(`block ${trade.block_hash} is unavailable`);
      const tokenIn = token(trade.token_in, metadata);
      const tokenOut = token(trade.token_out, metadata);
      const amount = CurrencyAmount.fromRawAmount(tokenIn, JSBI.BigInt(trade.amount_in));
      const started = process.hrtime.bigint();
      const routeAttempts = await Promise.allSettled(
        protocolFamilies.map(async (family) => {
          const swapOptions = family.usePermit2
            ? {
                type: SwapType.UNIVERSAL_ROUTER,
                version: UniversalRouterVersion.V2_0,
                recipient: SENDER,
                slippageTolerance: new Percent(50, 10_000),
                deadline: block.timestamp + 1_200,
              }
            : {
                type: SwapType.SWAP_ROUTER_02,
                recipient: SENDER,
                slippageTolerance: new Percent(50, 10_000),
                deadline: block.timestamp + 1_200,
              };
          const route = await withTimeout(
            router.route(amount, tokenOut, TradeType.EXACT_INPUT, swapOptions, {
              blockNumber: block.number,
              protocols: family.protocols,
              maxSwapsPerPath: 2,
              minSplits: 1,
              maxSplits: 4,
              distributionPercent: 5,
              forceCrossProtocol: false,
              useCachedRoutes: false,
            }),
            routeTimeoutMs,
            `Uniswap ${family.name} route`
          );
          return { route, family };
        })
      );
      const routes = routeAttempts
        .filter((attempt) => attempt.status === 'fulfilled' && attempt.value.route?.methodParameters)
        .map((attempt) => attempt.value);
      const selected = routes.reduce((best, candidate) =>
        !best || candidate.route.quote.greaterThan(best.route.quote) ? candidate : best
      , null);
      const route = selected?.route;
      if (!route || !route.methodParameters) throw new Error('Uniswap returned no executable route');
      const uniswapGas = await estimateTransactionGas(
        provider,
        block.number,
        tokenIn,
        trade.amount_in,
        route.methodParameters,
        selected.family.usePermit2
      );
      const fyndGas = await estimateTransactionGas(
        provider,
        block.number,
        tokenIn,
        trade.amount_in,
        {
          to: fynd.calldata.to,
          calldata: fynd.calldata.data,
          value: fynd.calldata.value,
        }
      );
      const elapsedMs = Number(process.hrtime.bigint() - started) / 1e6;

      const fyndGross = BigInt(fynd.eth_call_amount_out);
      const uniswapGross = BigInt(route.quote.quotient.toString());
      const fyndQuotedGross = BigInt(fynd.amount_out);
      const fyndQuotedNet = BigInt(fynd.amount_out_net_gas);
      const fyndQuotedGas = BigInt(fynd.gas_units);
      const quotedCost = fyndQuotedGross - fyndQuotedNet;
      if (quotedCost < 0n) throw new Error('Fynd net output exceeds gross output');
      const fyndNet = fyndGross - sharedGasCost(fyndGas, quotedCost, fyndQuotedGas);
      const uniswapNet = uniswapGross - sharedGasCost(uniswapGas, quotedCost, fyndQuotedGas);
      const grossWinner = winner(fyndGross, uniswapGross);
      const netWinner = winner(fyndNet, uniswapNet);
      const economic = economicWinner(fyndNet, uniswapNet, tokenOut);
      bump(grossScore, grossWinner);
      bump(netScore, netWinner);
      bump(economicScore, economic.winner);

      results.push({
        order: index,
        block_number: block.number,
        token_in: trade.token_in,
        token_out: trade.token_out,
        amount_in: trade.amount_in,
        fynd_replayed_output: fyndGross.toString(),
        fynd_estimated_gas: fyndGas.toString(),
        fynd_simulated_gas: fynd.eth_call_gas_used,
        fynd_normalized_net: fyndNet.toString(),
        uniswap_output: uniswapGross.toString(),
        uniswap_protocol_family: selected.family.name,
        uniswap_estimated_gas: uniswapGas.toString(),
        uniswap_normalized_net: uniswapNet.toString(),
        gross_winner: grossWinner,
        normalized_net_winner: netWinner,
        economic_winner_2_cent_cutoff: economic.winner,
        approximate_net_delta_usd: economic.deltaUsd,
        gross_bps_fynd_vs_uniswap: bps(fyndGross, uniswapGross),
        normalized_net_bps_fynd_vs_uniswap: bps(fyndNet, uniswapNet),
        uniswap_elapsed_ms: elapsedMs,
      });
      console.log(
        `[${index + 1}/${audit.results.length}] ${tokenIn.symbol}->${tokenOut.symbol}: ` +
          `gross=${grossWinner} net=${netWinner} ` +
          `gas=${fyndGas}/${uniswapGas} uni=${elapsedMs.toFixed(0)}ms`
      );
    } catch (error) {
      results.push({ order: index, status: 'error', error: error.message });
      console.log(`[${index + 1}/${audit.results.length}] error: ${error.message}`);
    }
  }

  const compared = grossScore.fynd + grossScore.tie + grossScore.uniswap;
  const outputPath = auditPath.replace(/\.json$/i, '') + '-uniswap-normalized.json';
  const output = {
    source_audit: auditPath,
    compared,
    gross_score: grossScore,
    normalized_net_score: netScore,
    economic_score_2_cent_cutoff: economicScore,
    gas_semantics: {
      fynd: 'eth_estimateGas at the pinned block with injected sender balance and allowance',
      uniswap: 'eth_estimateGas at the same pinned block with identical sender-state treatment',
      conversion: 'one Lexion-derived output-token-per-gas rate applied to both estimated gas counts',
      materiality: 'net advantages of at most $0.02 are ties; outputs without a configured USD price are unpriced and excluded',
    },
    results,
  };
  fs.writeFileSync(outputPath, JSON.stringify(output, null, 2) + '\n');
  console.log('\nMeasured-gas Fynd vs Uniswap SOR');
  console.log(`comparable:      ${compared}`);
  console.log(`replayed gross:  Fynd ${grossScore.fynd} | tie ${grossScore.tie} | Uniswap ${grossScore.uniswap}`);
  console.log(`normalized net:  Fynd ${netScore.fynd} | tie ${netScore.tie} | Uniswap ${netScore.uniswap}`);
  console.log(
    `economic >$0.02: Fynd ${economicScore.fynd} | tie ${economicScore.tie} | ` +
      `Uniswap ${economicScore.uniswap} | unpriced ${economicScore.unpriced}`
  );
  console.log(`details:         ${outputPath}`);
}

main().catch((error) => die(error.stack || error.message));
