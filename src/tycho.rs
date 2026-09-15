//! Thin Tycho adapters. No routing policy belongs in this module.

use std::{
    collections::HashMap,
    error::Error,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::Arc,
};

use alloy_primitives::Address;
use num_bigint::BigUint;
use tokio_stream::{Stream, StreamExt};
use tycho_simulation::{
    evm::{
        protocol::{
            uniswap_v2::state::UniswapV2State, uniswap_v3::state::UniswapV3State,
            uniswap_v4::state::UniswapV4State,
        },
        stream::ProtocolStreamBuilder,
    },
    protocol::models::Update,
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_common::{
        models::{Chain, protocol::ProtocolComponent},
        simulation::protocol_sim::ProtocolSim,
    },
    tycho_core::models::token::Token,
    utils::load_all_tokens,
};

use crate::{
    Amount, PoolId, TokenId,
    market::{MarketBuilder, MarketSnapshot, MarketTopology, PublishedMarket},
    simulator::{PoolKind, PoolState},
};

/// A decoded Tycho component at one block. Stream decoding stays outside the routing core.
pub struct TychoComponent {
    pub component_id: Box<str>,
    pub token_a_address: Address,
    pub token_b_address: Address,
    pub token_a: Token,
    pub token_b: Token,
    pub kind: PoolKind,
    pub state: Box<dyn ProtocolSim>,
    protocol_component: Option<ProtocolComponent>,
}

/// A decoded state update for a component already present in the frozen topology.
pub struct TychoUpdate {
    pub component_id: Box<str>,
    pub token_a: Token,
    pub token_b: Token,
    pub state: Box<dyn ProtocolSim>,
}

/// Stable identity maps around a compact, immutable routing market.
pub struct TychoMarket {
    topology: MarketTopology,
    snapshot: MarketSnapshot,
    tokens: HashMap<Address, TokenId>,
    components: HashMap<Box<str>, PoolId>,
    pools: HashMap<Box<str>, TychoPool>,
}

impl TychoMarket {
    fn from_initial_update(update: Update) -> Result<Self, TychoFeedError> {
        if update.new_pairs.is_empty() {
            return Err(TychoFeedError::EmptyInitialSnapshot);
        }
        let mut components = Vec::new();
        for (component_id, component) in update.new_pairs {
            if component.tokens.len() != 2 {
                return Err(TychoFeedError::InvalidComponent(component_id));
            }
            let Some(state) = update.states.get(&component_id) else {
                return Err(TychoFeedError::InvalidComponent(component_id));
            };
            let Some(kind) = protocol_kind(&component.protocol_system, state.as_ref()) else {
                continue;
            };
            let token_a = component.tokens[0].clone();
            let token_b = component.tokens[1].clone();
            components.push(TychoComponent {
                component_id: component_id.into_boxed_str(),
                token_a_address: token_address(&token_a)?,
                token_b_address: token_address(&token_b)?,
                token_a,
                token_b,
                kind,
                state: state.clone(),
                protocol_component: Some(component.into()),
            });
        }
        if components.is_empty() {
            return Err(TychoFeedError::EmptyInitialSnapshot);
        }
        Ok(Self::from_components(
            update.block_number_or_timestamp,
            components,
        ))
    }

    /// Builds a frozen topology and its first block snapshot from decoded components.
    pub fn from_components(
        block_number: u64,
        components: impl IntoIterator<Item = TychoComponent>,
    ) -> Self {
        let mut tokens = HashMap::new();
        let mut pools = HashMap::new();
        for component in components {
            let token_a_id = intern_token(&mut tokens, component.token_a_address);
            let token_b_id = intern_token(&mut tokens, component.token_b_address);
            assert!(
                pools
                    .insert(
                        component.component_id,
                        TychoPool::from_boxed(
                            component.kind,
                            component.state,
                            token_a_id,
                            token_b_id,
                            component.token_a,
                            component.token_b,
                            component.protocol_component,
                        ),
                    )
                    .is_none(),
                "duplicate Tycho component id"
            );
        }
        let (topology, snapshot, components) = rebuild_market(block_number, &pools);
        Self {
            topology,
            snapshot,
            tokens,
            components,
            pools,
        }
    }

