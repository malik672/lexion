//! External aggregator clients for Fynd audit comparisons.
//!
//! Add a new aggregator by implementing [`AggregatorClient`] and constructing it in
//! `audit::run`. The shared quote model (`AggregatorClient`, `AggregatorQuote`, …) lives in
//! `fynd-tools-common`.

use std::{collections::HashMap, time::Instant};

use async_trait::async_trait;
use fynd_tools_common::aggregator::{
    AggregatorCalldata, AggregatorClient, AggregatorQuote, AggregatorStatus,
};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

// ─── ParaSwap / Velora Market API ─────────────────────────────────────────────────────────────

/// Quote client for ParaSwap's current v6.2 Market API.
pub struct ParaswapClient {
    client: reqwest::Client,
    base_url: String,
    chain_id: u64,
    dexes: Option<String>,
    decimals: OnceCell<HashMap<String, u8>>,
}

impl ParaswapClient {
    pub fn new(
        base_url: impl Into<String>,
        chain_id: u64,
        dexes: Option<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build ParaSwap HTTP client: {e}"))?,
            base_url: base_url.into(),
            chain_id,
            dexes,
            decimals: OnceCell::new(),
        })
    }

    async fn decimals(&self) -> anyhow::Result<&HashMap<String, u8>> {
        self.decimals
            .get_or_try_init(|| async {
                #[derive(Deserialize)]
                struct TokensResponse {
                    tokens: Vec<Token>,
                }
                #[derive(Deserialize)]
                struct Token {
                    address: String,
                    decimals: u8,
                }
                let response = self
                    .client
                    .get(format!(
                        "{}/tokens/{}",
                        self.base_url.trim_end_matches('/'),
                        self.chain_id
                    ))
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<TokensResponse>()
                    .await?;
                Ok(response
                    .tokens
                    .into_iter()
                    .map(|token| (token.address.to_ascii_lowercase(), token.decimals))
                    .collect())
            })
            .await
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParaswapPriceResponse {
    price_route: ParaswapPriceRoute,
    tx_params: Option<ParaswapTransaction>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParaswapPriceRoute {
    dest_amount: String,
    gas_cost: Option<String>,
    best_route: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ParaswapTransaction {
    to: String,
    data: String,
    value: String,
}

#[async_trait]
impl AggregatorClient for ParaswapClient {
    fn name(&self) -> &str {
        "paraswap"
    }

    async fn quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount: &str,
        wallet: Option<&str>,
    ) -> anyhow::Result<AggregatorQuote> {
        let start = Instant::now();
        let decimals = self.decimals().await?;
        let Some(src_decimals) = decimals.get(&token_in.to_ascii_lowercase()) else {
            anyhow::bail!("ParaSwap has no decimals for source token {token_in}");
        };
        let Some(dest_decimals) = decimals.get(&token_out.to_ascii_lowercase()) else {
            anyhow::bail!("ParaSwap has no decimals for destination token {token_out}");
        };
        let src_decimals = src_decimals.to_string();
        let dest_decimals = dest_decimals.to_string();
        let chain_id = self.chain_id.to_string();
        let endpoint = if wallet.is_some() { "swap" } else { "prices" };
        let mut request = self
            .client
            .get(format!("{}/{endpoint}", self.base_url.trim_end_matches('/')))
            .query(&[
                ("srcToken", token_in),
                ("destToken", token_out),
                ("amount", amount),
                ("srcDecimals", src_decimals.as_str()),
                ("destDecimals", dest_decimals.as_str()),
                ("side", "SELL"),
                ("network", chain_id.as_str()),
                ("version", "6.2"),
                ("ignoreBadUsdPrice", "true"),
            ]);
        if let Some(dexes) = &self.dexes {
            request = request.query(&[("includeDEXS", dexes)]);
        }
        if let Some(wallet) = wallet {
            request = request.query(&[("userAddress", wallet), ("slippage", "50")]);
        }
        let response = request.send().await;
        let response_time_ms = start.elapsed().as_millis() as u64;
        let response = response.map_err(|e| anyhow::anyhow!("ParaSwap request failed: {e}"))?;
        if !response.status().is_success() {
            let code = response.status().as_u16();
            let snippet = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect();
            return Ok(AggregatorQuote {
                status: AggregatorStatus::HttpError { code, snippet },
                amount_out: None,
                amount_out_net_gas: None,
                gas_units: None,
                protocols: vec![],
                num_splits: None,
                response_time_ms,
                calldata: None,
                route: None,
            });
        }
        let data = response
            .json::<ParaswapPriceResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("ParaSwap parse error: {e}"))?;
        let amount_out =
            (data.price_route.dest_amount != "0").then_some(data.price_route.dest_amount);
        let gas_units = data
            .price_route
            .gas_cost
            .and_then(|gas| gas.parse().ok());
        let mut protocols = Vec::new();
        if let Some(route) = data.price_route.best_route {
            walk_protocol_names(&route, &mut protocols);
        }
        let calldata = data
            .tx_params
            .map(|tx| AggregatorCalldata { to: tx.to, data: tx.data, value: tx.value });
        protocols.sort();
        protocols.dedup();
        Ok(AggregatorQuote {
            status: if amount_out.is_some() {
                AggregatorStatus::Success
            } else {
                AggregatorStatus::NoAmount
            },
            amount_out,
            amount_out_net_gas: None,
            gas_units,
            protocols,
            num_splits: None,
            response_time_ms,
            calldata,
            route: None,
        })
    }
}

// ─── 1inch Swap API ─────────────────────────────────────────────────────────

/// Client for the authenticated 1inch Swap API.
pub struct OneInchClient {
    client: reqwest::Client,
    base_url: String,
    chain_id: u64,
}

impl OneInchClient {
    pub fn new(
        base_url: impl Into<String>,
        chain_id: u64,
        api_key: impl Into<String>,
    ) -> anyhow::Result<Self> {
        use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key.into()))
                .map_err(|_| anyhow::anyhow!("ONEINCH_API_KEY contains invalid characters"))?,
        );
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .default_headers(headers)
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build 1inch HTTP client: {e}"))?,
            base_url: base_url.into(),
            chain_id,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OneInchResponse {
    dst_amount: Option<String>,
    tx: Option<OneInchTransaction>,
    protocols: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct OneInchTransaction {
    to: Option<String>,
    data: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    gas: Option<u64>,
}

#[async_trait]
impl AggregatorClient for OneInchClient {
    fn name(&self) -> &str {
        "1inch"
    }

    async fn quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount: &str,
        wallet: Option<&str>,
    ) -> anyhow::Result<AggregatorQuote> {
        let start = Instant::now();
        let from = wallet.unwrap_or("0x00000000000000000000000000000000badbabe");
        let url = format!("{}/{}/swap", self.base_url.trim_end_matches('/'), self.chain_id);
        let resp = self
            .client
            .get(url)
            .query(&[
                ("src", token_in),
                ("dst", token_out),
                ("amount", amount),
                ("from", from),
                ("slippage", "0.5"),
                ("disableEstimate", "true"),
            ])
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("1inch request failed: {e}"))?;
        let response_time_ms = start.elapsed().as_millis() as u64;
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let snippet = resp
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect();
            return Ok(AggregatorQuote {
                status: AggregatorStatus::HttpError { code, snippet },
                amount_out: None,
                amount_out_net_gas: None,
                gas_units: None,
                protocols: vec![],
                num_splits: None,
                response_time_ms,
                calldata: None,
                route: None,
            });
        }
        let data = resp
            .json::<OneInchResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("1inch parse error: {e}"))?;
        let amount_out = data
            .dst_amount
            .filter(|amount| amount != "0" && !amount.is_empty());
        let gas_units = data
            .tx
            .as_ref()
            .and_then(|tx| tx.gas)
            .filter(|&gas| gas > 0);
        let calldata = data.tx.and_then(|tx| {
            Some(AggregatorCalldata {
                to: tx.to?,
                data: tx.data?,
                value: tx
                    .value
                    .unwrap_or_else(|| "0".to_owned()),
            })
        });
        let mut protocols = Vec::new();
        if let Some(value) = data.protocols {
            walk_protocol_names(&value, &mut protocols);
        }
        protocols.sort();
        protocols.dedup();
        Ok(AggregatorQuote {
            status: if amount_out.is_some() {
                AggregatorStatus::Success
            } else {
                AggregatorStatus::NoAmount
            },
            amount_out,
            amount_out_net_gas: None,
            gas_units,
            protocols,
            num_splits: None,
            response_time_ms,
            calldata,
            route: None,
        })
    }
}

