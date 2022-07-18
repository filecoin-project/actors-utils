use anyhow::Result;
use fvm_shared::{address::Address, econ::TokenAmount};

/// Standard interface for a contract that wishes to receive tokens
pub trait TokenReceiver {
    fn token_received(from: Address, amount: TokenAmount, data: &[u8]) -> Result<u32>;
}