    pub fn topology(&self) -> &MarketTopology {
        &self.topology
    }

    pub fn snapshot(&self) -> &MarketSnapshot {
        &self.snapshot
    }

    pub fn token_id(&self, address: Address) -> Option<TokenId> {
        self.tokens.get(&address).copied()
    }

    pub fn pool_id(&self, component_id: &str) -> Option<PoolId> {
        self.components.get(component_id).copied()
    }

    pub fn component_id(&self, pool_id: PoolId) -> Option<&str> {
        self.components
            .iter()
            .find_map(|(component_id, &candidate)| {
                (candidate == pool_id).then_some(component_id.as_ref())
            })
    }

    /// Applies decoded updates and publishes a new snapshot after all IDs are validated.
    pub fn apply_updates(
        &mut self,
        block_number: u64,
        updates: impl IntoIterator<Item = TychoUpdate>,
    ) -> Result<(), UnknownComponent> {
        let mut changed = Vec::new();
        for update in updates {
            let Some(pool) = self.pools.get_mut(update.component_id.as_ref()) else {
                return Err(UnknownComponent(update.component_id));
            };
            if !state_matches_kind(pool.kind, update.state.as_ref()) {
                return Err(UnknownComponent(update.component_id));
            }
            *pool = TychoPool::from_boxed(
                pool.kind,
                update.state,
                pool.token_a_id,
                pool.token_b_id,
                update.token_a,
                update.token_b,
                pool.protocol_component.clone(),
            );
            changed.push(update.component_id);
        }
        let translated = changed.into_iter().map(|component_id| {
            let pool_id = self.components[component_id.as_ref()];
            let state = self.pools[component_id.as_ref()].clone();
            (pool_id, PoolState::Tycho(Box::new(state)))
        });
        self.snapshot = self
            .topology
            .next_snapshot(&self.snapshot, block_number, translated);
        Ok(())
    }

    fn apply_update(&mut self, update: Update) -> Result<bool, TychoFeedError> {
        let Update {
            block_number_or_timestamp,
            mut states,
            new_pairs,
            removed_pairs,
            ..
        } = update;
        let topology_changed = !new_pairs.is_empty() || !removed_pairs.is_empty();
        for component_id in removed_pairs.keys() {
            self.pools.remove(component_id.as_str());
            states.remove(component_id.as_str());
        }
        for (component_id, component) in new_pairs {
            if component.tokens.len() != 2 {
                return Err(TychoFeedError::InvalidComponent(component_id));
            }
            let Some(state) = states.remove(&component_id) else {
                return Err(TychoFeedError::InvalidComponent(component_id));
            };
            let Some(kind) = protocol_kind(&component.protocol_system, state.as_ref()) else {
                continue;
            };
            let token_a = component.tokens[0].clone();
            let token_b = component.tokens[1].clone();
            let token_a_id = intern_token(&mut self.tokens, token_address(&token_a)?);
            let token_b_id = intern_token(&mut self.tokens, token_address(&token_b)?);
            self.pools.insert(
                component_id.into_boxed_str(),
                TychoPool::from_boxed(
                    kind,
                    state,
                    token_a_id,
                    token_b_id,
                    token_a,
                    token_b,
                    Some(component.into()),
                ),
            );
        }
        let mut changed = Vec::with_capacity(states.len());
        for (component_id, state) in states {
            let Some(pool) = self.pools.get_mut(component_id.as_str()) else {
                return Err(TychoFeedError::UnknownComponent(
                    component_id.into_boxed_str(),
                ));
            };
            if !state_matches_kind(pool.kind, state.as_ref()) {
                return Err(TychoFeedError::InvalidComponent(component_id));
            }
            *pool = pool.with_state(state);
            changed.push(component_id);
        }
        if topology_changed {
            (self.topology, self.snapshot, self.components) =
                rebuild_market(block_number_or_timestamp, &self.pools);
        } else {
            let translated = changed.into_iter().map(|component_id| {
                let pool_id = self.components[component_id.as_str()];
                let state = self.pools[component_id.as_str()].clone();
                (pool_id, PoolState::Tycho(Box::new(state)))
            });
            self.snapshot =
                self.topology
                    .next_snapshot(&self.snapshot, block_number_or_timestamp, translated);
        }
        Ok(topology_changed)
    }
}

