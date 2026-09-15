//! Unsigned Tycho Router transaction construction and RPC validation.

use std::{collections::HashSet, error::Error, sync::Arc};

use alloy::{
    primitives::{Address, U256, keccak256},
    sol_types::SolValue,
};
use num_bigint::BigUint;
use serde_json::{Value, json};
use tycho_execution::encoding::{
    evm::{
        encoder_builders::TychoRouterEncoderBuilder,
        swap_encoder::swap_encoder_registry::SwapEncoderRegistry,
    },
    models::{Solution as ExecutionSolution, Swap, UserTransferType},
};
use tycho_simulation::tycho_common::{Bytes, models::Chain};

use crate::{Amount, PoolId, market::MarketSnapshot, router::PathSet, solver::Solution};

#[derive(Clone, Debug)]
pub struct UnsignedTransaction {
    pub to: Address,
    pub from: Address,
    pub value: Amount,
    pub data: Vec<u8>,
    pub expected_amount_out: Amount,
    pub min_amount_out: Amount,
    pub estimated_gas: Amount,
}

/// Encodes a solved ERC-20 route through the deployed Tycho Router.
///
/// V1 deliberately rejects portfolios sharing pools. Such routes require merged-state replay;
/// emitting two independently simulated calls would be unsafe.
pub fn encode_unsigned_transaction(
    solution: &Solution,
    paths: &PathSet,
    snapshot: &MarketSnapshot,
    token_in: Address,
    token_out: Address,
    sender: Address,
    recipient: Address,
    slippage_bps: u16,
) -> Result<UnsignedTransaction, Box<dyn Error>> {
    if slippage_bps > 2_000 {
        return Err("slippage exceeds the Tycho Router 20% guardrail".into());
    }
    if token_in.is_zero() || token_out.is_zero() {
        return Err("native-token transaction encoding is not supported by this CLI yet".into());
    }

    let mut allocations = solution.allocations.iter().collect::<Vec<_>>();
    allocations.sort_unstable_by(|left, right| right.amount_in.cmp(&left.amount_in));
    let mut used_pools = HashSet::<PoolId>::new();
    for allocation in &allocations {
        for hop in &paths.paths[allocation.path_index].hops {
            if !used_pools.insert(hop.pool) {
                return Err(
                    "selected paths share a pool; merged execution encoding is required".into(),
                );
            }
        }
    }

    struct PreparedSwap {
        depth: usize,
        allocation: usize,
        swap: Swap,
    }
    let mut prepared = Vec::new();
    for (allocation_index, allocation) in allocations.iter().enumerate() {
        let path = &paths.paths[allocation.path_index];
        let mut current = allocation.amount_in;
        for (depth, hop) in path.hops.iter().enumerate() {
            let pool = snapshot
                .pool(hop.pool)
                .as_tycho()
                .ok_or("synthetic pools cannot be transaction-encoded")?;
            let (component, input, output, state) = pool
                .execution_parts(hop.token_in)
                .ok_or("pool is missing live execution metadata")?;
            let result = state
                .get_amount_out(amount_to_biguint(current), &input, &output)
                .map_err(|error| format!("failed to replay executable hop: {error}"))?;
            let split = if depth == 0 && allocation_index + 1 < allocations.len() {
                amount_ratio(allocation.amount_in, solution.amount_in)
            } else {
                0.0
            };
            prepared.push(PreparedSwap {
                depth,
                allocation: allocation_index,
                swap: Swap::new(component, input, output, result.gas.clone())
                    .with_split(split)
                    .with_protocol_state(Arc::from(state))
                    .with_estimated_amount_in(amount_to_biguint(current)),
            });
            current = biguint_to_amount(&result.amount)?;
        }
    }
    prepared.sort_by_key(|swap| (swap.depth, swap.allocation));
    let swaps = prepared.into_iter().map(|swap| swap.swap).collect();

    let minimum = solution.amount_out * Amount::from(10_000_u64 - u64::from(slippage_bps))
        / Amount::from(10_000_u64);
    let execution_solution = ExecutionSolution::new(
        Bytes::from(sender.as_slice()),
        Bytes::from(recipient.as_slice()),
        Bytes::from(token_in.as_slice()),
        Bytes::from(token_out.as_slice()),
        amount_to_biguint(solution.amount_in),
        amount_to_biguint(solution.amount_out),
        amount_to_biguint(minimum),
        swaps,
    )
    .with_user_transfer_type(UserTransferType::TransferFrom);

    let registry = SwapEncoderRegistry::new_with_defaults(Chain::Ethereum)?;
    let encoder = TychoRouterEncoderBuilder::new()
        .chain(Chain::Ethereum)
        .swap_encoder_registry(registry)
        .build()?;
    let encoded = encoder.encode_solutions(vec![execution_solution])?;
    let encoded = encoded
        .into_iter()
        .next()
        .ok_or("encoder returned no solution")?;
    let client_fee = (
        0_u32,
        Address::ZERO,
        U256::ZERO,
        U256::MAX,
        Vec::<u8>::new(),
    );
    let arguments = if encoded.function_signature().contains("splitSwap") {
        (
            solution.amount_in,
            token_in,
            token_out,
            solution.amount_out,
            minimum,
            U256::from(encoded.n_tokens()),
            recipient,
            client_fee,
            encoded.swaps().to_vec(),
        )
            .abi_encode()
    } else {
        (
            solution.amount_in,
            token_in,
            token_out,
            solution.amount_out,
            minimum,
            recipient,
            client_fee,
            encoded.swaps().to_vec(),
        )
            .abi_encode()
    };
    let hash = keccak256(encoded.function_signature().as_bytes());
    let mut data = hash[..4].to_vec();
    data.extend(arguments);
    Ok(UnsignedTransaction {
        to: Address::from_slice(encoded.interacting_with()),
        from: sender,
        value: Amount::ZERO,
        data,
        expected_amount_out: solution.amount_out,
        min_amount_out: minimum,
        estimated_gas: biguint_to_amount(encoded.estimated_gas())?,
    })
}

pub async fn dry_run(
    client: &reqwest::Client,
    rpc_url: &str,
    transaction: &UnsignedTransaction,
    block: u64,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let response: Value = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_call",
            "params": [{
                "from": transaction.from,
                "to": transaction.to,
                "data": format!("0x{}", alloy::hex::encode(&transaction.data)),
                "value": format!("0x{:x}", transaction.value),
            }, format!("0x{block:x}")]
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(error) = response.get("error") {
        return Err(format!("eth_call reverted: {error}").into());
    }
    let result = response["result"]
        .as_str()
        .ok_or("RPC response has no result")?;
    Ok(alloy::hex::decode(result.trim_start_matches("0x"))?)
}

fn amount_to_biguint(amount: Amount) -> BigUint {
    BigUint::from_bytes_be(&amount.to_be_bytes::<32>())
}

fn biguint_to_amount(amount: &BigUint) -> Result<Amount, Box<dyn Error>> {
    let bytes = amount.to_bytes_be();
    if bytes.len() > 32 {
        return Err("amount exceeds uint256".into());
    }
    Ok(Amount::from_be_slice(&bytes))
}

fn amount_ratio(part: Amount, total: Amount) -> f64 {
    let part = part.to_string().parse::<f64>().unwrap_or_default();
    let total = total.to_string().parse::<f64>().unwrap_or(1.0);
    part / total
}
