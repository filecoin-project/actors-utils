use fvm_shared::{address::Address, econ::TokenAmount};

pub type Result<T> = std::result::Result<T, ReceiverError>;

pub enum ReceiverError {}

/// Standard interface for a contract that wishes to receive tokens
pub trait TokenReceiver {
    fn token_received(from: Address, amount: TokenAmount, data: &[u8]) -> Result<u32>;
}