fn walk_protocol_names(value: &serde_json::Value, names: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .for_each(|value| walk_protocol_names(value, names)),
        serde_json::Value::Object(map) => {
            if let Some(name) = map
                .get("name")
                .and_then(serde_json::Value::as_str)
            {
                names.push(name.to_owned());
            }
            if let Some(exchange) = map
                .get("exchange")
                .and_then(serde_json::Value::as_str)
            {
                names.push(exchange.to_owned());
            }
            map.values()
                .for_each(|value| walk_protocol_names(value, names));
        }
        _ => {}
    }
}

/// Client for CoW Protocol's sell-order quote endpoint.
///
/// `buyAmount` is already the user's output after CoW's fee. CoW settles the order for the
/// user, so its settlement gas must not be subtracted from that amount a second time.
pub struct CowSwapClient {
    client: reqwest::Client,
    base_url: String,
}

impl CowSwapClient {
    pub fn new(base_url: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build CoW Swap HTTP client: {e}"))?,
            base_url: base_url.into(),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CowQuoteRequest<'a> {
    sell_token: &'a str,
    buy_token: &'a str,
    from: &'a str,
    receiver: &'a str,
    sell_amount_before_fee: &'a str,
    kind: &'static str,
    valid_to: u64,
    partially_fillable: bool,
    signing_scheme: &'static str,
}

#[derive(Deserialize)]
struct CowQuoteResponse {
    quote: CowQuote,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CowQuote {
    buy_amount: String,
}

#[async_trait]
impl AggregatorClient for CowSwapClient {
    fn name(&self) -> &str {
        "cowswap"
    }

