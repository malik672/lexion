use std::{collections::HashMap, env, error::Error, fs, str::FromStr, time::Instant};

use alloy_primitives::Address;
use lexion::{
    Amount,
    execution::{dry_run, encode_unsigned_transaction},
    router::PathSet,
    solver::{GasValuation, Solver, SolverConfig},
    tycho::{TychoLiveFeed, TychoStreamConfig},
};
use tycho_simulation::tycho_common::models::Chain;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = env::args().collect::<Vec<_>>();
    if args
        .get(1)
        .is_none_or(|argument| argument == "--help" || argument == "-h")
    {
        print_help();
        return Ok(());
    }
    if args[1] != "quote" && args[1] != "swap" {
        return Err(format!("unknown command `{}`; expected `quote` or `swap`", args[1]).into());
    }

    let file_env = load_dotenv(".env")?;
    let token_in = configured_required::<Address>(&args, &file_env, "--token-in", "TOKEN_IN")?;
    let token_out = configured_required::<Address>(&args, &file_env, "--token-out", "TOKEN_OUT")?;
    let amount_in = configured_required::<Amount>(&args, &file_env, "--amount", "AMOUNT")?;
    let max_hops = configured_optional(&args, &file_env, "--max-hops", "MAX_HOPS", 2_usize)?;
    let min_tvl = configured_optional(&args, &file_env, "--min-tvl", "MIN_TVL", 10_f64)?;
    let gas_numerator = configured_optional(
        &args,
        &file_env,
        "--gas-cost-numerator",
        "GAS_COST_NUMERATOR",
        Amount::ZERO,
    )?;
    let gas_denominator = configured_optional(
        &args,
        &file_env,
        "--gas-cost-denominator",
        "GAS_COST_DENOMINATOR",
        Amount::from(1),
    )?;
    let host = setting(&file_env, "TYCHO_HOST")
        .unwrap_or_else(|| "tycho-fynd-ethereum.propellerheads.xyz".to_owned());
    let api_key = setting(&file_env, "TYCHO_API_KEY");

    eprintln!("connecting to {host} ...");
    let feed = TychoLiveFeed::connect(TychoStreamConfig {
        host,
        api_key,
        chain: Chain::Ethereum,
        min_tvl,
        min_token_quality: 100,
        traded_n_days_ago: Some(3),
    })
    .await?;
    let input = feed
        .token_id(token_in)
        .ok_or("input token is absent from the captured market")?;
    let output = feed
        .token_id(token_out)
        .ok_or("output token is absent from the captured market")?;
    let market = feed.published_market();
    let view = market.view();
    let started = Instant::now();
    let paths = PathSet::discover(view.topology(), input, output, max_hops);
    if paths.paths.is_empty() {
        return Err("no structural path exists in the captured market".into());
    }
    let solution = Solver::new(
        view.topology(),
        view.snapshot(),
        SolverConfig {
            gas_valuation: GasValuation {
                cost_numerator: gas_numerator,
                cost_denominator: gas_denominator,
            },
            ..SolverConfig::default()
        },
    )
    .solve(&paths, amount_in)
    .ok_or("all discovered paths failed simulation")?;
    let elapsed = started.elapsed();

    println!("block:          {}", solution.block_number);
    println!("amount in:      {}", solution.amount_in);
    println!("gross output:   {}", solution.amount_out);
    println!("gas estimate:   {}", solution.gas_estimate);
    if gas_numerator.is_zero() {
        println!("net output:     unavailable (no output-token gas conversion supplied)");
    } else {
        println!("net output:     {}", solution.net_amount_out);
    }
    println!("paths examined: {}", paths.paths.len());
    println!("solve time:     {:.3} ms", elapsed.as_secs_f64() * 1_000.0);
    println!("allocations:");
    for (index, allocation) in solution.allocations.iter().enumerate() {
        let components = paths.paths[allocation.path_index]
            .hops
            .iter()
            .map(|hop| feed.component_id(hop.pool).unwrap_or("<unknown>"))
            .collect::<Vec<_>>()
            .join(" -> ");
        println!(
            "  {}. in={} out={} pools={components}",
            index + 1,
            allocation.amount_in,
            allocation.amount_out,
        );
    }
    if args[1] == "swap" {
        let sender =
            configured_required::<Address>(&args, &file_env, "--sender", "WALLET_ADDRESS")?;
        let recipient =
            configured_optional(&args, &file_env, "--recipient", "RECIPIENT_ADDRESS", sender)?;
        let slippage_bps =
            configured_optional(&args, &file_env, "--slippage-bps", "SLIPPAGE_BPS", 30_u16)?;
        let rpc_url = args
            .iter()
            .position(|argument| argument == "--rpc-url")
            .and_then(|position| args.get(position + 1).cloned())
            .or_else(|| setting(&file_env, "RPC_URL"))
            .ok_or("`swap` requires `--rpc-url` or RPC_URL in .env")?;
        let transaction = encode_unsigned_transaction(
            &solution,
            &paths,
            view.snapshot(),
            token_in,
            token_out,
            sender,
            recipient,
            slippage_bps,
        )?;
        dry_run(
            &reqwest::Client::new(),
            &rpc_url,
            &transaction,
            solution.block_number,
        )
        .await?;
        println!("\nunsigned transaction (same-block eth_call passed):");
        println!("  from:             {}", transaction.from);
        println!("  to:               {}", transaction.to);
        println!("  value:            {}", transaction.value);
        println!("  estimated gas:    {}", transaction.estimated_gas);
        println!("  expected output:  {}", transaction.expected_amount_out);
        println!("  minimum output:   {}", transaction.min_amount_out);
        println!("  approval spender: {}", transaction.to);
        println!(
            "  data:              0x{}",
            alloy::hex::encode(transaction.data)
        );
        println!("\nunsigned only: approve the spender, then let your wallet sign and submit");
    } else {
        println!("\nquote only: no transaction was signed or submitted");
    }
    Ok(())
}

