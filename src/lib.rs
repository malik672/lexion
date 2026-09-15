//! The small, simulator-backed core of Lexion.

pub mod allocator;
mod certified_v2;
#[cfg(feature = "tycho")]
pub mod execution;
pub mod frontier;
pub mod market;
pub mod portfolio;
pub mod router;
pub mod runtime;
pub mod simulator;
pub mod solver;
#[cfg(feature = "tycho")]
pub mod tycho;

/// Integer token quantity used by the first implementation.
pub type Amount = alloy_primitives::U256;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TokenId(pub u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PoolId(pub u32);