    async fn quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount: &str,
        wallet: Option<&str>,
    ) -> anyhow::Result<AggregatorQuote> {
        let start = Instant::now();
        let owner = wallet.unwrap_or("0x00000000000000000000000000000000badbabe");
        let valid_to = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow::anyhow!("system clock is before Unix epoch: {e}"))?
            .as_secs() +
            3_600;
        let request = CowQuoteRequest {
            sell_token: token_in,
            buy_token: token_out,
            from: owner,
            receiver: owner,
            sell_amount_before_fee: amount,
            kind: "sell",
            valid_to,
            partially_fillable: false,
            signing_scheme: "eip712",
        };
        let resp = self
            .client
            .post(format!("{}/api/v1/quote", self.base_url.trim_end_matches('/')))
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("CoW Swap request failed: {e}"))?;
        let response_time_ms = start.elapsed().as_millis() as u64;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(120).collect();
            return Ok(AggregatorQuote {
                status: AggregatorStatus::HttpError { code, snippet },
                amount_out: None,
                amount_out_net_gas: None,
                gas_units: None,
                protocols: vec![],
                num_splits: None,
                response_time_ms,
                calldata: None,
                route: None,
            });
        }

        let data = resp
            .json::<CowQuoteResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("CoW Swap parse error: {e}"))?;
        let amount_out = (data.quote.buy_amount != "0").then_some(data.quote.buy_amount);
        Ok(AggregatorQuote {
            status: if amount_out.is_some() {
                AggregatorStatus::Success
            } else {
                AggregatorStatus::NoAmount
            },
            amount_out_net_gas: amount_out.clone(),
            amount_out,
            gas_units: None,
            protocols: vec!["cow_protocol".to_string()],
            num_splits: None,
            response_time_ms,
            calldata: None,
            route: None,
        })
    }
}

// ─── Nordstern Finance ────────────────────────────────────────────────────────

/// Client for the Nordstern Finance aggregator API.
///
/// API: `GET /aggregator/{chain_id}?src=&dst=&amount=&from=`
pub struct NordsternClient {
    client: reqwest::Client,
    base_url: String,
    chain_id: u64,
    /// Dummy sender; Nordstern requires `from` but we are only quoting.
    from: String,
}

impl NordsternClient {
    pub fn new(base_url: impl Into<String>, chain_id: u64) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build Nordstern HTTP client: {e}"))?,
            base_url: base_url.into(),
            chain_id,
            from: "0x00000000000000000000000000000000badbabe".to_string(),
        })
    }
}

