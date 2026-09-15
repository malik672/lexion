use std::{env, error::Error, str::FromStr, time::Instant};

use alloy_primitives::Address;
use lexion::{
    Amount,
    router::PathSet,
    solver::{GasValuation, Solver, SolverConfig},
    tycho::{TychoLiveFeed, TychoStreamConfig},
};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::time::{Duration, sleep};
use tycho_simulation::tycho_common::models::Chain;

const SENDER: &str = "0x1111111111111111111111111111111111111111";

struct Order {
    name: &'static str,
    token_in: &'static str,
    token_out: &'static str,
    amount: &'static str,
}

struct ReferenceQuote {
    block: u64,
    status: String,
    amount_out: Option<Amount>,
    net_amount_out: Option<Amount>,
    gas_estimate: Amount,
    solve_ms: u64,
    components: Vec<String>,
    legs: Vec<String>,
    gas_valuation: GasValuation,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let original_url = argument("--original-url", "http://127.0.0.1:3000".to_string())?;
    let max_hops = argument("--max-hops", 2_usize)?;
    let min_tvl = argument("--min-tvl", 10_f64)?;
    let details = env::args().any(|argument| argument == "--details");
    let host = env::var("TYCHO_HOST")
        .unwrap_or_else(|_| "tycho-fynd-ethereum.propellerheads.xyz".to_string());
    let api_key = env::var("TYCHO_API_KEY").ok();
    let client = Client::new();

    client
        .get(format!("{original_url}/v1/health"))
        .send()
        .await?
        .error_for_status()?;

    println!("connecting rewrite to {host} ...");
    let mut feed = TychoLiveFeed::connect(TychoStreamConfig {
        host,
        api_key,
        chain: Chain::Ethereum,
        min_tvl,
        min_token_quality: 100,
        traded_n_days_ago: Some(3),
    })
    .await?;

    let orders = [
        Order {
            name: "WETH->USDC",
            token_in: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            token_out: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            amount: "10000000000000000000",
        },
        Order {
            name: "USDC->WETH",
            token_in: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            token_out: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            amount: "1000000000",
        },
        Order {
            name: "USDT->WETH",
            token_in: "0xdac17f958d2ee523a2206206994597c13d831ec7",
            token_out: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            amount: "1000000000",
        },
        Order {
            name: "WBTC->USDC",
            token_in: "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599",
            token_out: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            amount: "10000000",
        },
        Order {
            name: "DAI->USDC",
            token_in: "0x6b175474e89094c44da98b954eedeac495271d0f",
            token_out: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            amount: "1000000000000000000000",
        },
    ];

    println!(
        "{:<12} {:>10} {:>14} {:>14} {:>11} {:>11} {:>12} {:>8}",
        "pair", "block", "rewrite out", "original out", "gross bps", "net bps", "latency", "pools"
    );
    for order in orders {
        let mut reference = quote_original(&client, &original_url, &order).await?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let local_block = feed.published_market().view().snapshot().block_number();
            if local_block == reference.block {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "could not align {}: rewrite block {local_block}, original block {}",
                    order.name, reference.block
                )
                .into());
            }
            if local_block < reference.block {
                feed.next_block().await?;
            } else {
                sleep(Duration::from_millis(200)).await;
                reference = quote_original(&client, &original_url, &order).await?;
            }
        }

        let token_in = feed
            .token_id(Address::from_str(order.token_in)?)
            .ok_or("rewrite input token missing")?;
        let token_out = feed
            .token_id(Address::from_str(order.token_out)?)
            .ok_or("rewrite output token missing")?;
        let view = feed.published_market().view();
        let paths = PathSet::discover(view.topology(), token_in, token_out, max_hops);
        let started = Instant::now();
        let solution = Solver::new(
            view.topology(),
            view.snapshot(),
            SolverConfig {
                gas_valuation: reference.gas_valuation,
                ..SolverConfig::default()
            },
        )
        .solve(&paths, Amount::from_str(order.amount)?)
        .ok_or("rewrite returned no route")?;
        let rewrite_latency = started.elapsed();
        let rewrite_components = solution
            .allocations
            .iter()
            .flat_map(|allocation| paths.paths[allocation.path_index].hops.iter())
            .filter_map(|hop| feed.component_id(hop.pool))
            .collect::<Vec<_>>();
        let delta_bps = reference
            .amount_out
            .map(|amount_out| relative_bps(solution.amount_out, amount_out));
        let net_delta_bps = reference
            .net_amount_out
            .map(|amount_out| relative_bps(solution.net_amount_out, amount_out));
        let mut rewrite_pool_set = rewrite_components.clone();
        rewrite_pool_set.sort_unstable();
        let mut original_pool_set = reference
            .components
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        original_pool_set.sort_unstable();
        let route_equal = reference.amount_out.is_some() && rewrite_pool_set == original_pool_set;
        println!(
            "{:<12} {:>10} {:>14} {:>14} {:>11} {:>11} {:>5.1}/{:<5}ms {:>8}",
            order.name,
            solution.block_number,
            solution.amount_out,
            reference
                .amount_out
                .map_or_else(|| reference.status.clone(), |amount| amount.to_string()),
            delta_bps.map_or_else(|| "—".to_string(), |bps| format!("{bps:+.6}")),
            net_delta_bps.map_or_else(|| "—".to_string(), |bps| format!("{bps:+.6}")),
            rewrite_latency.as_secs_f64() * 1_000.0,
            reference.solve_ms,
            if reference.amount_out.is_none() {
                "n/a"
            } else if route_equal {
                "same"
            } else {
                "different"
            },
        );
        if details && !route_equal {
            println!(
                "  net: rewrite={} original={} gas={}/{}",
                solution.net_amount_out,
                reference
                    .net_amount_out
                    .map_or_else(|| "—".to_string(), |value| value.to_string()),
                solution.gas_estimate,
                reference.gas_estimate,
            );
            println!("  rewrite allocations:");
            for allocation in &solution.allocations {
                let path = &paths.paths[allocation.path_index];
                let components = path
                    .hops
                    .iter()
                    .filter_map(|hop| feed.component_id(hop.pool))
                    .collect::<Vec<_>>()
                    .join(" -> ");
                println!(
                    "    in={} out={} path={components}",
                    allocation.amount_in, allocation.amount_out
                );
            }
            println!("  original legs:");
            for leg in &reference.legs {
                println!("    {leg}");
            }
        }
    }
    Ok(())
}