fn print_help() {
    println!(
        "Lexion local router\n\n\
         Usage:\n  lexion quote --token-in ADDRESS --token-out ADDRESS --amount RAW_UNITS [OPTIONS]\n  \
         lexion swap --token-in ADDRESS --token-out ADDRESS --amount RAW_UNITS \\\n           --sender ADDRESS [--recipient ADDRESS] --rpc-url URL [OPTIONS]\n\n\
         Options:\n  --max-hops N                 Maximum path length (default: 2)\n  \
         --min-tvl ETH                Tycho pool filter (default: 10)\n  \
         --gas-cost-numerator N       Output-token units per gas ratio numerator\n  \
         --gas-cost-denominator N     Output-token units per gas ratio denominator\n\n\
         Swap options:\n  --sender ADDRESS              Address providing the input token\n  \
         --recipient ADDRESS           Output recipient (default: sender)\n  \
         --slippage-bps N              Minimum-output tolerance (default: 30)\n  \
         --rpc-url URL                 Ethereum RPC used for same-block eth_call\n\n\
         Amounts are raw token units. `swap` prints validated unsigned calldata; it never signs."
    );
}

fn required<T>(args: &[String], name: &str) -> Result<T, Box<dyn Error>>
where
    T: FromStr,
    T::Err: Error + 'static,
{
    let position = args.iter().position(|argument| argument == name);
    let value = position
        .and_then(|position| args.get(position + 1))
        .ok_or_else(|| format!("missing required `{name}`"))?;
    Ok(value.parse()?)
}

fn optional<T>(args: &[String], name: &str, default: T) -> Result<T, Box<dyn Error>>
where
    T: FromStr,
    T::Err: Error + 'static,
{
    match args.iter().position(|argument| argument == name) {
        Some(position) => Ok(args
            .get(position + 1)
            .ok_or_else(|| format!("missing value for `{name}`"))?
            .parse()?),
        None => Ok(default),
    }
}

fn configured_required<T>(
    args: &[String],
    file_env: &HashMap<String, String>,
    flag: &str,
    environment: &str,
) -> Result<T, Box<dyn Error>>
where
    T: FromStr,
    T::Err: Error + 'static,
{
    if args.iter().any(|argument| argument == flag) {
        required(args, flag)
    } else {
        setting(file_env, environment)
            .ok_or_else(|| format!("missing `{flag}` or {environment} in .env"))?
            .parse()
            .map_err(Into::into)
    }
}

fn configured_optional<T>(
    args: &[String],
    file_env: &HashMap<String, String>,
    flag: &str,
    environment: &str,
    default: T,
) -> Result<T, Box<dyn Error>>
where
    T: FromStr,
    T::Err: Error + 'static,
{
    if args.iter().any(|argument| argument == flag) {
        optional(args, flag, default)
    } else if let Some(value) = setting(file_env, environment) {
        Ok(value.parse()?)
    } else {
        Ok(default)
    }
}

fn load_dotenv(path: &str) -> Result<HashMap<String, String>, Box<dyn Error>> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(HashMap::new());
    };
    let mut values = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.strip_prefix("export ").unwrap_or(line).split_once('=')
        else {
            continue;
        };
        values.insert(
            key.trim().to_owned(),
            value.trim().trim_matches(['\'', '"']).to_owned(),
        );
    }
    Ok(values)
}

fn setting(file_env: &HashMap<String, String>, name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            file_env
                .get(name)
                .filter(|value| !value.is_empty())
                .cloned()
        })
}