fn rebuild_market(
    block_number: u64,
    pools: &HashMap<Box<str>, TychoPool>,
) -> (MarketTopology, MarketSnapshot, HashMap<Box<str>, PoolId>) {
    let mut entries = pools.iter().collect::<Vec<_>>();
    entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    let mut builder = MarketBuilder::default();
    let mut components = HashMap::with_capacity(entries.len());
    for (component_id, pool) in entries {
        let pool_id = builder.add_tycho_pool(pool.clone());
        components.insert(component_id.clone(), pool_id);
    }
    let (topology, snapshot) = builder.finish(block_number);
    (topology, snapshot, components)
}

#[derive(Debug)]
pub struct UnknownComponent(pub Box<str>);

fn intern_token(tokens: &mut HashMap<Address, TokenId>, address: Address) -> TokenId {
    if let Some(&id) = tokens.get(&address) {
        return id;
    }
    let id = TokenId(tokens.len().try_into().expect("too many tokens"));
    tokens.insert(address, id);
    id
}

fn token_address(token: &Token) -> Result<Address, TychoFeedError> {
    let bytes = token.address.as_ref();
    if bytes.len() != Address::len_bytes() {
        return Err(TychoFeedError::InvalidComponent(format!(
            "token address has {} bytes",
            bytes.len()
        )));
    }
    Ok(Address::from_slice(bytes))
}

/// Exact Tycho simulator state and metadata for one V2, V3, or V4 pool.
#[derive(Clone, Debug)]
pub struct TychoPool {
    state: Box<dyn ProtocolSim>,
    kind: PoolKind,
    token_a_id: TokenId,
    token_b_id: TokenId,
    token_a: Token,
    token_b: Token,
    protocol_component: Option<ProtocolComponent>,
}

impl TychoPool {
    pub fn new<S>(
        kind: PoolKind,
        state: S,
        token_a_id: TokenId,
        token_b_id: TokenId,
        token_a: Token,
        token_b: Token,
    ) -> Self
    where
        S: ProtocolSim,
    {
        assert!(
            state_matches_kind(kind, &state),
            "state does not match pool kind"
        );
        Self::from_boxed(
            kind,
            Box::new(state),
            token_a_id,
            token_b_id,
            token_a,
            token_b,
            None,
        )
    }

    fn from_boxed(
        kind: PoolKind,
        state: Box<dyn ProtocolSim>,
        token_a_id: TokenId,
        token_b_id: TokenId,
        token_a: Token,
        token_b: Token,
        protocol_component: Option<ProtocolComponent>,
    ) -> Self {
        Self {
            state,
            kind,
            token_a_id,
            token_b_id,
            token_a,
            token_b,
            protocol_component,
        }
    }

    pub fn kind(&self) -> PoolKind {
        self.kind
    }

    pub fn token_a_id(&self) -> TokenId {
        self.token_a_id
    }

    pub fn token_b_id(&self) -> TokenId {
        self.token_b_id
    }

    fn with_state(&self, state: Box<dyn ProtocolSim>) -> Self {
        Self::from_boxed(
            self.kind,
            state,
            self.token_a_id,
            self.token_b_id,
            self.token_a.clone(),
            self.token_b.clone(),
            self.protocol_component.clone(),
        )
    }

    pub fn quote_exact_input(&self, token_in: TokenId, amount_in: Amount) -> Option<Amount> {
        self.quote_exact_input_with_gas(token_in, amount_in)
            .map(|(amount, _)| amount)
    }

