use std::{env, error::Error, str::FromStr, time::Duration};

use alloy_primitives::Address;
use lexion::{
    Amount,
    router::PathSet,
    runtime::{QuoteRequest, QuoteRuntimeConfig, QuoteWorker},
    tycho::{TychoLiveFeed, TychoStreamConfig},
};
use tycho_simulation::tycho_common::models::Chain;

const WETH: &str = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
const USDC: &str = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
const USDT: &str = "0xdac17f958d2ee523a2206206994597c13d831ec7";
const WBTC: &str = "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599";
const DAI: &str = "0x6b175474e89094c44da98b954eedeac495271d0f";

struct Pair {
    name: &'static str,
    token_in: &'static str,
    token_out: &'static str,
    amount_in: Amount,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let quotes = argument("--quotes", 10_usize)?;
    let max_hops = argument("--max-hops", 2_usize)?;
    let min_tvl = argument("--min-tvl", 10_f64)?;
    let min_token_quality = argument("--min-token-quality", 100_u32)?;
    let traded_n_days_ago = argument("--traded-days", 3_u64)?;
    let host = env::var("TYCHO_HOST")
        .unwrap_or_else(|_| "tycho-fynd-ethereum.propellerheads.xyz".to_string());
    let api_key = env::var("TYCHO_API_KEY").ok();

    println!("connecting to {host} ...");
    let feed = TychoLiveFeed::connect(TychoStreamConfig {
        host,
        api_key,
        chain: Chain::Ethereum,
        min_tvl,
        min_token_quality,
        traded_n_days_ago: Some(traded_n_days_ago),
    })
    .await?;
    let market = feed.published_market();
    let initial = market.view();
    println!(
        "captured block {} (topology generation {}, {} pools)",
        initial.snapshot().block_number(),
        initial.topology_generation(),
        initial.topology().pool_count(),
    );

    let pairs = [
        Pair {
            name: "WETH->USDC",
            token_in: WETH,
            token_out: USDC,
            amount_in: Amount::from(10_u64).pow(Amount::from(18_u64)),
        },
        Pair {
            name: "USDC->WETH",
            token_in: USDC,
            token_out: WETH,
            amount_in: Amount::from(1_000_000_000_u64),
        },
        Pair {
            name: "USDT->WETH",
            token_in: USDT,
            token_out: WETH,
            amount_in: Amount::from(1_000_000_000_u64),
        },
        Pair {
            name: "WBTC->USDC",
            token_in: WBTC,
            token_out: USDC,
            amount_in: Amount::from(10_000_000_u64),
        },
        Pair {
            name: "DAI->USDC",
            token_in: DAI,
            token_out: USDC,
            amount_in: Amount::from(1_000_u64) * Amount::from(10_u64).pow(Amount::from(18_u64)),
        },
    ];
    let mut resolved = Vec::new();
    for pair in pairs {
        let Some(token_in) = feed.token_id(Address::from_str(pair.token_in)?) else {
            println!(
                "skip {}: input token absent from captured V4 market",
                pair.name
            );
            continue;
        };
        let Some(token_out) = feed.token_id(Address::from_str(pair.token_out)?) else {
            println!(
                "skip {}: output token absent from captured V4 market",
                pair.name
            );
            continue;
        };
        let forward = PathSet::discover(initial.topology(), token_in, token_out, max_hops);
        let reverse = PathSet::discover(initial.topology(), token_out, token_in, max_hops);
        println!(
            "pair {:>11}: ids={}->{} degree={}/{} paths={}/{} asymmetric_edges={}",
            pair.name,
            token_in.0,
            token_out.0,
            initial.topology().token_degree(token_in),
            initial.topology().token_degree(token_out),
            forward.paths.len(),
            reverse.paths.len(),
            initial.topology().asymmetric_edge_count(),
        );
        assert_eq!(
            forward.paths.len(),
            reverse.paths.len(),
            "undirected path symmetry violated for {}",
            pair.name
        );
        resolved.push((pair, token_in, token_out));
    }
    if resolved.is_empty() {
        return Err("none of the default pairs exists in the captured market".into());
    }

    let writer = tokio::spawn(feed.run());
    let mut worker = QuoteWorker::new(
        market.clone(),
        QuoteRuntimeConfig {
            max_hops,
            max_block_lag: 2,
            ..QuoteRuntimeConfig::default()
        },
    );
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    let mut solve = Vec::new();
    let mut discovery = Vec::new();
    let mut successes = 0_usize;
    let mut attempts = 0_usize;

    for round in 0..quotes {
        for (pair, token_in, token_out) in &resolved {
            attempts += 1;
            let chain_head = market.snapshot().block_number();
            let request = QuoteRequest {
                token_in: *token_in,
                token_out: *token_out,
                amount_in: pair.amount_in,
                chain_head,
            };
            match worker.quote(request) {
                Ok(quote) => {
                    successes += 1;
                    let bucket = if quote.path_cache_hit {
                        &mut warm
                    } else {
                        &mut cold
                    };
                    bucket.push(quote.timings.total);
                    solve.push(quote.timings.solve);
                    discovery.push(quote.timings.path_discovery);
                    println!(
                        "round {:>2} {:>11}: block={} gen={} paths={} cache={} total={:?} discover={:?} solve={:?}",
                        round + 1,
                        pair.name,
                        quote.solution.block_number,
                        quote.topology_generation,
                        quote.path_count,
                        quote.path_cache_hit,
                        quote.timings.total,
                        quote.timings.path_discovery,
                        quote.timings.solve,
                    );
                }
                Err(error) => println!("round {:>2} {:>11}: {error:?}", round + 1, pair.name),
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    println!("\nLexion live smoke");
    println!("attempts/successes  {attempts}/{successes}");
    print_distribution("cold total", &mut cold);
    print_distribution("warm total", &mut warm);
    print_distribution("path discovery", &mut discovery);
    print_distribution("solver", &mut solve);
    println!("final block          {}", market.snapshot().block_number());

    writer.abort();
    Ok(())
}

fn argument<T>(name: &str, default: T) -> Result<T, Box<dyn Error>>
where
    T: FromStr,
    T::Err: Error + 'static,
{
    let mut args = env::args();
    while let Some(argument) = args.next() {
        if argument == name {
            return Ok(args.next().ok_or("missing argument value")?.parse()?);
        }
    }
    Ok(default)
}

fn print_distribution(label: &str, values: &mut [Duration]) {
    if values.is_empty() {
        println!("{label:<20} unavailable");
        return;
    }
    values.sort_unstable();
    let p50 = values[(values.len() - 1) / 2];
    let p95 = values[((values.len() - 1) * 95) / 100];
    println!("{label:<20} n={} p50={p50:?} p95={p95:?}", values.len());
}
