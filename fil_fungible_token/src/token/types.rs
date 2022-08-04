use anyhow::Result;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;

// TODO: finalise this spec and remove anyhow!

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
pub trait FrcXXXToken {
    /// Returns the name of the token
    fn name(&self) -> String;

    /// Returns the ticker symbol of the token
    fn symbol(&self) -> String;

    /// Returns the total amount of the token in existence
    fn total_supply(&self) -> TokenAmount;

    /// Gets the balance of a particular address (if it exists)
    ///
    /// This will method attempt to resolve addresses to ID-addresses
    fn balance_of(&self, params: Address) -> Result<TokenAmount>;

    /// Atomically increase the amount that a operator can pull from the owner account
    ///
    /// The increase must be non-negative. Returns the new allowance between those two addresses if
    /// successful
    fn increase_allowance(&self, params: ChangeAllowanceParams) -> Result<AllowanceReturn>;

    /// Atomically decrease the amount that a operator can pull from an account
    ///
    /// The decrease must be non-negative. The resulting allowance is set to zero if the decrease is
    /// more than the current allowance. Returns the new allowance between the two addresses if
    /// successful
    fn decrease_allowance(&self, params: ChangeAllowanceParams) -> Result<AllowanceReturn>;

    /// Set the allowance a operator has on the owner's account to zero
    fn revoke_allowance(&self, params: RevokeAllowanceParams) -> Result<AllowanceReturn>;

    /// Get the allowance between two addresses
    ///
    /// The operator can burn or transfer the allowance amount out of the owner's address. If the
    /// address of the owner cannot be resolved, this method returns an error. If the owner can be
    /// resolved, but the operator address is not registered with an allowance, an implicit allowance
    /// of 0 is returned
    fn allowance(&self, params: GetAllowanceParams) -> Result<AllowanceReturn>;

    /// Burn tokens from the caller's account, decreasing the total supply
    ///
    /// When burning tokens:
    /// - Any owner MUST be allowed to burn their own tokens
    /// - The balance of the owner MUST decrease by the amount burned
    /// - This method MUST revert if the burn amount is more than the owner's balance
    fn burn(&self, params: BurnParams) -> Result<BurnReturn>;

    /// Transfer tokens from one account to another
    fn transfer(&self, params: TransferParams) -> Result<TransferReturn>;
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct MintParams {
    pub initial_owner: ActorID,
    #[serde(with = "bigint_ser")]
    pub amount: TokenAmount,
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct MintReturn {
    pub successful: bool,
    #[serde(with = "bigint_ser")]
    pub newly_minted: TokenAmount,
    #[serde(with = "bigint_ser")]
    pub total_supply: TokenAmount,
}

impl Cbor for MintParams {}
impl Cbor for MintReturn {}

/// An amount to increase or decrease an allowance by
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct ChangeAllowanceParams {
    pub owner: Address,
    pub operator: Address,
    #[serde(with = "bigint_ser")]
    pub amount: TokenAmount,
}

/// Params to get allowance between to addresses
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct GetAllowanceParams {
    pub owner: Address,
    pub operator: Address,
}

/// Instruction to revoke (set to 0) an allowance
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct RevokeAllowanceParams {
    pub owner: Address,
    pub operator: Address,
}

/// The updated value after allowance is increased or decreased
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct AllowanceReturn {
    pub owner: Address,
    pub operator: Address,
    #[serde(with = "bigint_ser")]
    pub amount: TokenAmount,
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
    pub amount: TokenAmount,
    pub data: RawBytes,
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct BurnReturn {
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
    pub amount: TokenAmount,
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct TransferReturn {
    pub from: Address,
    pub to: Address,
    #[serde(with = "bigint_ser")]
    pub amount: TokenAmount,
}

impl Cbor for TransferParams {}
impl Cbor for TransferReturn {}