async fn quote_original(
    client: &Client,
    base_url: &str,
    order: &Order,
) -> Result<ReferenceQuote, Box<dyn Error>> {
    let response: Value = client
        .post(format!("{base_url}/v1/quote"))
        .json(&json!({
            "orders": [{
                "token_in": order.token_in,
                "token_out": order.token_out,
                "amount": order.amount,
                "side": "sell",
                "sender": SENDER
            }],
            "options": { "timeout_ms": 30000, "min_responses": 1 }
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let quote = &response["orders"][0];
    let status = quote["status"].as_str().unwrap_or("unknown").to_owned();
    let block = parse_u64(&quote["block"]["number"])
        .or_else(|| parse_u64(&quote["block"]))
        .ok_or("original response has no readable block number")?;
    let amount_out = (status == "success")
        .then(|| {
            quote["amount_out"]
                .as_str()
                .and_then(|value| Amount::from_str(value).ok())
        })
        .flatten();
    let gas_estimate = quote["gas_estimate"]
        .as_str()
        .and_then(|value| Amount::from_str(value).ok())
        .unwrap_or_default();
    let net_amount_out = quote["amount_out_net_gas"]
        .as_str()
        .and_then(|value| Amount::from_str(value).ok());
    let gas_valuation = match (amount_out, net_amount_out) {
        (Some(gross), Some(net)) if !gas_estimate.is_zero() && gross > net => GasValuation {
            cost_numerator: gross.saturating_sub(net),
            cost_denominator: gas_estimate,
        },
        _ => GasValuation::ZERO,
    };
    let components = quote["route"]["swaps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|swap| swap["component_id"].as_str().map(str::to_owned))
        .collect();
    let legs = quote["route"]["swaps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|swap| {
            format!(
                "in={} out={} component={} protocol={}",
                swap["amount_in"].as_str().unwrap_or("?"),
                swap["amount_out"].as_str().unwrap_or("?"),
                swap["component_id"].as_str().unwrap_or("?"),
                swap["protocol"].as_str().unwrap_or("?"),
            )
        })
        .collect();
    Ok(ReferenceQuote {
        block,
        status,
        amount_out,
        net_amount_out,
        solve_ms: response["solve_time_ms"].as_u64().unwrap_or_default(),
        components,
        legs,
        gas_estimate,
        gas_valuation,
    })
}

fn parse_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let text = value.as_str()?;
        text.strip_prefix("0x")
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            .or_else(|| text.parse().ok())
    })
}

fn relative_bps(left: Amount, right: Amount) -> f64 {
    if right.is_zero() {
        return 0.0;
    }
    let scale = Amount::from(1_000_000_000_u64);
    let scaled = left.saturating_mul(scale) / right;
    let ratio = scaled.try_into().unwrap_or(u64::MAX) as f64 / 1_000_000_000.0;
    (ratio - 1.0) * 10_000.0
}

fn argument<T>(name: &str, default: T) -> Result<T, Box<dyn Error>>
where
    T: FromStr,
    T::Err: Error + 'static,
{
    let Some(position) = env::args().position(|arg| arg == name) else {
        return Ok(default);
    };
    env::args()
        .nth(position + 1)
        .ok_or_else(|| format!("missing value for {name}").into())
        .and_then(|value| value.parse().map_err(Into::into))
}