#[derive(Deserialize)]
struct NordsternResponse {
    /// Can be a JSON string or integer depending on Nordstern backend; we normalise to String.
    #[serde(rename = "toAmount")]
    to_amount: Option<serde_json::Value>,
    swaps: Option<Vec<NordsternSwap>>,
    /// Encoded transaction returned by Nordstern when a `from` address is supplied.
    tx: Option<NordsternTx>,
}

#[derive(Deserialize)]
struct NordsternTx {
    to: Option<String>,
    data: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Deserialize)]
struct NordsternSwap {
    // Nordstern returns fractional gas units (e.g. 162984.6); deserialise as f64, truncate later.
    #[serde(rename = "gasUnits", default)]
    gas_units: Option<f64>,
    route: Option<Vec<NordsternRouteItem>>,
}

#[derive(Deserialize)]
struct NordsternRouteItem {
    #[serde(rename = "type")]
    dex: Option<String>,
}

#[async_trait]
impl AggregatorClient for NordsternClient {
    fn name(&self) -> &str {
        "nordstern"
    }

    async fn quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount: &str,
        wallet: Option<&str>,
    ) -> anyhow::Result<AggregatorQuote> {
        let start = Instant::now();
        let url = format!("{}/aggregator/{}", self.base_url, self.chain_id);
        let from = wallet.unwrap_or(self.from.as_str());

        let resp = self
            .client
            .get(&url)
            .query(&[("src", token_in), ("dst", token_out), ("amount", amount), ("from", from)])
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Nordstern request failed: {e}"))?;

        let response_time_ms = start.elapsed().as_millis() as u64;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(120).collect();
            return Ok(AggregatorQuote {
                status: AggregatorStatus::HttpError { code, snippet },
                amount_out: None,
                amount_out_net_gas: None,
                gas_units: None,
                protocols: vec![],
                num_splits: None,
                response_time_ms,
                calldata: None,
                route: None,
            });
        }

        let text = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("Nordstern read error: {e}"))?;
        let data = match serde_json::from_str::<NordsternResponse>(&text) {
            Ok(d) => d,
            Err(_) => {
                // Nordstern returns a pseudo-schema description (not valid JSON) for token
                // pairs it doesn't support — treat that as no route rather than a hard error.
                return Ok(AggregatorQuote {
                    status: AggregatorStatus::NoRoute,
                    amount_out: None,
                    amount_out_net_gas: None,
                    gas_units: None,
                    protocols: vec![],
                    num_splits: None,
                    response_time_ms,
                    calldata: None,
                    route: None,
                });
            }
        };

        let swaps = data
            .swaps
            .as_deref()
            .unwrap_or_default();
        let num_splits = if swaps.is_empty() { None } else { Some(swaps.len()) };

        let gas_units: u64 = swaps
            .iter()
            .filter_map(|s| s.gas_units)
            .map(|f| f as u64)
            .sum();
        let gas_units = (gas_units > 0).then_some(gas_units);

        let mut seen = std::collections::HashSet::new();
        let protocols: Vec<String> = swaps
            .iter()
            .flat_map(|s| s.route.as_deref().unwrap_or_default())
            .filter_map(|r| r.dex.clone())
            .filter(|dex| seen.insert(dex.clone()))
            .collect();

        // Nordstern returns toAmount as either a JSON string or integer.
        // A value of "0" means the aggregator has no route for this pair/amount.
        let amount_out: Option<String> = data.to_amount.and_then(|v| {
            let s = match v {
                serde_json::Value::String(s) => s,
                serde_json::Value::Number(n) => n.to_string(),
                _ => return None,
            };
            if s == "0" {
                None
            } else {
                Some(s)
            }
        });

        let calldata = data.tx.and_then(|tx| {
            Some(AggregatorCalldata {
                to: tx.to?,
                data: tx.data?,
                value: tx
                    .value
                    .unwrap_or_else(|| "0".to_string()),
            })
        });

        Ok(AggregatorQuote {
            status: if amount_out.is_some() {
                AggregatorStatus::Success
            } else {
                AggregatorStatus::NoAmount
            },
            amount_out,
            amount_out_net_gas: None,
            gas_units,
            protocols,
            num_splits,
            response_time_ms,
            calldata,
            route: None,
        })
    }
}

// ─── KyberSwap Aggregator ────────────────────────────────────────────────────