    pub fn quote_exact_input_with_gas(
        &self,
        token_in: TokenId,
        amount_in: Amount,
    ) -> Option<(Amount, Amount)> {
        let (input, output) = if token_in == self.token_a_id {
            (&self.token_a, &self.token_b)
        } else if token_in == self.token_b_id {
            (&self.token_b, &self.token_a)
        } else {
            return None;
        };
        let amount = BigUint::from_bytes_be(&amount_in.to_be_bytes::<32>());
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.state.get_amount_out(amount, input, output)
        }))
        .ok()?
        .ok()?;
        let amount_bytes = result.amount.to_bytes_be();
        let gas_bytes = result.gas.to_bytes_be();
        (amount_bytes.len() <= 32 && gas_bytes.len() <= 32).then(|| {
            (
                Amount::from_be_slice(&amount_bytes),
                Amount::from_be_slice(&gas_bytes),
            )
        })
    }

    pub(crate) fn execution_parts(
        &self,
        token_in: TokenId,
    ) -> Option<(ProtocolComponent, Token, Token, Box<dyn ProtocolSim>)> {
        let component = self.protocol_component.clone()?;
        let (input, output) = if token_in == self.token_a_id {
            (self.token_a.clone(), self.token_b.clone())
        } else if token_in == self.token_b_id {
            (self.token_b.clone(), self.token_a.clone())
        } else {
            return None;
        };
        Some((component, input, output, self.state.clone_box()))
    }
}

fn protocol_kind(protocol: &str, state: &dyn ProtocolSim) -> Option<PoolKind> {
    let kind = match protocol {
        "uniswap_v2" => PoolKind::V2,
        "uniswap_v3" => PoolKind::V3,
        "uniswap_v4" => PoolKind::V4,
        _ => return None,
    };
    state_matches_kind(kind, state).then_some(kind)
}

fn state_matches_kind(kind: PoolKind, state: &dyn ProtocolSim) -> bool {
    match kind {
        PoolKind::V2 => state.as_any().is::<UniswapV2State>(),
        PoolKind::V3 => state.as_any().is::<UniswapV3State>(),
        PoolKind::V4 => state.as_any().is::<UniswapV4State>(),
    }
}

/// Minimal configuration for a live Uniswap V2/V3/V4 Tycho stream.
pub struct TychoStreamConfig {
    pub host: String,
    pub api_key: Option<String>,
    pub chain: Chain,
    pub min_tvl: f64,
    pub min_token_quality: u32,
    pub traded_n_days_ago: Option<u64>,
}

type UpdateStream = Pin<Box<dyn Stream<Item = Result<Update, String>> + Send>>;

/// Single-writer live feed with lock-free snapshot reads.
pub struct TychoLiveFeed {
    market: TychoMarket,
    published: Arc<PublishedMarket>,
    stream: UpdateStream,
}

impl TychoLiveFeed {
    /// Connects to Tycho and initializes the market from its first complete update.
    pub async fn connect(config: TychoStreamConfig) -> Result<Self, TychoFeedError> {
        let tokens = load_all_tokens(
            &config.host,
            false,
            config.api_key.as_deref(),
            true,
            config.chain,
            Some(config.min_token_quality as i32),
            config.traded_n_days_ago,
        )
        .await
        .map_err(|error| TychoFeedError::Transport(error.to_string()))?;
        let filter = ComponentFilter::with_tvl_range(config.min_tvl, config.min_tvl);
        let builder = ProtocolStreamBuilder::new(&config.host, config.chain)
            .exchange::<UniswapV2State>("uniswap_v2", filter.clone(), None)
            .exchange::<UniswapV3State>("uniswap_v3", filter.clone(), None)
            .exchange::<UniswapV4State>("uniswap_v4", filter, None)
            .auth_key(config.api_key)
            .skip_state_decode_failures(true)
            .set_tokens(tokens)
            .await;
        let stream = builder
            .build()
            .await
            .map_err(|error| TychoFeedError::Transport(error.to_string()))?
            .map(|update| update.map_err(|error| error.to_string()));
        let mut stream: UpdateStream = Box::pin(stream);
        let first = stream
            .next()
            .await
            .ok_or(TychoFeedError::StreamEnded)?
            .map_err(TychoFeedError::Transport)?;
        let market = TychoMarket::from_initial_update(first)?;
        let published = Arc::new(PublishedMarket::new(
            market.topology.clone(),
            market.snapshot.clone(),
        ));
        Ok(Self {
            market,
            published,
            stream,
        })
    }

