use fvm_actor_utils::receiver::RecipientData;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::econ::TokenAmount;

/// A standard fungible token interface allowing for on-chain transactions that implements the
/// FRC-0046 standard. This represents the external interface exposed to other on-chain actors
///
/// Token authors must implement this trait and link the methods to standard dispatch numbers (as
/// defined by [FRC-0042](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0042.md)).
pub trait FRC46Token {
    type TokenError;
    /// Returns the name of the token
    ///
    /// Must not be empty
    fn name(&self) -> String;

    /// Returns the ticker symbol of the token
    ///
    /// Must not be empty. Should be a short uppercase string
    fn symbol(&self) -> String;

    /// Returns the smallest amount of tokens which is indivisible.
    ///
    /// All transfers, burns, and mints must be a whole multiple of the granularity. All balances
    /// must be a multiple of this granularity (but allowances need not be). Must be at least 1.
    /// Must never change.
    ///
    /// A granularity of 10^18 corresponds to whole units only, with no further decimal precision.
    fn granularity(&self) -> GranularityReturn;

    /// Returns the total amount of the token in existence
    ///
    /// Must be non-negative. The total supply must equal the balances of all addresses. The total
    /// supply should equal the sum of all minted tokens less the sum of all burnt tokens.
    fn total_supply(&mut self) -> TotalSupplyReturn;

    /// Returns the balance of an address
    ///
    /// Balance is always non-negative. Uninitialised addresses have an implicit zero balance.
    fn balance_of(&mut self, params: Address) -> Result<BalanceReturn, Self::TokenError>;

    /// Returns the allowance approved for an operator on a spender's balance
    ///
    /// The operator can burn or transfer the allowance amount out of the owner's address.
    fn allowance(
        &mut self,
        params: GetAllowanceParams,
    ) -> Result<AllowanceReturn, Self::TokenError>;

    /// Transfers tokens from the caller to another address
    ///
    /// Amount must be non-negative (but can be zero). Transferring to the caller's own address must
    /// be treated as a normal transfer. Must call the receiver hook on the receiver's address,
    /// failing and aborting the transfer if calling the hook fails or aborts.
    fn transfer(&mut self, params: TransferParams) -> Result<TransferReturn, Self::TokenError>;

    /// Transfers tokens from one address to another
    ///
    /// The caller must have previously approved to control at least the sent amount. If successful,
    /// the amount transferred is deducted from the caller's allowance.
    fn transfer_from(
        &mut self,
        params: TransferFromParams,
    ) -> Result<TransferFromReturn, Self::TokenError>;

    /// Atomically increases the approved allowance that a operator can transfer/burn from the
    /// caller's balance
    ///
    /// The increase must be non-negative. Returns the new total allowance approved for that
    /// owner-operator pair.
    fn increase_allowance(
        &mut self,
        params: IncreaseAllowanceParams,
    ) -> Result<IncreaseAllowanceReturn, Self::TokenError>;

    /// Atomically decreases the approved balance that a operator can transfer/burn from the caller's
    /// balance
    ///
    /// The decrease must be non-negative. Sets the allowance to zero if the decrease is greater
    /// than the currently approved allowance. Returns the new total allowance approved for that
    /// owner-operator pair.
    fn decrease_allowance(
        &mut self,
        params: DecreaseAllowanceParams,
    ) -> Result<DecreaseAllowanceReturn, Self::TokenError>;

    /// Sets the allowance a operator has on the owner's account to zero
    fn revoke_allowance(
        &mut self,
        params: RevokeAllowanceParams,
    ) -> Result<RevokeAllowanceReturn, Self::TokenError>;

    /// Burns tokens from the caller's balance, decreasing the total supply
    fn burn(&mut self, params: BurnParams) -> Result<BurnReturn, Self::TokenError>;

    /// Burns tokens from an address's balance
    ///
    /// The caller must have been previously approved to control at least the burnt amount.
    fn burn_from(&mut self, params: BurnFromParams) -> Result<BurnFromReturn, Self::TokenError>;
}

pub type GranularityReturn = u64;
pub type TotalSupplyReturn = TokenAmount;
pub type BalanceReturn = TokenAmount;
pub type AllowanceReturn = TokenAmount;
pub type IncreaseAllowanceReturn = TokenAmount;
pub type DecreaseAllowanceReturn = TokenAmount;
pub type RevokeAllowanceReturn = ();

