pub mod receiver;

use fvm_shared::address::Address;
pub use fvm_shared::econ::TokenAmount;

pub type Result<T> = std::result::Result<T, TokenError>;
pub enum TokenError {}

/// A standard fungible token interface allowing for on-chain transactions
pub trait Token {
    fn name() -> String;

    fn symbol() -> String;

    fn total_supply() -> TokenAmount;

    fn balance_of(holder: Address) -> Result<TokenAmount>;

    fn increase_allowance(spender: Address, value: TokenAmount) -> Result<TokenAmount>;

    fn decrease_allowance(spender: Address, value: TokenAmount) -> Result<TokenAmount>;

    fn revoke_allowance(spender: Address) -> Result<()>;

    fn allowance(owner: Address, spender: Address) -> Result<TokenAmount>;

    fn burn(amount: TokenAmount, data: &[u8]) -> Result<TokenAmount>;

    fn burn_from(from: Address, amount: TokenAmount, data: &[u8]) -> Result<TokenAmount>;
}