    pub fn published_market(&self) -> Arc<PublishedMarket> {
        Arc::clone(&self.published)
    }

    pub fn token_id(&self, address: Address) -> Option<TokenId> {
        self.market.token_id(address)
    }

    pub fn component_id(&self, pool_id: PoolId) -> Option<&str> {
        self.market.component_id(pool_id)
    }

    /// Consumes and atomically publishes one complete Tycho block update.
    pub async fn next_block(&mut self) -> Result<u64, TychoFeedError> {
        let update = match self.stream.next().await {
            Some(Ok(update)) => update,
            Some(Err(error)) => {
                self.published.set_healthy(false);
                return Err(TychoFeedError::Transport(error));
            }
            None => {
                self.published.set_healthy(false);
                return Err(TychoFeedError::StreamEnded);
            }
        };
        let block = update.block_number_or_timestamp;
        let topology_changed = match self.market.apply_update(update) {
            Ok(changed) => changed,
            Err(error) => {
                self.published.set_healthy(false);
                return Err(error);
            }
        };
        if topology_changed {
            self.published
                .publish_topology(self.market.topology.clone(), self.market.snapshot.clone());
        } else {
            self.published
                .publish_snapshot(self.market.snapshot.clone());
        }
        Ok(block)
    }

    /// Runs the single-writer update loop until the stream fails or ends.
    pub async fn run(mut self) -> Result<(), TychoFeedError> {
        loop {
            self.next_block().await?;
        }
    }
}

impl Drop for TychoLiveFeed {
    fn drop(&mut self) {
        self.published.set_healthy(false);
    }
}

#[derive(Debug)]
pub enum TychoFeedError {
    Transport(String),
    StreamEnded,
    EmptyInitialSnapshot,
    InvalidComponent(String),
    UnknownComponent(Box<str>),
}

impl fmt::Display for TychoFeedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "Tycho transport error: {error}"),
            Self::StreamEnded => formatter.write_str("Tycho stream ended"),
            Self::EmptyInitialSnapshot => formatter.write_str("Tycho initial snapshot is empty"),
            Self::InvalidComponent(component) => {
                write!(formatter, "invalid Tycho component: {component}")
            }
            Self::UnknownComponent(component) => {
                write!(formatter, "unknown Tycho component: {component}")
            }
        }
    }
}

