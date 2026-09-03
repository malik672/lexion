//! Test scenario types and loaders.

use fynd_core::types::{Order, OrderSide};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use tycho_simulation::tycho_common::models::Address;

/// A test scenario: a single token swap to quote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestScenario {
    /// Input token address.
    pub token_in: Address,
    /// Output token address.
    pub token_out: Address,
    /// Swap amount in token's smallest unit.
    pub amount: BigUint,
    /// Buy or sell.
    pub side: OrderSide,
    /// Human-readable scenario name (e.g. `"WETH_to_USDC_500"`).
    pub name: String,
}

impl TestScenario {
    /// Convert to an [`Order`] for quoting.
    pub fn to_order(&self) -> Order {
        Order::new(
            self.token_in.clone(),
            self.token_out.clone(),
            self.amount.clone(),
            self.side,
            Address::zero(20),
        )
    }
}

/// Load test scenarios from a pairs JSON string.
///
/// The JSON must have `tokens` (array of `{symbol, address, decimals}`)
/// and `pairs` (array of `{token_in, token_out, amounts: [f64]}`).
///
/// Takes the first amount per pair for a representative subset.
pub fn load_test_scenarios(pairs_json: &str) -> anyhow::Result<Vec<TestScenario>> {
    let raw: serde_json::Value = serde_json::from_str(pairs_json)?;

    let tokens_arr = raw["tokens"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing 'tokens' array in pairs JSON"))?;

    let tokens: std::collections::HashMap<String, (Address, u32)> = tokens_arr
        .iter()
        .map(|t| {
            let symbol = t["symbol"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("token missing 'symbol'"))?
                .to_string();
            let address: Address = t["address"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("token {symbol} missing 'address'"))?
                .parse()
                .map_err(|e| anyhow::anyhow!("token {symbol}: invalid address: {e}"))?;
            let decimals = t["decimals"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("token {symbol} missing 'decimals'"))?
                as u32;
            Ok((symbol, (address, decimals)))
        })
        .collect::<anyhow::Result<_>>()?;

    let pairs_arr = raw["pairs"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing 'pairs' array in pairs JSON"))?;

    pairs_arr
        .iter()
        .map(|pair| {
            let token_in_sym = pair["token_in"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("pair missing 'token_in'"))?;
            let token_out_sym = pair["token_out"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("pair missing 'token_out'"))?;
            let (token_in, decimals_in) = tokens
                .get(token_in_sym)
                .ok_or_else(|| anyhow::anyhow!("unknown token: {token_in_sym}"))?;
            let (token_out, _) = tokens
                .get(token_out_sym)
                .ok_or_else(|| anyhow::anyhow!("unknown token: {token_out_sym}"))?;

            let human_amount = pair["amounts"][0]
                .as_f64()
                .ok_or_else(|| {
                    anyhow::anyhow!("pair {token_in_sym}/{token_out_sym} missing amount")
                })?;
            // f64 precision is sufficient for human-readable test amounts
            let raw_amount = human_amount * 10_f64.powi(*decimals_in as i32);
            if !raw_amount.is_finite() || raw_amount < 0.0 {
                anyhow::bail!(
                    "pair {token_in_sym}/{token_out_sym}: \
                     computed amount is not a valid positive number ({raw_amount})"
                );
            }

            Ok(TestScenario {
                name: format!("{token_in_sym}_to_{token_out_sym}_{human_amount}"),
                token_in: token_in.clone(),
                token_out: token_out.clone(),
                amount: BigUint::from(raw_amount as u128),
                side: OrderSide::Sell,
            })
        })
        .collect()
}