/// Return value after a successful mint.
/// The mint method is not standardised, so this is merely a useful library-level type,
/// and recommendation for token implementations.
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct MintReturn {
    /// The new balance of the owner address
    pub balance: TokenAmount,
    /// The new total supply.
    pub supply: TokenAmount,
    /// (Optional) data returned from receiver hook
    pub recipient_data: RawBytes,
}

/// Intermediate data used by mint_return to construct the return data
#[derive(Clone, Debug)]
pub struct MintIntermediate {
    /// Recipient address to use for querying balance
    pub recipient: Address,
    /// (Optional) data returned from receiver hook
    pub recipient_data: RawBytes,
}

impl RecipientData for MintIntermediate {
    fn set_recipient_data(&mut self, data: RawBytes) {
        self.recipient_data = data;
    }
}

/// Instruction to transfer tokens to another address
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TransferParams {
    pub to: Address,
    /// A non-negative amount to transfer
    pub amount: TokenAmount,
    /// Arbitrary data to pass on via the receiver hook
    pub operator_data: RawBytes,
}

/// Return value after a successful transfer
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TransferReturn {
    /// The new balance of the `from` address
    pub from_balance: TokenAmount,
    /// The new balance of the `to` address
    pub to_balance: TokenAmount,
    /// (Optional) data returned from receiver hook
    pub recipient_data: RawBytes,
}

/// Intermediate data used by transfer_return to construct the return data
#[derive(Debug)]
pub struct TransferIntermediate {
    pub from: Address,
    pub to: Address,
    /// (Optional) data returned from receiver hook
    pub recipient_data: RawBytes,
}

impl RecipientData for TransferIntermediate {
    fn set_recipient_data(&mut self, data: RawBytes) {
        self.recipient_data = data;
    }
}

/// Instruction to transfer tokens between two addresses as an operator
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TransferFromParams {
    pub from: Address,
    pub to: Address,
    /// A non-negative amount to transfer
    pub amount: TokenAmount,
    /// Arbitrary data to pass on via the receiver hook
    pub operator_data: RawBytes,
}

/// Return value after a successful delegated transfer
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TransferFromReturn {
    /// The new balance of the `from` address
    pub from_balance: TokenAmount,
    /// The new balance of the `to` address
    pub to_balance: TokenAmount,
    /// The new remaining allowance between `owner` and `operator` (caller)
    pub allowance: TokenAmount,
    /// (Optional) data returned from receiver hook
    pub recipient_data: RawBytes,
}

/// Intermediate data used by transfer_from_return to construct the return data
#[derive(Clone, Debug)]
pub struct TransferFromIntermediate {
    pub operator: Address,
    pub from: Address,
    pub to: Address,
    /// (Optional) data returned from receiver hook
    pub recipient_data: RawBytes,
}

impl RecipientData for TransferFromIntermediate {
    fn set_recipient_data(&mut self, data: RawBytes) {
        self.recipient_data = data;
    }
}

/// Instruction to increase an allowance between two addresses
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct IncreaseAllowanceParams {
    pub operator: Address,
    /// A non-negative amount to increase the allowance by
    pub increase: TokenAmount,
}

/// Instruction to decrease an allowance between two addresses
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct DecreaseAllowanceParams {
    pub operator: Address,
    /// A non-negative amount to decrease the allowance by
    pub decrease: TokenAmount,
}

/// Instruction to revoke (set to 0) an allowance
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct RevokeAllowanceParams {
    pub operator: Address,
}

/// Params to get allowance between to addresses
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct GetAllowanceParams {
    pub owner: Address,
    pub operator: Address,
}

/// Instruction to burn an amount of tokens
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct BurnParams {
    /// A non-negative amount to burn
    pub amount: TokenAmount,
}

/// The updated value after burning
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct BurnReturn {
    /// New balance in the account after the successful burn
    pub balance: TokenAmount,
}

/// Instruction to burn an amount of tokens from another address
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct BurnFromParams {
    pub owner: Address,
    /// A non-negative amount to burn
    pub amount: TokenAmount,
}

/// The updated value after a delegated burn
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct BurnFromReturn {
    /// New balance in the account after the successful burn
    pub balance: TokenAmount,
    /// New remaining allowance between the owner and operator (caller)
    pub allowance: TokenAmount,
}