/// Client for the KyberSwap Aggregator API.
///
/// API: `GET /{chain}/api/v1/routes?tokenIn=&tokenOut=&amountIn=`
pub struct KyberswapClient {
    client: reqwest::Client,
    base_url: String,
    chain: String,
}

impl KyberswapClient {
    pub fn new(base_url: impl Into<String>, chain: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                // KyberSwap sits behind Cloudflare; a browser UA avoids 403 challenges.
                .user_agent("Mozilla/5.0 (compatible; fynd-benchmark/1.0)")
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build KyberSwap HTTP client: {e}"))?,
            base_url: base_url.into(),
            chain: chain.into(),
        })
    }
}

#[derive(Deserialize)]
struct KyberswapResponse {
    data: Option<KyberswapData>,
}

#[derive(Deserialize)]
struct KyberswapData {
    #[serde(rename = "routeSummary")]
    route_summary: KyberswapRouteSummary,
}

/// KyberSwap route summary — typed fields for the values we read plus a catch-all
/// `extra` map that preserves all other fields for the `/route/build` POST body.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KyberswapRouteSummary {
    #[serde(default)]
    pub gas: Option<String>,
    pub amount_out: Option<String>,
    /// Outer: parallel split paths. Inner: hops within each path.
    pub route: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct KyberswapBuildResponse {
    data: Option<KyberswapBuildData>,
}

#[derive(Deserialize)]
struct KyberswapBuildData {
    /// ABI-encoded calldata.
    data: String,
    #[serde(rename = "routerAddress")]
    router_address: String,
    /// Encoded `msg.value` for native ETH swaps.
    #[serde(default)]
    value: Option<String>,
}

#[async_trait]
impl AggregatorClient for KyberswapClient {
    fn name(&self) -> &str {
        "kyberswap"
    }

    async fn quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount: &str,
        wallet: Option<&str>,
    ) -> anyhow::Result<AggregatorQuote> {
        let start = Instant::now();
        let url = format!("{}/{}/api/v1/routes", self.base_url, self.chain);

        let resp = self
            .client
            .get(&url)
            .header("X-Client-Id", "fynd-benchmark")
            .query(&[("tokenIn", token_in), ("tokenOut", token_out), ("amountIn", amount)])
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("KyberSwap request failed: {e}"))?;

        let response_time_ms = start.elapsed().as_millis() as u64;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(120).collect();
            return Ok(AggregatorQuote {
                status: AggregatorStatus::HttpError { code, snippet },
                amount_out: None,
                amount_out_net_gas: None,
                gas_units: None,
                protocols: vec![],
                num_splits: None,
                response_time_ms,
                calldata: None,
                route: None,
            });
        }

        let body = resp
            .json::<KyberswapResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("KyberSwap parse error: {e}"))?;

        let Some(data) = body.data else {
            return Ok(AggregatorQuote {
                status: AggregatorStatus::NoRoute,
                amount_out: None,
                amount_out_net_gas: None,
                gas_units: None,
                protocols: vec![],
                num_splits: None,
                response_time_ms,
                calldata: None,
                route: None,
            });
        };

        let rs = &data.route_summary;
        let gas_units: Option<u64> = rs
            .gas
            .as_deref()
            .and_then(|s| s.parse().ok())
            .filter(|&v: &u64| v > 0);
        let amount_out_str = rs.amount_out.as_deref().unwrap_or("");
        let amount_out = if amount_out_str == "0" || amount_out_str.is_empty() {
            None
        } else {
            Some(amount_out_str.to_string())
        };

        let route_outer = rs
            .route
            .as_ref()
            .and_then(|v| v.as_array());
        let num_splits = route_outer.map(|outer| outer.len().max(1));

        let mut seen = std::collections::HashSet::new();
        let protocols: Vec<String> = route_outer
            .map(|outer| {
                outer
                    .iter()
                    .filter_map(|inner| inner.as_array())
                    .flatten()
                    .filter_map(|hop| {
                        hop.get("exchange")
                            .and_then(|e| e.as_str())
                    })
                    .filter(|p| seen.insert(p.to_string()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let calldata = if let (Some(w), true) = (wallet, amount_out.is_some()) {
            self.build_calldata(w, data.route_summary)
                .await
        } else {
            None
        };

        Ok(AggregatorQuote {
            status: if amount_out.is_some() {
                AggregatorStatus::Success
            } else {
                AggregatorStatus::NoAmount
            },
            amount_out,
            amount_out_net_gas: None,
            gas_units,
            protocols,
            num_splits,
            response_time_ms,
            calldata,
            route: None,
        })
    }
}

impl KyberswapClient {
    /// POST to `/route/build` to get the encoded transaction for a previously fetched route.
    async fn build_calldata(
        &self,
        sender: &str,
        route_summary: KyberswapRouteSummary,
    ) -> Option<AggregatorCalldata> {
        let build_url = format!("{}/{}/api/v1/route/build", self.base_url, self.chain);
        let body = serde_json::json!({
            "routeSummary": &route_summary,
            "sender": sender,
            "recipient": sender,
            "slippageTolerance": 50,
            "deadline": 9_999_999_999u64,
        });
        let resp = self
            .client
            .post(&build_url)
            .header("X-Client-Id", "fynd-benchmark")
            .json(&body)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let built = resp
            .json::<KyberswapBuildResponse>()
            .await
            .ok()?;
        let d = built.data?;
        Some(AggregatorCalldata {
            to: d.router_address,
            data: d.data,
            value: d
                .value
                .unwrap_or_else(|| "0".to_string()),
        })
    }
}

// ─── 0x Protocol Swap API v2 ─────────────────────────────────────────────────

/// Client for the 0x Swap API v2 `/price` endpoint (read-only, no taker required).
///
/// API: `GET /swap/allowance-holder/price?chainId=&sellToken=&buyToken=&sellAmount=`
pub struct ZeroExClient {
    client: reqwest::Client,
    base_url: String,
    chain_id: u64,
}

impl ZeroExClient {
    pub fn new(
        base_url: impl Into<String>,
        chain_id: u64,
        api_key: impl Into<String>,
    ) -> anyhow::Result<Self> {
        use reqwest::header::{HeaderMap, HeaderValue};
        let api_key: String = api_key.into();
        let mut headers = HeaderMap::new();
        headers.insert(
            "0x-api-key",
            HeaderValue::from_str(&api_key)
                .map_err(|_| anyhow::anyhow!("ZRX_API_KEY contains invalid header characters"))?,
        );
        headers.insert("0x-version", HeaderValue::from_static("v2"));
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .default_headers(headers)
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build 0x HTTP client: {e}"))?,
            base_url: base_url.into(),
            chain_id,
        })
    }
}