impl Error for TychoFeedError {}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use alloy_primitives::U256;
    use num_bigint::BigUint;
    use tycho_simulation::{
        evm::protocol::{
            uniswap_v2::state::UniswapV2State,
            uniswap_v3::{enums::FeeAmount, state::UniswapV3State},
            uniswap_v4::state::{UniswapV4Fees, UniswapV4State},
            utils::uniswap::tick_list::TickInfo,
        },
        tycho_common::{Bytes, simulation::protocol_sim::ProtocolSim},
        tycho_core::models::token::Token,
    };

    use super::{TychoComponent, TychoMarket, TychoPool, TychoUpdate};
    use crate::{
        Amount, TokenId,
        market::MarketBuilder,
        router::{PathSet, Router},
        simulator::PoolKind,
    };

    fn token(address: &str, symbol: &str) -> Token {
        Token::new(
            &Bytes::from_str(address).unwrap(),
            symbol,
            18,
            0,
            &[],
            Default::default(),
            100,
        )
    }

    #[test]
    fn adapter_matches_tycho_v4_quote() {
        let token_a = token("0x0000000000000000000000000000000000000001", "A");
        let token_b = token("0x0000000000000000000000000000000000000002", "B");
        let state = UniswapV4State::new(
            1_000_000_000_000_000_000,
            U256::from(1_u8) << 96,
            UniswapV4Fees::new(0, 0, 3_000),
            0,
            60,
            vec![
                TickInfo::new(-120, 0).unwrap(),
                TickInfo::new(120, 0).unwrap(),
            ],
        )
        .unwrap();
        let input = BigUint::from(1_000_000_u64);
        let expected = state
            .get_amount_out(input.clone(), &token_a, &token_b)
            .unwrap()
            .amount;

        let a = TokenId(0);
        let b = TokenId(1);
        let mut builder = MarketBuilder::default();
        builder.add_tycho_pool(TychoPool::new(PoolKind::V4, state, a, b, token_a, token_b));
        let (topology, snapshot) = builder.finish(123);
        let paths = PathSet::discover(&topology, a, b, 1);
        let quote = Router::new(&topology, &snapshot)
            .quote_paths(&paths, Amount::from(1_000_000_u64))
            .unwrap();

        assert_eq!(
            quote.amount_out,
            Amount::from_be_slice(&expected.to_bytes_be())
        );
        assert_eq!(quote.block_number, 123);
    }

    #[test]
    fn ingester_keeps_ids_stable_across_snapshots() {
        let address_a = "0x0000000000000000000000000000000000000001";
        let address_b = "0x0000000000000000000000000000000000000002";
        let token_a = token(address_a, "A");
        let token_b = token(address_b, "B");
        let state = UniswapV4State::new(
            1_000_000_000_000_000_000,
            U256::from(1_u8) << 96,
            UniswapV4Fees::new(0, 0, 3_000),
            0,
            60,
            vec![
                TickInfo::new(-120, 0).unwrap(),
                TickInfo::new(120, 0).unwrap(),
            ],
        )
        .unwrap();
        let mut market = TychoMarket::from_components(
            100,
            [TychoComponent {
                component_id: "pool-1".into(),
                token_a_address: address_a.parse().unwrap(),
                token_b_address: address_b.parse().unwrap(),
                token_a: token_a.clone(),
                token_b: token_b.clone(),
                kind: PoolKind::V4,
                state: Box::new(state.clone()),
                protocol_component: None,
            }],
        );
        let old_snapshot = market.snapshot().clone();
        let pool_id = market.pool_id("pool-1").unwrap();

        market
            .apply_updates(
                101,
                [TychoUpdate {
                    component_id: "pool-1".into(),
                    token_a,
                    token_b,
                    state: Box::new(state),
                }],
            )
            .unwrap();

        assert_eq!(old_snapshot.block_number(), 100);
        assert_eq!(market.snapshot().block_number(), 101);
        assert_eq!(market.pool_id("pool-1"), Some(pool_id));
        assert_eq!(
            market.token_id(address_a.parse().unwrap()),
            Some(TokenId(0))
        );
        assert_eq!(
            market.token_id(address_b.parse().unwrap()),
            Some(TokenId(1))
        );
    }

    #[test]
    fn v2_and_v3_adapters_match_tycho_quotes() {
        let token_a = token("0x0000000000000000000000000000000000000001", "A");
        let token_b = token("0x0000000000000000000000000000000000000002", "B");
        let a = TokenId(0);
        let b = TokenId(1);
        let amount = BigUint::from(1_000_000_u64);

        let v2 = UniswapV2State::new(U256::from(1_000_000_000_u64), U256::from(2_000_000_000_u64));
        let expected_v2 = v2
            .get_amount_out(amount.clone(), &token_a, &token_b)
            .unwrap()
            .amount;
        let v2_pool = TychoPool::new(PoolKind::V2, v2, a, b, token_a.clone(), token_b.clone());
        assert_eq!(
            v2_pool.quote_exact_input(a, Amount::from(1_000_000_u64)),
            Some(Amount::from_be_slice(&expected_v2.to_bytes_be()))
        );

        let v3 = UniswapV3State::new(
            1_000_000_000_000_000_000,
            U256::from(1_u8) << 96,
            FeeAmount::Medium,
            0,
            vec![
                TickInfo::new(-120, 0).unwrap(),
                TickInfo::new(120, 0).unwrap(),
            ],
        )
        .unwrap();
        let expected_v3 = v3
            .get_amount_out(amount, &token_a, &token_b)
            .unwrap()
            .amount;
        let v3_pool = TychoPool::new(PoolKind::V3, v3, a, b, token_a, token_b);
        assert_eq!(
            v3_pool.quote_exact_input(a, Amount::from(1_000_000_u64)),
            Some(Amount::from_be_slice(&expected_v3.to_bytes_be()))
        );
    }
}
