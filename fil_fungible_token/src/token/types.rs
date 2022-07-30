use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::econ::TokenAmount;
use thiserror::Error;

use super::TokenError;

#[derive(Error, Debug)]
pub enum ActorError<Err> {
    #[error("token error: {0}")]
    Token(#[from] TokenError),
    #[error("error during actor execution: {0}")]
    Runtime(Err),
}

pub type Result<T, E> = std::result::Result<T, ActorError<E>>;

/// A standard fungible token interface allowing for on-chain transactions that implements the
/// FRC-XXX standard. This represents the external interface exposed to other on-chain actors
///
/// Token authors should implement this trait and link the methods to standard dispatch numbers in
/// their actor's `invoke` entrypoint. A standard helper (TODO) is provided to aid method dispatch.
///
/// TODO: make non-pseudo code
/// ```
/// //struct Token {}
/// //impl FrcXXXToken for Token {
/// //    ...
/// //}
///
/// //fn invoke(params: u32) -> u32 {
/// //    let token = Token {};
///
/// //    match sdk::message::method_number() {
/// //        1 => constructor(),
/// //        2 => token.name(),
/// //        _ => abort!()
/// //        // etc.
/// //    }
/// //}
/// ```
pub trait FrcXXXToken<E> {
    /// Returns the name of the token
    fn name(&self) -> String;

    /// Returns the ticker symbol of the token
    fn symbol(&self) -> String;

    /// Returns the total amount of the token in existence
    fn total_supply(&self) -> TokenAmount;

    /// Gets the balance of a particular address (if it exists)
    ///
    /// This will method attempt to resolve addresses to ID-addresses
    fn balance_of(&self, params: Address) -> Result<TokenAmount, E>;

    /// Atomically increase the amount that a spender can pull from the owner account
    ///
    /// The increase must be non-negative. Returns the new allowance between those two addresses if
    /// successful
    fn increase_allowance(&mut self, params: ChangeAllowanceParams) -> Result<AllowanceReturn, E>;

    /// Atomically decrease the amount that a spender can pull from an account
    ///
    /// The decrease must be non-negative. The resulting allowance is set to zero if the decrease is
    /// more than the current allowance. Returns the new allowance between the two addresses if
    /// successful
    fn decrease_allowance(&mut self, params: ChangeAllowanceParams) -> Result<AllowanceReturn, E>;

    /// Set the allowance a spender has on the owner's account to zero
    fn revoke_allowance(&mut self, params: RevokeAllowanceParams) -> Result<AllowanceReturn, E>;

    /// Get the allowance between two addresses
    ///
    /// The spender can burn or transfer the allowance amount out of the owner's address. If the
    /// address of the owner cannot be resolved, this method returns an error. If the owner can be
    /// resolved, but the spender address is not registered with an allowance, an implicit allowance
    /// of 0 is returned
    fn allowance(&self, params: GetAllowanceParams) -> Result<AllowanceReturn, E>;

    /// Burn tokens from the caller's account, decreasing the total supply
    ///
    /// When burning tokens:
    /// - Any holder MUST be allowed to burn their own tokens
    /// - The balance of the holder MUST decrease by the amount burned
    /// - This method MUST revert if the burn amount is more than the holder's balance
    fn burn(&mut self, params: BurnParams) -> Result<BurnReturn, E>;

    /// Transfer tokens from the caller to the receiver
    ///
    fn transfer(&mut self, params: TransferParams) -> Result<TransferReturn, E>;
}

/// An amount to increase or decrease an allowance by
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct ChangeAllowanceParams {
    pub spender: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

/// Params to get allowance between to addresses
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct GetAllowanceParams {
    pub owner: Address,
    pub spender: Address,
}

/// Instruction to revoke (set to 0) an allowance
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct RevokeAllowanceParams {
    pub owner: Address,
    pub spender: Address,
}

/// The updated value after allowance is increased or decreased
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct AllowanceReturn {
    pub owner: Address,
    pub spender: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

impl Cbor for ChangeAllowanceParams {}
impl Cbor for GetAllowanceParams {}
impl Cbor for RevokeAllowanceParams {}
impl Cbor for AllowanceReturn {}

/// Burns an amount of token from an address
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct BurnParams {
    pub owner: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct BurnReturn {
    pub by: Address,
    pub owner: Address,
    #[serde(with = "bigint_ser")]
    pub burnt: TokenAmount,
    #[serde(with = "bigint_ser")]
    pub remaining_balance: TokenAmount,
}

impl Cbor for BurnParams {}
impl Cbor for BurnReturn {}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct TransferParams {
    pub from: Address,
    pub to: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
    pub data: RawBytes,
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct TransferReturn {
    pub from: Address,
    pub to: Address,
    pub by: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

impl Cbor for TransferParams {}
impl Cbor for TransferReturn {}