#[derive(Deserialize)]
struct ZeroExFees {
    #[serde(rename = "zeroExFee")]
    zero_ex_fee: Option<ZeroExFeeDetail>,
}

#[derive(Deserialize)]
struct ZeroExFeeDetail {
    amount: Option<String>,
}

#[derive(Deserialize)]
struct ZeroExRoute {
    fills: Option<Vec<ZeroExFill>>,
}

#[derive(Deserialize)]
struct ZeroExFill {
    source: Option<String>,
}

/// 0x `/quote` response — superset of `/price`, includes transaction calldata.
#[derive(Deserialize)]
struct ZeroExQuoteResponse {
    #[serde(rename = "buyAmount")]
    buy_amount: Option<String>,
    fees: Option<ZeroExFees>,
    route: Option<ZeroExRoute>,
    gas: Option<String>,
    transaction: Option<ZeroExTransaction>,
}

#[derive(Deserialize)]
struct ZeroExTransaction {
    to: Option<String>,
    data: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

/// Fallback taker for 0x `/quote` when no wallet is configured.
/// Must be > 0x000000000000000000000000000000000000ffff per 0x API requirements.
const ZEROX_DUMMY_TAKER: &str = "0x000000000000000000000000000000000001dead";

#[async_trait]
impl AggregatorClient for ZeroExClient {
    fn name(&self) -> &str {
        "0x"
    }

    async fn quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount: &str,
        wallet: Option<&str>,
    ) -> anyhow::Result<AggregatorQuote> {
        let start = Instant::now();
        // Always use /quote — it returns amount, gas, route, and calldata in one call.
        // /price is a strict subset and requires a second round-trip for calldata.
        let url = format!("{}/swap/allowance-holder/quote", self.base_url);
        let taker = wallet.unwrap_or(ZEROX_DUMMY_TAKER);

        let resp = self
            .client
            .get(&url)
            .query(&[
                ("chainId", self.chain_id.to_string()),
                ("sellToken", token_in.to_string()),
                ("buyToken", token_out.to_string()),
                ("sellAmount", amount.to_string()),
                ("taker", taker.to_string()),
            ])
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("0x request failed: {e}"))?;

