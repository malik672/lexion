use crate::{Amount, TokenId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolKind {
    V2,
    #[cfg(feature = "tycho")]
    V3,
    #[cfg(feature = "tycho")]
    V4,
}

/// Mutable simulator state stored in the slot identified by a `PoolId`.
#[derive(Clone, Debug)]
pub enum PoolState {
    V2(V2State),
    #[cfg(feature = "tycho")]
    Tycho(Box<crate::tycho::TychoPool>),
}

impl PoolState {
    pub fn kind(&self) -> PoolKind {
        match self {
            Self::V2(_) => PoolKind::V2,
            #[cfg(feature = "tycho")]
            Self::Tycho(state) => state.kind(),
        }
    }

    pub fn quote_exact_input(
        &self,
        token_in: TokenId,
        token_a: TokenId,
        token_b: TokenId,
        amount_in: Amount,
    ) -> Option<Amount> {
        match self {
            Self::V2(state) => state.quote_exact_input(token_in, token_a, token_b, amount_in),
            #[cfg(feature = "tycho")]
            Self::Tycho(state) => state.quote_exact_input(token_in, amount_in),
        }
    }

    pub fn quote_exact_input_with_gas(
        &self,
        token_in: TokenId,
        token_a: TokenId,
        token_b: TokenId,
        amount_in: Amount,
    ) -> Option<(Amount, Amount)> {
        match self {
            Self::V2(state) => state
                .quote_exact_input(token_in, token_a, token_b, amount_in)
                .map(|amount| (amount, Amount::ZERO)),
            #[cfg(feature = "tycho")]
            Self::Tycho(state) => state.quote_exact_input_with_gas(token_in, amount_in),
        }
    }

    pub fn as_v2(&self) -> Option<&V2State> {
        match self {
            Self::V2(state) => Some(state),
            #[cfg(feature = "tycho")]
            Self::Tycho(_) => None,
        }
    }

    #[cfg(feature = "tycho")]
    pub fn as_tycho(&self) -> Option<&crate::tycho::TychoPool> {
        match self {
            Self::Tycho(state) => Some(state),
            Self::V2(_) => None,
        }
    }
}

/// Integer state required to simulate a constant-product V2 pool.
#[derive(Clone, Debug)]
pub struct V2State {
    reserve_a: Amount,
    reserve_b: Amount,
    fee_bps: u16,
}

impl V2State {
    pub fn new(reserve_a: Amount, reserve_b: Amount, fee_bps: u16) -> Self {
        assert!(
            !reserve_a.is_zero() && !reserve_b.is_zero(),
            "pool reserves must be nonzero"
        );
        assert!(fee_bps < 10_000, "fee must be below 100%");
        Self {
            reserve_a,
            reserve_b,
            fee_bps,
        }
    }

    pub fn reserves(&self) -> (Amount, Amount) {
        (self.reserve_a, self.reserve_b)
    }

    pub fn fee_bps(&self) -> u16 {
        self.fee_bps
    }

    fn quote_exact_input(
        &self,
        token_in: TokenId,
        token_a: TokenId,
        token_b: TokenId,
        amount_in: Amount,
    ) -> Option<Amount> {
        let (reserve_in, reserve_out) = if token_in == token_a {
            (self.reserve_a, self.reserve_b)
        } else if token_in == token_b {
            (self.reserve_b, self.reserve_a)
        } else {
            return None;
        };

        if amount_in.is_zero() {
            return Some(Amount::ZERO);
        }
        let scale = Amount::from(10_000);
        let amount_with_fee = amount_in.checked_mul(scale - Amount::from(self.fee_bps))?;
        let numerator = amount_with_fee.checked_mul(reserve_out)?;
        let denominator = reserve_in
            .checked_mul(scale)?
            .checked_add(amount_with_fee)?;
        Some(numerator / denominator)
    }
}