        let response_time_ms = start.elapsed().as_millis() as u64;

        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(120).collect();
            return Ok(AggregatorQuote {
                status: AggregatorStatus::HttpError { code, snippet },
                amount_out: None,
                amount_out_net_gas: None,
                gas_units: None,
                protocols: vec![],
                num_splits: None,
                response_time_ms,
                calldata: None,
                route: None,
            });
        }

        let data = resp
            .json::<ZeroExQuoteResponse>()
            .await
            .map_err(|e| anyhow::anyhow!("0x parse error: {e}"))?;

        // 0x returns buyAmount net of their protocol fee. Add back zeroExFee.amount
        // so we compare gross routing output (routing quality), not post-fee value.
        let fee: u128 = data
            .fees
            .as_ref()
            .and_then(|f| f.zero_ex_fee.as_ref())
            .and_then(|f| f.amount.as_deref())
            .and_then(|a| a.parse().ok())
            .unwrap_or(0);
        let amount_out = data
            .buy_amount
            .filter(|a| a != "0" && !a.is_empty())
            .and_then(|a| a.parse::<u128>().ok())
            .filter(|&v| v > 0)
            .map(|v| (v + fee).to_string());

        let mut seen = std::collections::HashSet::new();
        let protocols: Vec<String> = data
            .route
            .as_ref()
            .and_then(|r| r.fills.as_deref())
            .unwrap_or_default()
            .iter()
            .filter_map(|f| f.source.clone())
            .filter(|s| seen.insert(s.clone()))
            .collect();

        let gas_units = data
            .gas
            .as_deref()
            .and_then(|g| g.parse::<u64>().ok())
            .filter(|&g| g > 0);

        let calldata = data.transaction.and_then(|tx| {
            Some(AggregatorCalldata {
                to: tx.to?,
                data: tx.data?,
                value: tx
                    .value
                    .unwrap_or_else(|| "0".to_string()),
            })
        });

        Ok(AggregatorQuote {
            status: if amount_out.is_some() {
                AggregatorStatus::Success
            } else {
                AggregatorStatus::NoAmount
            },
            amount_out,
            amount_out_net_gas: None,
            gas_units,
            protocols,
            num_splits: None, // 0x route.fills is a flat list, not split-aware
            response_time_ms,
            calldata,
            route: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kyberswap_route_summary_round_trips() {
        let json = r#"{"gas":"150000","amountOut":"1000000","tokenIn":"0xabc","route":[[{"exchange":"uni_v3"}]]}"#;
        let rs: KyberswapRouteSummary = serde_json::from_str(json).unwrap();
        assert_eq!(rs.gas.as_deref(), Some("150000"));
        assert_eq!(rs.amount_out.as_deref(), Some("1000000"));
        let back = serde_json::to_string(&rs).unwrap();
        let v: serde_json::Value = serde_json::from_str(&back).unwrap();
        assert_eq!(v["gas"], "150000");
        assert_eq!(v["amountOut"], "1000000");
        assert_eq!(v["tokenIn"], "0xabc");
    }

    #[test]
    fn kyberswap_route_summary_preserves_unknown_fields() {
        let json = r#"{"gas":"100","amountOut":"500","slippage":50,"tokenIn":"0xfoo"}"#;
        let rs: KyberswapRouteSummary = serde_json::from_str(json).unwrap();
        let back = serde_json::to_value(&rs).unwrap();
        assert_eq!(back["slippage"], 50);
        assert_eq!(back["tokenIn"], "0xfoo");
    }

    #[test]
    fn kyberswap_route_summary_missing_optional_fields() {
        let json = r#"{"tokenIn":"0xabc","tokenOut":"0xdef"}"#;
        let rs: KyberswapRouteSummary = serde_json::from_str(json).unwrap();
        assert!(rs.gas.is_none());
        assert!(rs.amount_out.is_none());
        assert!(rs.route.is_none());
        let back = serde_json::to_value(&rs).unwrap();
        assert_eq!(back["tokenIn"], "0xabc");
    }
}
