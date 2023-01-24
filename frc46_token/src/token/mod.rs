use std::ops::Neg;

use cid::Cid;
pub use error::TokenError;
use fvm_actor_utils::messaging::{MessagingError, RECEIVER_HOOK_METHOD_NUM};
use fvm_actor_utils::receiver::{ReceiverHook, ReceiverHookError};
use fvm_actor_utils::syscalls::Syscalls;
use fvm_actor_utils::util::ActorRuntime;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::ipld_block::IpldBlock;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use num_traits::Zero;

use self::state::{StateError as TokenStateError, StateInvariantError, StateSummary, TokenState};
use self::types::TransferFromIntermediate;
use self::types::TransferFromReturn;
use self::types::TransferReturn;
use self::types::{BurnFromReturn, MintIntermediate};
use self::types::{BurnReturn, TransferIntermediate};
use crate::receiver::{FRC46ReceiverHook, FRC46TokenReceived};
use crate::token::types::MintReturn;
use crate::token::TokenError::InvalidGranularity;

mod error;
pub mod state;
pub mod types;

/// Ratio of integral units to interpretation as standard token units, as given by FRC-0046.
/// Aka "18 decimals".
pub const TOKEN_PRECISION: u64 = 1_000_000_000_000_000_000;

type Result<T> = std::result::Result<T, TokenError>;

/// Library functions that implement core FRC-??? standards
///
/// Holds injectable services to access/interface with IPLD/FVM layer.
pub struct Token<'st, S, BS>
where
    S: Syscalls,
    BS: Blockstore,
{
    /// Runtime services to interact with the execution environment
    runtime: ActorRuntime<S, BS>,
    /// Reference to token state that will be inspected/mutated
    state: &'st mut TokenState,
    /// Minimum granularity of token amounts.
    /// All balances and amounts must be a multiple of this granularity.
    /// Set to 1 for standard 18-dp precision, TOKEN_PRECISION for whole units only, or some
    /// value in between.
    granularity: u64,
}

impl<'st, S, BS> Token<'st, S, BS>
where
    S: Syscalls,
    BS: Blockstore,
{
    /// Creates a new clean token state instance
    ///
    /// This should be wrapped in a Token handle for convenience. Must be flushed to the blockstore
    /// explicitly to persist changes
    pub fn create_state(bs: &BS) -> Result<TokenState> {
        Ok(TokenState::new(bs)?)
    }

    /// Creates a new clean token state instance, specifying the underlying Hamt bit widths
    ///
    /// This should be wrapped in a Token handle for convenience. Must be flushed to the blockstore
    /// explicitly to persist changes
    pub fn create_state_with_bit_width(bs: &BS, hamt_bit_width: u32) -> Result<TokenState> {
        Ok(TokenState::new_with_bit_width(bs, hamt_bit_width)?)
    }

    /// Wrap an existing token state
    pub fn wrap(
        runtime: ActorRuntime<S, BS>,
        granularity: u64,
        state: &'st mut TokenState,
    ) -> Self {
        Self { runtime, granularity, state }
    }

    /// Replace the current state with another
    /// The previous state is returned and can be safely dropped
    pub fn replace(&mut self, state: TokenState) -> TokenState {
        std::mem::replace(self.state, state)
    }

    /// For an already initialised state tree, loads the state tree from the blockstore at a Cid
    pub fn load_state(bs: &BS, state_cid: &Cid) -> Result<TokenState> {
        Ok(TokenState::load(bs, state_cid)?)
    }

    /// Loads a fresh copy of the state from a blockstore from a given cid, replacing existing state
    /// The old state is returned to enable comparisons and the like but can be safely dropped otherwise
    pub fn load_replace(&mut self, cid: &Cid) -> Result<TokenState> {
        let new_state = TokenState::load(&self.runtime, cid)?;
        Ok(std::mem::replace(self.state, new_state))
    }

    /// Flush state and return Cid for root
    pub fn flush(&mut self) -> Result<Cid> {
        Ok(self.state.save(&self.runtime)?)
    }

    /// Get a reference to the wrapped state tree
    pub fn state(&self) -> &TokenState {
        self.state
    }

    /// Get a reference to the underlying runtime
    pub fn runtime(&self) -> &ActorRuntime<S, BS> {
        &self.runtime
    }

    /// Opens an atomic transaction on TokenState which allows a closure to make multiple
    /// modifications to the state tree.
    ///
    /// If the closure returns an error, the transaction is dropped atomically and no change is
    /// observed on token state.
    fn transaction<F, Res>(&mut self, f: F) -> Result<Res>
    where
        F: FnOnce(&mut TokenState, &ActorRuntime<S, BS>) -> Result<Res>,
    {
        let mut mutable_state = self.state.clone();
        let res = f(&mut mutable_state, &self.runtime)?;
        // if closure didn't error, save state
        *self.state = mutable_state;
        Ok(res)
    }
}

impl<'st, S, BS> Token<'st, S, BS>
where
    S: Syscalls,
    BS: Blockstore,
{
    /// Returns the smallest amount of tokens which is indivisible
    ///
    /// Transfers and balances must be in multiples of granularity but allowances need not be.
    /// Granularity never changes after it is initially set
    pub fn granularity(&self) -> u64 {
        self.granularity
    }

    /// Mints the specified value of tokens into an account
    ///
    /// The minter is implicitly defined as the caller of the actor, and must be an ID address.
    /// The mint amount must be non-negative or the method returns an error.
    ///
    /// Returns a ReceiverHook to call the owner's token receiver hook,
    /// and the owner's new balance.
    /// ReceiverHook must be called or it will panic and abort the transaction.
    ///
    /// The hook call will return a MintIntermediate struct which must be passed to mint_return
    /// to get the final return data
    pub fn mint(
        &mut self,
        operator: &Address,
        initial_owner: &Address,
        amount: &TokenAmount,
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<MintIntermediate>> {
        let amount = validate_amount_with_granularity(amount, "mint", self.granularity)?;
        // init the operator account so that its actor ID can be referenced in the receiver hook
        let operator_id = self.runtime.resolve_or_init(operator)?;
        // init the owner account as allowance and balance checks are not performed for minting
        let owner_id = self.runtime.resolve_or_init(initial_owner)?;

        // Increase the balance of the actor and increase total supply
        let result = self.transaction(|state, bs| {
            state.change_balance_by(&bs, owner_id, amount)?;
            state.change_supply_by(amount)?;
            Ok(MintIntermediate { recipient: *initial_owner, recipient_data: RawBytes::default() })
        })?;

        // return the params we'll send to the receiver hook
        let params = FRC46TokenReceived {
            operator: operator_id,
            from: self.runtime.actor_id(),
            to: owner_id,
            amount: amount.clone(),
            operator_data,
            token_data,
        };

        Ok(ReceiverHook::new_frc46(*initial_owner, params, result)?)
    }

    /// Finalise return data from MintIntermediate data returned by calling receiver hook after minting
    /// This is done to allow reloading the state if it changed as a result of the hook call
    /// so we can return an accurate balance even if the receiver transferred or burned tokens upon receipt
    pub fn mint_return(&self, intermediate: MintIntermediate) -> Result<MintReturn> {
        Ok(MintReturn {
            balance: self.balance_of(&intermediate.recipient)?,
            supply: self.total_supply(),
            recipient_data: intermediate.recipient_data,
        })
    }

    /// Gets the total number of tokens in existence
    ///
    /// This equals the sum of `balance_of` called on all addresses. This equals sum of all
    /// successful `mint` calls minus the sum of all successful `burn`/`burn_from` calls
    pub fn total_supply(&self) -> TokenAmount {
        self.state.supply.clone()
    }

    /// Returns the balance associated with a particular address
    ///
    /// Accounts that have never received transfers implicitly have a zero-balance
    pub fn balance_of(&self, owner: &Address) -> Result<TokenAmount> {
        // Don't instantiate an account if unable to resolve to an ID address, as non-initialized
        // addresses have an implicit zero balance
        match self.runtime.resolve_id(owner) {
            Ok(owner) => Ok(self.state.get_balance(&self.runtime, owner)?),
            Err(MessagingError::AddressNotResolved(_)) => {
                // uninitialized address has implicit zero balance
                Ok(TokenAmount::zero())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Gets the allowance between owner and operator
    ///
    /// An allowance is the amount that the operator can transfer or burn out of the owner's account
    /// via the `transfer` and `burn` methods.
    pub fn allowance(&self, owner: &Address, operator: &Address) -> Result<TokenAmount> {
        // Don't instantiate an account if unable to resolve owner-ID, as non-initialized addresses
        // give implicit zero allowances to all addresses
        let owner = match self.runtime.resolve_id(owner) {
            Ok(owner) => owner,
            Err(MessagingError::AddressNotResolved(_)) => {
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };

        // Don't instantiate an account if unable to resolve operator-ID, as non-initialized
        // addresses have an implicit zero allowance
        let operator = match self.runtime.resolve_id(operator) {
            Ok(operator) => operator,
            Err(MessagingError::AddressNotResolved(_)) => {
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };

        // For concretely resolved accounts, retrieve the allowance from the map
        Ok(self.state.get_allowance_between(&self.runtime, owner, operator)?)
    }

    /// Increase the allowance that an operator can control of an owner's balance by the requested delta
    ///
    /// Returns an error if requested delta is negative or there are errors in (de)serialization of
    /// state.If either owner or operator addresses are not resolvable and cannot be initialised, this
    /// method returns MessagingError::AddressNotInitialized.
    ///
    /// Else returns the new allowance
    pub fn increase_allowance(
        &mut self,
        owner: &Address,
        operator: &Address,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        let delta = validate_allowance(delta, "increase allowance delta")?;

        // Attempt to instantiate the accounts if they don't exist
        let owner = self.runtime.resolve_or_init(owner)?;
        let operator = self.runtime.resolve_or_init(operator)?;
        let new_amount = self.state.change_allowance_by(&self.runtime, owner, operator, delta)?;

        Ok(new_amount)
    }

    /// Decrease the allowance that an operator controls of the owner's balance by the requested delta
    ///
    /// Returns an error if requested delta is negative or there are errors in (de)serialization of
    /// of state. If the resulting allowance would be negative, the allowance between owner and operator is
    /// set to zero.Returns an error if either the operator or owner addresses are not resolvable and
    /// cannot be initialized.
    ///
    /// Else returns the new allowance
    pub fn decrease_allowance(
        &mut self,
        owner: &Address,
        operator: &Address,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        let delta = validate_allowance(delta, "decrease allowance delta")?;

        // Attempt to instantiate the accounts if they don't exist
        let owner = self.runtime.resolve_or_init(owner)?;
        let operator = self.runtime.resolve_or_init(operator)?;
        let new_allowance =
            self.state.change_allowance_by(&self.runtime, owner, operator, &delta.neg())?;

        Ok(new_allowance)
    }

    /// Sets the allowance between owner and operator to zero, returning the old allowance
    pub fn revoke_allowance(&mut self, owner: &Address, operator: &Address) -> Result<TokenAmount> {
        let owner = match self.runtime.resolve_id(owner) {
            Ok(owner) => owner,
            Err(MessagingError::AddressNotResolved(_)) => {
                // uninitialized address has implicit zero allowance already
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };
        let operator = match self.runtime.resolve_id(operator) {
            Ok(operator) => operator,
            Err(MessagingError::AddressNotResolved(_)) => {
                // uninitialized address has implicit zero allowance already
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };
        // if both accounts resolved, explicitly set allowance to zero
        Ok(self.state.revoke_allowance(&self.runtime, owner, operator)?)
    }

    /// Sets the allowance to a specified amount, returning the old allowance
    pub fn set_allowance(
        &mut self,
        owner: &Address,
        operator: &Address,
        amount: &TokenAmount,
    ) -> Result<TokenAmount> {
        let amount = validate_allowance(amount, "set allowance amount")?;

        // Handle special revoke allowance case to avoid unnecessary account initialization
        if amount.is_zero() {
            return self.revoke_allowance(owner, operator);
        }

        // Attempt to instantiate the accounts if they don't exist
        let owner = self.runtime.resolve_or_init(owner)?;
        let operator = self.runtime.resolve_or_init(operator)?;

        // if both accounts resolved, explicitly set allowance
        Ok(self.state.set_allowance(&self.runtime, owner, operator, amount)?)
    }

    /// Burns an amount of token from the specified address, decreasing total token supply
    ///
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the target's balance
    /// - If the burn operation would result in a negative balance for the owner, the burn is
    /// discarded and this method returns an error
    ///
    /// Upon successful burn
    /// - The target's balance decreases by the requested value
    /// - The total_supply decreases by the requested value
    pub fn burn(&mut self, owner: &Address, amount: &TokenAmount) -> Result<BurnReturn> {
        let amount = validate_amount_with_granularity(amount, "burn", self.granularity)?;

        let owner = self.runtime.resolve_or_init(owner)?;
        self.transaction(|state, bs| {
            // attempt to burn the requested amount
            let new_amount = state.change_balance_by(&bs, owner, &amount.clone().neg())?;
            // decrease total_supply
            state.change_supply_by(&amount.neg())?;
            Ok(BurnReturn { balance: new_amount })
        })
    }

    /// Burns an amount of token from the specified address, decreasing total token supply
    ///
    /// If operator and owner are the same address, this method returns an InvalidOperator error.
    ///
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the target's balance
    /// - If the burn operation would result in a negative balance for the owner, the burn is
    /// discarded and this method returns an error
    /// - The operator MUST have an allowance not less than the requested value
    ///
    /// Upon successful burn
    /// - The target's balance decreases by the requested value
    /// - The total_supply decreases by the requested value
    /// - The operator's allowance is decreased by the requested value
    pub fn burn_from(
        &mut self,
        operator: &Address,
        owner: &Address,
        amount: &TokenAmount,
    ) -> Result<BurnFromReturn> {
        let amount = validate_amount_with_granularity(amount, "burn", self.granularity)?;
        if self.runtime.same_address(operator, owner) {
            return Err(TokenError::InvalidOperator(*operator));
        }

        // operator must exist to have a non-zero allowance
        let operator = match self.runtime.resolve_id(operator) {
            Ok(operator) => operator,
            Err(MessagingError::AddressNotResolved(addr)) => {
                // if not resolved, implicit zero allowance is not permitted to burn, so return an
                // insufficient allowance error
                return Err(TokenStateError::InsufficientAllowance {
                    owner: *owner,
                    operator: addr,
                    allowance: TokenAmount::zero(),
                    delta: amount.clone(),
                }
                .into());
            }
            Err(e) => return Err(e.into()),
        };

        // owner must exist to have set a non-zero allowance
        let owner = match self.runtime.resolve_id(owner) {
            Ok(owner) => owner,
            Err(MessagingError::AddressNotResolved(addr)) => {
                return Err(TokenStateError::InsufficientAllowance {
                    owner: *owner,
                    operator: addr,
                    allowance: TokenAmount::zero(),
                    delta: amount.clone(),
                }
                .into());
            }
            Err(e) => return Err(e.into()),
        };

        self.transaction(|state, bs| {
            let new_allowance = state.attempt_use_allowance(&bs, operator, owner, amount)?;
            // attempt to burn the requested amount
            let new_balance = state.change_balance_by(&bs, owner, &amount.clone().neg())?;
            // decrease total_supply
            state.change_supply_by(&amount.neg())?;
            Ok(BurnFromReturn { balance: new_balance, allowance: new_allowance })
        })
    }

    /// Transfers an amount from the caller to another address
    ///
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the sender's balance
    /// - The receiving actor MUST implement a method called `tokens_received`, corresponding to the
    /// interface specified for FRC-0046 token receiver. If the receiving hook aborts, when called,
    /// the transfer is discarded and this method returns an error
    ///
    /// Upon successful transfer:
    /// - The from balance decreases by the requested value
    /// - The to balance increases by the requested value
    ///
    /// Returns a ReceiverHook to call the recipient's token receiver hook,
    /// and a TransferIntermediate struct
    /// ReceiverHook must be called or it will panic and abort the transaction.
    ///
    /// Return data from the hook should be passed to transfer_return which will generate
    /// the Transfereturn struct
    pub fn transfer(
        &mut self,
        from: &Address,
        to: &Address,
        amount: &TokenAmount,
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferIntermediate>> {
        let amount = validate_amount_with_granularity(amount, "transfer", self.granularity)?;

        // owner-initiated transfer
        let from_id = self.runtime.resolve_or_init(from)?;
        let to_id = self.runtime.resolve_or_init(to)?;
        // skip allowance check for self-managed transfers
        let res = self.transaction(|state, bs| {
            // don't change balance if to == from, but must check that the transfer doesn't exceed balance
            if to_id == from_id {
                let balance = state.get_balance(&bs, from_id)?;
                if balance.lt(amount) {
                    return Err(TokenStateError::InsufficientBalance {
                        owner: from_id,
                        balance,
                        delta: amount.clone().neg(),
                    }
                    .into());
                }
                Ok(TransferIntermediate {
                    from: *from,
                    to: *to,
                    recipient_data: RawBytes::default(),
                })
            } else {
                state.change_balance_by(&bs, to_id, amount)?;
                state.change_balance_by(&bs, from_id, &amount.neg())?;
                Ok(TransferIntermediate {
                    from: *from,
                    to: *to,
                    recipient_data: RawBytes::default(),
                })
            }
        })?;

        let params = FRC46TokenReceived {
            operator: from_id,
            from: from_id,
            to: to_id,
            amount: amount.clone(),
            operator_data,
            token_data,
        };

        Ok(ReceiverHook::new_frc46(*to, params, res)?)
    }

    /// Generate TransferReturn from the intermediate data returned by a receiver hook call
    pub fn transfer_return(&self, intermediate: TransferIntermediate) -> Result<TransferReturn> {
        Ok(TransferReturn {
            from_balance: self.balance_of(&intermediate.from)?,
            to_balance: self.balance_of(&intermediate.to)?,
            recipient_data: intermediate.recipient_data,
        })
    }

    /// Transfers an amount from one address to another
    ///
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the sender's balance
    /// - The receiving actor MUST implement a method called `tokens_received`, corresponding to the
    /// interface specified for FRC-0046 token receiver. If the receiving hook aborts, when called,
    /// the transfer is discarded and this method returns an error
    ///  - The operator MUST be initialised AND have an allowance not less than the requested value
    ///
    /// Upon successful transfer:
    /// - The from balance decreases by the requested value
    /// - The to balance increases by the requested value
    /// - The owner-operator allowance decreases by the requested value
    ///
    /// Returns a ReceiverHook to call the recipient's token receiver hook,
    /// and a TransferFromIntermediate struct.
    /// ReceiverHook must be called or it will panic and abort the transaction.
    ///
    /// Return data from the hook should be passed to transfer_from_return which will generate
    /// the TransferFromReturn struct
    pub fn transfer_from(
        &mut self,
        operator: &Address,
        from: &Address,
        to: &Address,
        amount: &TokenAmount,
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferFromIntermediate>> {
        let amount = validate_amount_with_granularity(amount, "transfer", self.granularity)?;
        if self.runtime.same_address(operator, from) {
            return Err(TokenError::InvalidOperator(*operator));
        }

        // operator-initiated transfer must have a resolvable operator
        let operator_id = match self.runtime.resolve_id(operator) {
            // if operator resolved, we can continue with other checks
            Ok(id) => id,
            // if we cannot resolve the operator, they are forbidden to transfer
            Err(MessagingError::AddressNotResolved(_)) => {
                return Err(TokenError::TokenState(TokenStateError::InsufficientAllowance {
                    operator: *operator,
                    owner: *from,
                    allowance: TokenAmount::zero(),
                    delta: amount.clone(),
                }));
            }
            Err(e) => return Err(e.into()),
        };

        // the owner must exist to have specified a non-zero allowance
        let from_id = match self.runtime.resolve_id(from) {
            Ok(id) => id,
            Err(MessagingError::AddressNotResolved(from)) => {
                return Err(TokenError::TokenState(TokenStateError::InsufficientAllowance {
                    operator: *operator,
                    owner: from,
                    allowance: TokenAmount::zero(),
                    delta: amount.clone(),
                }));
            }
            Err(e) => return Err(e.into()),
        };

        // attempt to initialize the receiving account if not present
        let to_id = self.runtime.resolve_or_init(to)?;

        // update token state
        let ret = self.transaction(|state, bs| {
            state.attempt_use_allowance(&bs, operator_id, from_id, amount)?;
            // don't change balance if to == from, but must check that the transfer doesn't exceed balance
            if to_id == from_id {
                let balance = state.get_balance(&bs, from_id)?;
                if balance.lt(amount) {
                    return Err(TokenStateError::InsufficientBalance {
                        owner: from_id,
                        balance,
                        delta: amount.clone().neg(),
                    }
                    .into());
                }
                Ok(TransferFromIntermediate {
                    operator: *operator,
                    from: *from,
                    to: *to,
                    recipient_data: RawBytes::default(),
                })
            } else {
                state.change_balance_by(&bs, to_id, amount)?;
                state.change_balance_by(&bs, from_id, &amount.neg())?;
                Ok(TransferFromIntermediate {
                    operator: *operator,
                    from: *from,
                    to: *to,
                    recipient_data: RawBytes::default(),
                })
            }
        })?;

        let params = FRC46TokenReceived {
            operator: operator_id,
            from: from_id,
            to: to_id,
            amount: amount.clone(),
            operator_data,
            token_data,
        };

        Ok(ReceiverHook::new_frc46(*to, params, ret)?)
    }

    /// Generate TransferReturn from the intermediate data returned by a receiver hook call
    pub fn transfer_from_return(
        &self,
        intermediate: TransferFromIntermediate,
    ) -> Result<TransferFromReturn> {
        Ok(TransferFromReturn {
            from_balance: self.balance_of(&intermediate.from)?,
            to_balance: self.balance_of(&intermediate.to)?,
            allowance: self.allowance(&intermediate.from, &intermediate.operator)?, // allowance remains unchanged?
            recipient_data: intermediate.recipient_data,
        })
    }

    /// Sets the balance of an account to a specific amount
    ///
    /// Using this library method obeys internal invariants but does not invoke the receiver
    /// hook on recipient accounts. Returns the old balance.
    pub fn set_balance(&mut self, owner: &Address, amount: &TokenAmount) -> Result<TokenAmount> {
        let amount = validate_amount_with_granularity(amount, "set_balance", self.granularity)?;

        let owner = self.runtime.resolve_or_init(owner)?;
        let old_balance = self.transaction(|state, bs| {
            // update the account's balance
            let old_balance = state.set_balance(bs, owner, amount)?;
            // update the total supply accordingly
            let supply_change = amount - old_balance.clone();
            state.supply += supply_change;
            Ok(old_balance)
        })?;

        Ok(old_balance)
    }
}

impl<'st, S, BS> Token<'st, S, BS>
where
    S: Syscalls,
    BS: Blockstore,
{
    /// Calls the receiver hook, returning the result
    pub fn call_receiver_hook(
        &mut self,
        token_receiver: &Address,
        params: FRC46TokenReceived,
    ) -> Result<()> {
        let receipt = self.runtime.send(
            token_receiver,
            RECEIVER_HOOK_METHOD_NUM,
            IpldBlock::serialize_cbor(&params)?,
            TokenAmount::zero(),
        )?;

        match receipt.exit_code {
            ExitCode::OK => Ok(()),
            abort_code => Err(ReceiverHookError::new_receiver_error(
                *token_receiver,
                abort_code,
                receipt.return_data,
            )
            .into()),
        }
    }

    /// Checks the state invariants, throwing an error if they are not met
    pub fn assert_invariants(&self) -> std::result::Result<StateSummary, Vec<StateInvariantError>> {
        let (summary, errors) = self.check_invariants();
        match errors.is_empty() {
            true => Ok(summary),
            false => Err(errors),
        }
    }

    /// Checks the state invariants, returning a state summary and list of errors
    pub fn check_invariants(&self) -> (StateSummary, Vec<StateInvariantError>) {
        self.state.check_invariants(&self.runtime, self.granularity)
    }
}

/// Validates that a token amount for burning/transfer/minting is non-negative, and an integer
/// multiple of granularity.
///
/// Returns the argument, or an error.
pub fn validate_amount_with_granularity<'a>(
    a: &'a TokenAmount,
    name: &'static str,
    granularity: u64,
) -> Result<&'a TokenAmount> {
    if a.is_negative() {
        return Err(TokenError::InvalidNegative { name, amount: a.clone() });
    }
    let (_, modulus) = a.div_rem(granularity);
    if !modulus.is_zero() {
        return Err(InvalidGranularity { name, amount: a.clone(), granularity });
    }
    Ok(a)
}

/// Validates that an allowance is non-negative. Allowances do not need to be an integer multiple of
/// granularity.
///
/// Returns the argument, or an error.
pub fn validate_allowance<'a>(a: &'a TokenAmount, name: &'static str) -> Result<&'a TokenAmount> {
    if a.is_negative() {
        return Err(TokenError::InvalidNegative { name, amount: a.clone() });
    }
    Ok(a)
}

#[cfg(test)]
mod test {
    use std::ops::Neg;

    use fvm_actor_utils::messaging::{MessagingError, RECEIVER_HOOK_METHOD_NUM};
    use fvm_actor_utils::receiver::{ReceiverHookError, UniversalReceiverParams};
    use fvm_actor_utils::syscalls::fake_syscalls::FakeSyscalls;
    use fvm_actor_utils::util::ActorRuntime;
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_ipld_encoding::RawBytes;
    use fvm_sdk::sys::ErrorNumber;
    use fvm_shared::address::{Address, BLS_PUB_LEN};
    use fvm_shared::econ::TokenAmount;
    use num_traits::Zero;

    use crate::receiver::{FRC46TokenReceived, FRC46_TOKEN_TYPE};
    use crate::token::state::StateError;
    use crate::token::state::TokenState;
    use crate::token::Token;
    use crate::token::TokenError;

    /// Returns a static secp256k1 address
    fn secp_address() -> Address {
        let key = vec![0; 65];
        Address::new_secp256k1(key.as_slice()).unwrap()
    }

    /// Returns a static BLS address
    fn bls_address() -> Address {
        let key = vec![0; BLS_PUB_LEN];
        Address::new_bls(key.as_slice()).unwrap()
    }

    // Returns a new Actor address, that is uninitializable by the FakeMessenger
    fn actor_address() -> Address {
        Address::new_actor(Default::default())
    }

    const TOKEN_ACTOR: &Address = &Address::new_id(1);
    const TREASURY: &Address = &Address::new_id(2);
    const ALICE: &Address = &Address::new_id(3);
    const BOB: &Address = &Address::new_id(4);
    const CAROL: &Address = &Address::new_id(5);

    fn new_token(
        runtime: ActorRuntime<FakeSyscalls, MemoryBlockstore>,
        state: &mut TokenState,
    ) -> Token<FakeSyscalls, MemoryBlockstore> {
        Token::wrap(runtime, 1, state)
    }

    fn assert_last_hook_call_eq(
        runtime: &ActorRuntime<FakeSyscalls, MemoryBlockstore>,
        expected: FRC46TokenReceived,
    ) {
        let last_message = runtime.syscalls.last_message.borrow().clone().unwrap();
        assert_eq!(last_message.method, RECEIVER_HOOK_METHOD_NUM);
        let last_called: UniversalReceiverParams =
            last_message.params.unwrap().deserialize().unwrap();
        assert_eq!(last_called.type_, FRC46_TOKEN_TYPE);
        let last_called: FRC46TokenReceived = last_called.payload.deserialize().unwrap();
        assert_eq!(last_called, expected);
    }

    #[test]
    fn it_wraps_a_previously_loaded_state_tree() {
        struct ActorState {
            token_state: TokenState,
        }

        // simulate the token state being a node in a larger state tree
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut actor_state = ActorState {
            token_state: Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs())
                .unwrap(),
        };
        // wrap the token state, moving it into a TokenHandle
        let mut token = new_token(helper, &mut actor_state.token_state);

        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from_atto(1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        let state = token.state();
        // gets a read-only state
        assert_eq!(state.supply, TokenAmount::from_atto(1));
        // can get a token_state here but doing so borrows the value making the mutable borrow on line 550 invalid
        assert_eq!(actor_state.token_state.supply, TokenAmount::from_atto(1));

        // therefore, after the above line 560, can no longer use the token handle to read OR mutate state
        // any single one of these lines now causes a compiler error
        // token.mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from_atto(1), Default::default(), Default::default()).unwrap();
        // token.balance_of(TREASURY).unwrap();
    }

    #[test]
    fn it_instantiates_and_persists() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        // create a new token
        let mut state = Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        // wrap the token state, moving it into a TokenHandle
        let mut token = new_token(helper, &mut state);

        // state exists but is empty
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // mint some value
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // flush token to blockstore
        let cid = token.flush().unwrap();

        // the returned cid can be used to reference the same token state
        let helper = ActorRuntime {
            blockstore: token.runtime.blockstore,
            syscalls: FakeSyscalls::default(),
        };
        let mut state =
            Token::<FakeSyscalls, MemoryBlockstore>::load_state(helper.bs(), &cid).unwrap();
        let token2 = Token::wrap(helper, 1, &mut state);
        assert_eq!(token2.total_supply(), TokenAmount::from_atto(100));
    }

    #[test]
    fn it_instantiates_with_variable_bit_width() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state_with_bit_width(helper.bs(), 2)
                .unwrap();
        state.set_balance(&helper, ALICE.id().unwrap(), &TokenAmount::from_atto(100)).unwrap();
        let state_cid = state.save(&helper).unwrap();

        let token =
            Token::<FakeSyscalls, MemoryBlockstore>::load_state(helper.bs(), &state_cid).unwrap();
        assert_eq!(
            token.get_balance(&helper, ALICE.id().unwrap()).unwrap(),
            TokenAmount::from_atto(100)
        );
    }

    #[test]
    fn it_mutates_externally_loaded_state() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut state = TokenState::new(&helper).unwrap();
        let mut token = Token::<FakeSyscalls, MemoryBlockstore>::wrap(helper, 1, &mut state);

        // mutate state via the handle
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // visible via the handle
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // the underlying state was mutated
        let helper = ActorRuntime {
            blockstore: token.runtime.blockstore,
            syscalls: FakeSyscalls::default(),
        };
        assert_eq!(state.supply, TokenAmount::from_atto(100));
        assert_eq!(
            state.get_balance(&helper, ALICE.id().unwrap()).unwrap(),
            TokenAmount::from_atto(100)
        );

        // note: its not allowed here to use the token handle anymore given that we have read from state
        // assert_eq!(token.total_supply(), TokenAmount::from_atto(100));
    }

    #[test]
    fn it_provides_atomic_transactions() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // entire transaction succeeds
        token
            .transaction(|state, _bs| {
                state.change_supply_by(&TokenAmount::from_atto(100))?;
                state.change_supply_by(&TokenAmount::from_atto(100))?;
                Ok(())
            })
            .unwrap();
        assert_eq!(token.total_supply(), TokenAmount::from_atto(200));

        // entire transaction fails
        token
            .transaction(|state, _bs| {
                state.change_supply_by(&TokenAmount::from_atto(-100))?;
                state.change_supply_by(&TokenAmount::from_atto(-100))?;
                // this makes supply negative and should revert the entire transaction
                state.change_supply_by(&TokenAmount::from_atto(-100))?;
                Ok(())
            })
            .unwrap_err();
        // total_supply should be unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(200));
    }

    #[test]
    fn it_mints() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);
        token.runtime.syscalls.actor_id = TOKEN_ACTOR.id().unwrap(); // minting relies on runtime to determine the token actor's id

        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from_atto(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        let hook_ret = hook.call(&token.runtime).unwrap();

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: TREASURY.id().unwrap(),
                amount: TokenAmount::from_atto(1_000_000),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        let result = token.mint_return(hook_ret).unwrap();
        assert_eq!(TokenAmount::from_atto(1_000_000), result.balance);
        assert_eq!(TokenAmount::from_atto(1_000_000), result.supply);

        // balance and total supply both went up
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(1_000_000));

        // cannot mint a negative amount
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(-1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from_atto(1_000_000));

        // mint zero
        let mut hook = token
            .mint(TOKEN_ACTOR, ALICE, &TokenAmount::zero(), Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::zero(),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from_atto(1_000_000));

        // mint again to same address
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from_atto(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        let hook_ret = hook.call(&token.runtime).unwrap();
        let result = token.mint_return(hook_ret).unwrap();
        assert_eq!(TokenAmount::from_atto(2_000_000), result.balance);
        assert_eq!(TokenAmount::from_atto(2_000_000), result.supply);

        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(2_000_000));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: TREASURY.id().unwrap(),
                amount: TokenAmount::from_atto(1_000_000),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // mint to a different address
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        let hook_ret = hook.call(&token.runtime).unwrap();
        let result = token.mint_return(hook_ret).unwrap();
        assert_eq!(TokenAmount::from_atto(1_000_000), result.balance);
        assert_eq!(TokenAmount::from_atto(3_000_000), result.supply);

        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(3_000_000));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::from_atto(1_000_000),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // carols account was unaffected
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // can mint to secp address
        let secp_address = secp_address();
        // initially zero
        assert_eq!(token.balance_of(&secp_address).unwrap(), TokenAmount::zero());
        // self-mint to secp address
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                &secp_address,
                &TokenAmount::from_atto(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: token.runtime.resolve_id(&secp_address).unwrap(),
                amount: TokenAmount::from_atto(1_000_000),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // can mint to bls address
        let bls_address = bls_address();
        // initially zero
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::zero());
        // minting creates the account
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                &bls_address,
                &TokenAmount::from_atto(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(2_000_000));
        assert_eq!(token.balance_of(&secp_address).unwrap(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(5_000_000));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: token.runtime.resolve_id(&bls_address).unwrap(),
                amount: TokenAmount::from_atto(1_000_000),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // mint fails if actor address cannot be initialised
        let actor_address: Address = actor_address();
        token
            .mint(
                TOKEN_ACTOR,
                &actor_address,
                &TokenAmount::from_atto(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(2_000_000));
        assert_eq!(token.balance_of(&secp_address).unwrap(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(5_000_000));

        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_mint_if_receiver_hook_aborts() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // force hook to abort
        token.runtime.syscalls.abort_next_send.replace(true);
        let original_state = token.state().clone();
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from_atto(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        let err = hook.call(&token.runtime).unwrap_err();

        // messaging error as we told to abort
        if let ReceiverHookError::Messaging(MessagingError::Syscall(e)) = err {
            assert_eq!(e, ErrorNumber::AssertionFailed);
        } else {
            panic!("expected receiver hook error {err:?}");
        }

        // restore original pre-mint state
        // in actor code, we'd just abort and let the VM handle this
        token.replace(original_state);

        // state remained unchanged
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::zero());
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_burns() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        let mint_amount = TokenAmount::from_atto(1_000_000);
        let burn_amount = TokenAmount::from_atto(600_000);
        let mut hook = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        token.burn(TREASURY, &burn_amount).unwrap();

        // total supply decreased
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from_atto(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // cannot burn a negative amount
        token.burn(TREASURY, &TokenAmount::from_atto(-1)).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(400_000));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // burn zero
        token.burn(TREASURY, &TokenAmount::zero()).unwrap();

        // balances and supply were unchanged
        let remaining_balance = token.balance_of(TREASURY).unwrap();
        assert_eq!(remaining_balance, TokenAmount::from_atto(400_000));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // burn exact amount left
        token.burn(TREASURY, &remaining_balance).unwrap();
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::zero());
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_burn_below_zero() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        let mint_amount = TokenAmount::from_atto(1_000_000);
        let burn_amount = TokenAmount::from_atto(2_000_000);
        let mut hook = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        token.burn(TREASURY, &burn_amount).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(1_000_000));
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_sets_balances() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // check that it obeys granularity
        token.granularity = 50;
        token.set_balance(ALICE, &TokenAmount::from_atto(49)).unwrap_err();
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // set balance for Alice to 100
        let old_balance = token.set_balance(ALICE, &TokenAmount::from_atto(100)).unwrap();
        assert_eq!(old_balance, TokenAmount::zero());
        let new_balance = token.balance_of(ALICE).unwrap();
        assert_eq!(new_balance, TokenAmount::from_atto(100));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // set balance for Alice to 50
        let old_balance = token.set_balance(ALICE, &TokenAmount::from_atto(50)).unwrap();
        assert_eq!(old_balance, TokenAmount::from_atto(100));
        let new_balance = token.balance_of(ALICE).unwrap();
        assert_eq!(new_balance, TokenAmount::from_atto(50));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(50));

        // attempt to set balance for Alice to negative
        token.set_balance(ALICE, &TokenAmount::from_atto(-50)).unwrap_err();
        // see that balance was not changed
        let new_balance = token.balance_of(ALICE).unwrap();
        assert_eq!(new_balance, TokenAmount::from_atto(50));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(50));

        // set balance for Alice to 0
        let old_balance = token.set_balance(ALICE, &TokenAmount::from_atto(0)).unwrap();
        assert_eq!(old_balance, TokenAmount::from_atto(50));
        let new_balance = token.balance_of(ALICE).unwrap();
        assert_eq!(new_balance, TokenAmount::from_atto(0));
        assert_eq!(token.total_supply(), TokenAmount::from_atto(0));

        // check that the balance map was emptied
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_transfers() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 100 for owner
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        // transfer 60 from owner -> receiver
        let mut hook = token
            .transfer(
                ALICE,
                BOB,
                &TokenAmount::from_atto(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        let intermediate = hook.call(&token.runtime).unwrap();
        let ret = token.transfer_return(intermediate).unwrap();

        // owner has 100 - 60 = 40
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(40));
        assert_eq!(ret.from_balance, TokenAmount::from_atto(40));
        // receiver has 0 + 60 = 60
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from_atto(60));
        assert_eq!(ret.to_balance, TokenAmount::from_atto(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                amount: TokenAmount::from_atto(60),
                to: BOB.id().unwrap(),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // cannot transfer a negative value
        token
            .transfer(
                ALICE,
                BOB,
                &TokenAmount::from_atto(-1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from_atto(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // transfer zero value
        let mut hook = token
            .transfer(ALICE, BOB, &TokenAmount::zero(), RawBytes::default(), RawBytes::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from_atto(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: BOB.id().unwrap(),
                amount: TokenAmount::zero(),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );
    }

    #[test]
    fn it_transfers_to_self() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 100 for owner
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        // transfer zero to self
        let mut hook = token
            .transfer(ALICE, ALICE, &TokenAmount::zero(), RawBytes::default(), RawBytes::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::zero(),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // transfer value to self
        let mut hook = token
            .transfer(
                ALICE,
                ALICE,
                &TokenAmount::from_atto(10),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::from_atto(10),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );
    }

    #[test]
    fn it_transfers_to_uninitialized_addresses() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // transfer to an uninitialized pubkey
        let secp_address = &secp_address();
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        let mut hook = token
            .transfer(
                ALICE,
                secp_address,
                &TokenAmount::from_atto(10),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // balances changed
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(90));
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::from_atto(10));

        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: token.runtime.resolve_id(secp_address).unwrap(),
                amount: TokenAmount::from_atto(10),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );
    }

    #[test]
    fn it_transfers_from_uninitialized_addresses() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        let secp_address = &secp_address();
        // non-zero transfer should fail
        assert!(token
            .transfer(
                secp_address,
                ALICE,
                &TokenAmount::from_atto(1),
                Default::default(),
                Default::default()
            )
            .is_err());
        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // zero-transfer should succeed
        let mut hook = token
            .transfer(
                secp_address,
                ALICE,
                &TokenAmount::zero(),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // secp_address was initialized
        assert!(&token.runtime.resolve_id(secp_address).is_ok());

        let actor_address = &actor_address();
        // transfers from actors fail with uninitializable
        let err = token
            .transfer(
                actor_address,
                ALICE,
                &TokenAmount::zero(),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        if let TokenError::Messaging(MessagingError::AddressNotInitialized(e)) = err {
            assert_eq!(e, *actor_address);
        } else {
            panic!("Expected AddressNotInitialized error {err:?}");
        }

        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // actor address was not initialized
        assert!(&token.runtime.resolve_id(actor_address).is_err());
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_transfer_when_receiver_hook_aborts() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 100 for owner
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // transfer 60 from owner -> receiver, but simulate receiver aborting the hook
        let _ = token.runtime.syscalls.abort_next_send.replace(true);
        let pre_transfer_state = token.state().clone();
        let mut hook = token
            .transfer(
                ALICE,
                BOB,
                &TokenAmount::from_atto(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap_err();

        // restore original pre-mint state
        // in actor code, we'd just abort and let the VM handle this
        token.replace(pre_transfer_state);

        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from_atto(0));

        // transfer 60 from owner -> self, simulate receiver aborting the hook
        token.runtime.syscalls.abort_next_send.replace(true);
        let pre_transfer_state = token.state().clone();
        let mut hook = token
            .transfer(
                ALICE,
                ALICE,
                &TokenAmount::from_atto(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap_err();

        // restore original pre-mint state
        // in actor code, we'd just abort and let the VM handle this
        token.replace(pre_transfer_state);

        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from_atto(0));
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_balance() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 50 for the owner
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(50),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // attempt transfer 51 from owner -> receiver
        token
            .transfer(
                ALICE,
                BOB,
                &TokenAmount::from_atto(51),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // balances remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(50));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_tracks_allowances() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // set allowance between Alice and Carol as 100
        let new_allowance =
            token.increase_allowance(ALICE, CAROL, &TokenAmount::from_atto(100)).unwrap();
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        // return value and allowance should be the same
        assert_eq!(new_allowance, allowance);
        assert_eq!(allowance, TokenAmount::from_atto(100));

        // one-way only
        assert_eq!(token.allowance(CAROL, ALICE).unwrap(), TokenAmount::zero());
        // unrelated allowance unaffected
        assert_eq!(token.allowance(ALICE, BOB).unwrap(), TokenAmount::zero());

        // cannot set negative deltas
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from_atto(-1)).unwrap_err();
        token.decrease_allowance(ALICE, CAROL, &TokenAmount::from_atto(-1)).unwrap_err();

        // allowance was unchanged
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(allowance, TokenAmount::from_atto(100));

        // keeps track of decreasing allowances
        let new_allowance =
            token.decrease_allowance(ALICE, CAROL, &TokenAmount::from_atto(60)).unwrap();
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(new_allowance, allowance);
        assert_eq!(allowance, TokenAmount::from_atto(40));

        // allowance revoking sets to 0
        token.revoke_allowance(ALICE, CAROL).unwrap();
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::zero());

        // allowances cannot be negative, but decreasing an allowance below 0 revokes the allowance
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from_atto(10)).unwrap();
        let new_allowance =
            token.decrease_allowance(ALICE, CAROL, &TokenAmount::from_atto(20)).unwrap();
        assert_eq!(new_allowance, TokenAmount::zero());
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::zero());

        // allowances can be set for a pubkey address
        let resolvable_address = &secp_address();
        assert_eq!(token.allowance(ALICE, resolvable_address).unwrap(), TokenAmount::zero());
        token.increase_allowance(ALICE, resolvable_address, &TokenAmount::from_atto(10)).unwrap();
        assert_eq!(token.allowance(ALICE, resolvable_address).unwrap(), TokenAmount::from_atto(10));

        let initializable_address = &bls_address();
        assert_eq!(token.allowance(ALICE, initializable_address).unwrap(), TokenAmount::zero());
        token
            .increase_allowance(ALICE, initializable_address, &TokenAmount::from_atto(10))
            .unwrap();
        assert_eq!(
            token.allowance(ALICE, initializable_address).unwrap(),
            TokenAmount::from_atto(10)
        );

        let uninitializable_address = &actor_address();
        assert_eq!(token.allowance(ALICE, uninitializable_address).unwrap(), TokenAmount::zero());
        token
            .increase_allowance(ALICE, uninitializable_address, &TokenAmount::from_atto(10))
            .unwrap_err();
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_sets_allowances() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // set allowance between Alice and Carol as 100
        token.set_allowance(ALICE, CAROL, &TokenAmount::from_atto(100)).unwrap();
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(allowance, TokenAmount::from_atto(100));

        // set allowance between Alice and Carol as 120
        token.set_allowance(ALICE, CAROL, &TokenAmount::from_atto(120)).unwrap();
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(allowance, TokenAmount::from_atto(120));

        // set allowance between Alice and Carol as 0
        token.set_allowance(ALICE, CAROL, &TokenAmount::from_atto(0)).unwrap();
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(allowance, TokenAmount::from_atto(0));

        // attempt to set allowance between Alice and Carol as -50 which should error
        token.set_allowance(ALICE, CAROL, &TokenAmount::from_atto(-50)).unwrap_err();

        // check invariants (i.e. that the allowance map is emptied after being set to 0)
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_allows_delegated_transfer() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 100 for the owner
        let mut hook = token
            .mint(
                ALICE,
                ALICE,
                &TokenAmount::from_atto(100),
                Default::default(),
                Default::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // operator can't transfer without allowance, even if amount is zero
        token
            .transfer_from(
                CAROL,
                ALICE,
                ALICE,
                &TokenAmount::zero(),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // approve 100 spending allowance for operator
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from_atto(100)).unwrap();
        // operator makes transfer of 60 from owner -> receiver
        let mut hook = token
            .transfer_from(
                CAROL,
                ALICE,
                BOB,
                &TokenAmount::from_atto(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        let intermediate = hook.call(&token.runtime).unwrap();
        let ret = token.transfer_from_return(intermediate).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(40));
        assert_eq!(ret.from_balance, TokenAmount::from_atto(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from_atto(60));
        assert_eq!(ret.to_balance, TokenAmount::from_atto(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());
        // verify remaining allowance
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::from_atto(40));
        assert_eq!(ret.allowance, TokenAmount::from_atto(40));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: CAROL.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: BOB.id().unwrap(),
                amount: TokenAmount::from_atto(60),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // verify allowance is correct
        let operator_allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(operator_allowance, TokenAmount::from_atto(40));

        // operator makes another transfer of 40 from owner -> self
        let mut hook = token
            .transfer_from(
                CAROL,
                ALICE,
                CAROL,
                &TokenAmount::from_atto(40),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from_atto(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::from_atto(40));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.runtime,
            FRC46TokenReceived {
                operator: CAROL.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: CAROL.id().unwrap(),
                amount: TokenAmount::from_atto(40),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // verify allowance is correct
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_allows_delegated_transfer_by_resolvable_pubkey() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);
        // mint 100 for owner
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        let initialised_address = &secp_address();
        let _ = token.runtime.initialize_account(initialised_address).unwrap();

        // an initialised pubkey cannot transfer zero out of Alice balance without an allowance
        token
            .transfer_from(
                initialised_address,
                ALICE,
                initialised_address,
                &TokenAmount::zero(),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // balances remained same
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::zero());
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // initialised pubkey can has zero-allowance, so cannot transfer non-zero amount
        token
            .transfer_from(
                initialised_address,
                ALICE,
                initialised_address,
                &TokenAmount::from_atto(1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        // balances remained same
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::zero());
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));

        // the pubkey can be given an allowance which it can use to transfer tokens
        token.increase_allowance(ALICE, initialised_address, &TokenAmount::from_atto(100)).unwrap();
        let mut hook = token
            .transfer_from(
                initialised_address,
                ALICE,
                initialised_address,
                &TokenAmount::from_atto(1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // balances and allowance changed
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(99));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::from_atto(1));
        assert_eq!(
            token.allowance(ALICE, initialised_address).unwrap(),
            TokenAmount::from_atto(99)
        );
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));
    }

    #[test]
    fn it_disallows_delgated_transfer_by_uninitialised_pubkey() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 100 for owner
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // non-zero transfer by an uninitialized pubkey
        let secp_address = &secp_address();
        let err = token
            .transfer_from(
                secp_address,
                ALICE,
                ALICE,
                &TokenAmount::from_atto(10),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // returns the implied insufficient allowance error
        match err {
            TokenError::TokenState(StateError::InsufficientAllowance {
                owner,
                operator,
                allowance,
                delta,
            }) => {
                assert_eq!(owner, *ALICE);
                assert_eq!(operator, *secp_address);
                assert_eq!(allowance, TokenAmount::zero());
                assert_eq!(delta, TokenAmount::from_atto(10));
            }
            e => panic!("Unexpected error {e:?}"),
        }
        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));
        // account wasn't created
        assert!(&token.runtime.resolve_id(secp_address).is_err());

        // zero transfer by an uninitialized pubkey
        let err = token
            .transfer_from(
                secp_address,
                ALICE,
                ALICE,
                &TokenAmount::zero(),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // returns the implied insufficient allowance error even for zero transfers
        match err {
            TokenError::TokenState(StateError::InsufficientAllowance {
                owner,
                operator,
                allowance,
                delta,
            }) => {
                assert_eq!(owner, *ALICE);
                assert_eq!(operator, *secp_address);
                assert_eq!(allowance, TokenAmount::zero());
                assert_eq!(delta, TokenAmount::zero());
            }
            e => panic!("Unexpected error {e:?}"),
        }
        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(100));
        // account wasn't created
        assert!(&token.runtime.resolve_id(secp_address).is_err());
    }

    #[test]
    fn it_allows_delegated_burns() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        let mint_amount = TokenAmount::from_atto(1_000_000);
        let approval_amount = TokenAmount::from_atto(600_000);
        let burn_amount = TokenAmount::from_atto(600_000);

        // mint the total amount
        let mut hook = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // approve the burner to spend the allowance
        token.increase_allowance(TREASURY, ALICE, &approval_amount).unwrap();
        // burn the approved amount
        token.burn_from(ALICE, TREASURY, &burn_amount).unwrap();

        // total supply decreased
        assert_eq!(token.total_supply(), TokenAmount::from_atto(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(400_000));
        // burner approval decreased
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());

        // disallows another delegated burn as approval is now zero
        // burn the approved amount
        token.burn_from(ALICE, TREASURY, &burn_amount).unwrap_err();

        // balances didn't change
        assert_eq!(token.total_supply(), TokenAmount::from_atto(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(400_000));
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());

        // cannot burn again due to insufficient balance
        let err = token.burn_from(ALICE, TREASURY, &burn_amount).unwrap_err();

        // gets an allowance error
        match err {
            TokenError::TokenState(StateError::InsufficientAllowance {
                owner,
                operator,
                allowance,
                delta,
            }) => {
                assert_eq!(owner, *TREASURY);
                assert_eq!(operator, *ALICE);
                assert_eq!(allowance, TokenAmount::zero());
                assert_eq!(delta, burn_amount);
            }
            e => panic!("unexpected error {e:?}"),
        };

        // balances didn't change
        assert_eq!(token.total_supply(), TokenAmount::from_atto(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(400_000));
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_allows_delegated_burns_by_resolvable_pubkeys() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        let mint_amount = TokenAmount::from_atto(1_000_000);
        let approval_amount = TokenAmount::from_atto(600_000);
        let burn_amount = TokenAmount::from_atto(600_000);

        // create a resolvable pubkey
        let secp_address = &secp_address();
        let secp_id = &token.runtime.initialize_account(secp_address).unwrap();

        // mint the total amount
        let mut hook = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // approve the burner to spend the allowance
        token.increase_allowance(TREASURY, secp_address, &approval_amount).unwrap();
        // burn the approved amount
        token.burn_from(secp_address, TREASURY, &burn_amount).unwrap();

        // total supply decreased
        assert_eq!(token.total_supply(), TokenAmount::from_atto(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(400_000));
        // burner approval decreased
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());

        // cannot burn non-zero again
        let err = token.burn_from(secp_address, TREASURY, &burn_amount).unwrap_err();
        // gets an allowance error
        match err {
            TokenError::TokenState(StateError::InsufficientAllowance {
                owner,
                operator,
                allowance,
                delta,
            }) => {
                assert_eq!(owner, *TREASURY);
                assert_eq!(operator, Address::new_id(*secp_id));
                assert_eq!(allowance, TokenAmount::zero());
                assert_eq!(delta, burn_amount);
            }
            e => panic!("unexpected error {e:?}"),
        };
        // balances unchanged
        assert_eq!(token.total_supply(), TokenAmount::from_atto(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(400_000));
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());

        // cannot burn zero now that allowance is zero
        let res = token.burn_from(secp_address, TREASURY, &TokenAmount::zero());

        // balances unchanged
        assert!(res.is_err());
        assert_eq!(token.total_supply(), TokenAmount::from_atto(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from_atto(400_000));
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_disallows_delegated_burns_by_uninitialised_pubkeys() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        let mint_amount = TokenAmount::from_atto(1_000_000);
        let burn_amount = TokenAmount::from_atto(600_000);

        // create a resolvable pubkey
        let secp_address = &secp_address();

        // mint the total amount
        let mut hook = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // cannot burn non-zero
        let err = token.burn_from(secp_address, TREASURY, &burn_amount).unwrap_err();
        // gets an allowance error
        match err {
            TokenError::TokenState(StateError::InsufficientAllowance {
                owner,
                operator,
                allowance,
                delta,
            }) => {
                assert_eq!(owner, *TREASURY);
                assert_eq!(operator, *secp_address);
                assert_eq!(allowance, TokenAmount::zero());
                assert_eq!(delta, burn_amount);
            }
            e => panic!("unexpected error {e:?}"),
        };
        // balances unchanged
        assert_eq!(token.total_supply(), mint_amount);
        assert_eq!(token.balance_of(TREASURY).unwrap(), mint_amount);
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());

        // also cannot burn zero
        let err = token.burn_from(secp_address, TREASURY, &TokenAmount::zero()).unwrap_err();
        // gets an allowance error
        match err {
            TokenError::TokenState(StateError::InsufficientAllowance {
                owner,
                operator,
                allowance,
                delta,
            }) => {
                assert_eq!(owner, *TREASURY);
                assert_eq!(operator, *secp_address);
                assert_eq!(allowance, TokenAmount::zero());
                assert_eq!(delta, TokenAmount::zero());
            }
            e => panic!("unexpected error {e:?}"),
        };
        // balances unchanged
        assert_eq!(token.total_supply(), mint_amount);
        assert_eq!(token.balance_of(TREASURY).unwrap(), mint_amount);
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());

        // account was not initialised
        assert!(&token.runtime.resolve_id(secp_address).is_err());
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_allowance() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 100 for the owner
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // approve only 40 spending allowance for operator
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from_atto(40)).unwrap();
        // operator attempts makes transfer of 60 from owner -> receiver
        // this is within the owner's balance but not within the operator's allowance
        token
            .transfer_from(
                CAROL,
                ALICE,
                BOB,
                &TokenAmount::from_atto(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // verify allowance was not spent
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::from_atto(40));
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_doesnt_use_allowance_when_insufficent_balance() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 50 for the owner
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(50),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // allow 100 to be spent by operator
        token.increase_allowance(ALICE, BOB, &TokenAmount::from_atto(100)).unwrap();

        // operator attempts transfer 51 from owner -> operator
        // they have enough allowance, but not enough balance
        token
            .transfer_from(
                BOB,
                ALICE,
                BOB,
                &TokenAmount::from_atto(51),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // attempt burn 51 by operator
        token.burn_from(BOB, ALICE, &TokenAmount::from_atto(51)).unwrap_err();

        // balances remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from_atto(50));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        assert_eq!(token.allowance(ALICE, BOB).unwrap(), TokenAmount::from_atto(100));
        token.assert_invariants().unwrap();
    }

    #[test]
    fn it_enforces_granularity() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        // construct token with 100 granularity
        let mut token =
            Token::<FakeSyscalls, MemoryBlockstore>::wrap(helper, 100, &mut token_state);

        assert_eq!(token.granularity(), 100);

        // Minting
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(1),
                Default::default(),
                Default::default(),
            )
            .expect_err("minted below granularity");
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(10),
                RawBytes::default(),
                RawBytes::default(),
            )
            .expect_err("minted below granularity");
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(99),
                RawBytes::default(),
                RawBytes::default(),
            )
            .expect_err("minted below granularity");
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(101),
                RawBytes::default(),
                RawBytes::default(),
            )
            .expect_err("minted below granularity");
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(0),
                Default::default(),
                Default::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(200),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        let mut hook = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from_atto(1000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // Burn
        token.burn(ALICE, &TokenAmount::from_atto(1)).expect_err("burned below granularity");
        token.burn(ALICE, &TokenAmount::from_atto(0)).unwrap();
        token.burn(ALICE, &TokenAmount::from_atto(100)).unwrap();

        // Transfer
        token
            .transfer(
                ALICE,
                BOB,
                &TokenAmount::from_atto(1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .expect_err("transfer delta below granularity");
        let mut hook = token
            .transfer(
                ALICE,
                BOB,
                &TokenAmount::from_atto(0),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
        let mut hook = token
            .transfer(
                ALICE,
                BOB,
                &TokenAmount::from_atto(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();
    }

    #[test]
    fn it_doesnt_initialize_accounts_when_default_values_can_be_returned() {
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let token = new_token(helper, &mut token_state);

        let secp = &secp_address();
        let bls = &bls_address();

        // allowances are all zero
        let allowance = token.allowance(secp, bls).unwrap();
        assert_eq!(allowance, TokenAmount::zero());
        let allowance = token.allowance(bls, secp).unwrap();
        assert_eq!(allowance, TokenAmount::zero());
        let allowance = token.allowance(ALICE, bls).unwrap();
        assert_eq!(allowance, TokenAmount::zero());
        let allowance = token.allowance(bls, ALICE).unwrap();
        assert_eq!(allowance, TokenAmount::zero());

        // accounts were not initialized
        let err = &token.runtime.resolve_id(bls).unwrap_err();
        if let MessagingError::AddressNotResolved(e) = err {
            assert_eq!(e, bls);
        } else {
            panic!("expected AddressNotResolved error");
        }
        let err = &token.runtime.resolve_id(secp).unwrap_err();
        if let MessagingError::AddressNotResolved(e) = err {
            assert_eq!(e, secp);
        } else {
            panic!("expected AddressNotResolved error");
        }

        // balances are zero
        let balance = token.balance_of(secp).unwrap();
        assert_eq!(balance, TokenAmount::zero());
        let balance = token.balance_of(bls).unwrap();
        assert_eq!(balance, TokenAmount::zero());

        // accounts were not initialized
        let err = &token.runtime.resolve_id(bls).unwrap_err();
        if let MessagingError::AddressNotResolved(e) = err {
            assert_eq!(e, bls);
        } else {
            panic!("expected AddressNotResolved error");
        }
        let err = &token.runtime.resolve_id(secp).unwrap_err();
        if let MessagingError::AddressNotResolved(e) = err {
            assert_eq!(e, secp);
        } else {
            panic!("expected AddressNotResolved error");
        }
    }

    #[test]
    fn test_account_combinations() {
        fn setup_accounts<'st>(
            operator: &Address,
            from: &Address,
            allowance: &TokenAmount,
            balance: &TokenAmount,
            runtime: ActorRuntime<FakeSyscalls, MemoryBlockstore>,
            state: &'st mut TokenState,
        ) -> Token<'st, FakeSyscalls, MemoryBlockstore> {
            // fresh token state
            let mut token = new_token(runtime, state);
            // set allowance if not zero (avoiding unecessary account instantiation)
            if !allowance.is_zero() && from != operator {
                token.increase_allowance(from, operator, allowance).unwrap();
            }
            // set balance if not zero (avoiding unecessary account insantiation)
            if !balance.is_zero() {
                let mut hook = token
                    .mint(from, from, balance, Default::default(), Default::default())
                    .unwrap();
                token.flush().unwrap();
                hook.call(&token.runtime).unwrap();
            }
            token
        }

        fn assert_behaviour(
            operator: &Address,
            from: &Address,
            allowance: u32,
            balance: u32,
            transfer: u32,
            behaviour: &str,
        ) {
            let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
            let mut token_state = TokenState::new(&helper).unwrap();
            let mut token = setup_accounts(
                operator,
                from,
                &TokenAmount::from_atto(allowance),
                &TokenAmount::from_atto(balance),
                helper,
                &mut token_state,
            );

            let assert_error = |err: TokenError, token: Token<FakeSyscalls, MemoryBlockstore>| {
                match behaviour {
                    "ALLOWANCE_ERR" => {
                        if let TokenError::TokenState(StateError::InsufficientAllowance {
                            // can't match addresses as may be pubkey or ID (though they would resolve to the same)
                            owner: _,
                            operator: _,
                            allowance: a,
                            delta,
                        }) = err
                        {
                            assert_eq!(a, TokenAmount::from_atto(allowance));
                            assert_eq!(delta, TokenAmount::from_atto(transfer));
                        } else {
                            panic!("unexpected error {err:?}");
                        }
                    }
                    "BALANCE_ERR" => {
                        if let TokenError::TokenState(StateError::InsufficientBalance {
                            owner,
                            balance: b,
                            delta,
                        }) = err
                        {
                            assert_eq!(owner, token.runtime.resolve_id(from).unwrap());
                            assert_eq!(delta, TokenAmount::from_atto(transfer).neg());
                            assert_eq!(b, TokenAmount::from_atto(balance));
                        } else {
                            panic!("unexpected error {err:?}");
                        }
                    }
                    "ADDRESS_ERR" => {
                        if let TokenError::Messaging(MessagingError::AddressNotInitialized(addr)) =
                            err
                        {
                            assert!((addr == *operator) || (addr == *from));
                        } else {
                            panic!("unexpected error {err:?}");
                        }
                    }
                    _ => panic!("test case not implemented"),
                }
            };

            if token.runtime.same_address(operator, from) {
                let res = token.transfer(
                    from,
                    operator,
                    &TokenAmount::from_atto(transfer),
                    RawBytes::default(),
                    RawBytes::default(),
                );

                if behaviour != "OK" {
                    assert_error(res.unwrap_err(), token);
                } else {
                    let mut hook = res.expect("expect transfer to succeed");
                    hook.call(&token.runtime).expect("receiver hook should succeed");
                }
            } else {
                let res = token.transfer_from(
                    operator,
                    from,
                    operator,
                    &TokenAmount::from_atto(transfer),
                    RawBytes::default(),
                    RawBytes::default(),
                );

                if behaviour != "OK" {
                    assert_error(res.unwrap_err(), token);
                } else {
                    let mut hook = res.expect("expect transfer to succeed");
                    hook.call(&token.runtime).expect("receiver hook should succeed");
                }
            }
        }

        // distinct resolvable address operates on resolvable address
        assert_behaviour(ALICE, BOB, 0, 0, 0, "ALLOWANCE_ERR");
        assert_behaviour(ALICE, BOB, 0, 0, 1, "ALLOWANCE_ERR");
        assert_behaviour(ALICE, BOB, 0, 1, 0, "ALLOWANCE_ERR");
        assert_behaviour(ALICE, BOB, 0, 1, 1, "ALLOWANCE_ERR");
        assert_behaviour(ALICE, BOB, 1, 0, 0, "OK");
        assert_behaviour(ALICE, BOB, 1, 0, 1, "BALANCE_ERR");
        assert_behaviour(ALICE, BOB, 1, 1, 0, "OK");
        assert_behaviour(ALICE, BOB, 1, 1, 1, "OK");

        // initialisable (but uninitialised) address operates on resolved address
        assert_behaviour(&secp_address(), BOB, 0, 0, 0, "ALLOWANCE_ERR");
        assert_behaviour(&secp_address(), BOB, 0, 0, 1, "ALLOWANCE_ERR");
        assert_behaviour(&secp_address(), BOB, 0, 1, 0, "ALLOWANCE_ERR");
        assert_behaviour(&secp_address(), BOB, 0, 1, 1, "ALLOWANCE_ERR");
        // impossible to have non-zero allowance specified for uninitialised address

        // resolvable address operates on initialisable address
        assert_behaviour(BOB, &secp_address(), 0, 0, 0, "ALLOWANCE_ERR");
        assert_behaviour(BOB, &secp_address(), 0, 0, 1, "ALLOWANCE_ERR");
        // impossible to have uninitialised address have a balance
        // impossible to have non-zero allowance specified by an uninitialised address

        // distinct uninitialised address operates on uninitialised address
        assert_behaviour(&bls_address(), &secp_address(), 0, 0, 0, "ALLOWANCE_ERR");
        assert_behaviour(&bls_address(), &secp_address(), 0, 0, 1, "ALLOWANCE_ERR");
        // impossible to have uninitialised address have a balance
        // impossible to have non-zero allowance specified by an uninitialised address

        // distinct actor address operates on actor address
        assert_behaviour(&Address::new_actor(&[1]), &actor_address(), 0, 0, 0, "ALLOWANCE_ERR");
        assert_behaviour(&Address::new_actor(&[1]), &actor_address(), 0, 0, 1, "ALLOWANCE_ERR");
        // impossible for actor to have balance (for now)
        // impossible for actor to have allowance (for now)

        // actor addresses are currently never initialisable, but may be in the future, so it returns allowance errors to mirror pubkey behaviour
        // id address operates on actor address
        assert_behaviour(ALICE, &actor_address(), 0, 0, 0, "ALLOWANCE_ERR");
        assert_behaviour(ALICE, &actor_address(), 0, 0, 1, "ALLOWANCE_ERR");
        // impossible for actor to have balance (for now)
        // impossible for actor to have allowance (for now)
        // currently the same actor will fail to transfer to itself, here it fails with an address error as for self transfers, we
        // attempt to resolve the "from" address but currently that's not possible
        assert_behaviour(&actor_address(), &actor_address(), 0, 0, 0, "ADDRESS_ERR");
        assert_behaviour(&actor_address(), &actor_address(), 0, 0, 1, "ADDRESS_ERR");

        // transfers should never fail with allowance err when the operator is the owner
        // all allowance err should be replaced with successes or balance errs
        assert_behaviour(ALICE, ALICE, 0, 0, 0, "OK");
        assert_behaviour(ALICE, ALICE, 0, 0, 1, "BALANCE_ERR");
        assert_behaviour(ALICE, ALICE, 0, 1, 0, "OK");
        assert_behaviour(ALICE, ALICE, 0, 1, 1, "OK");
        assert_behaviour(ALICE, ALICE, 1, 0, 0, "OK");
        assert_behaviour(ALICE, ALICE, 1, 0, 1, "BALANCE_ERR");
        assert_behaviour(ALICE, ALICE, 1, 1, 0, "OK");
        assert_behaviour(ALICE, ALICE, 1, 1, 1, "OK");
        // pubkey to pubkey
        assert_behaviour(&secp_address(), &secp_address(), 0, 0, 0, "OK");
        assert_behaviour(&secp_address(), &secp_address(), 0, 0, 1, "BALANCE_ERR");
        assert_behaviour(&bls_address(), &bls_address(), 0, 0, 0, "OK");
        assert_behaviour(&bls_address(), &bls_address(), 0, 0, 1, "BALANCE_ERR");
    }

    #[test]
    fn check_invariants_returns_a_state_summary() {
        //! Simulate a delgated transfer flow and then check the invariants manually
        let helper = ActorRuntime::<FakeSyscalls, MemoryBlockstore>::new_test_runtime();
        let mut token_state =
            Token::<FakeSyscalls, MemoryBlockstore>::create_state(helper.bs()).unwrap();
        let mut token = new_token(helper, &mut token_state);

        // mint 100 for the owner
        let mut hook = token
            .mint(
                ALICE,
                ALICE,
                &TokenAmount::from_atto(100),
                Default::default(),
                Default::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        // approve 100 spending allowance for operator
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from_atto(100)).unwrap();
        // operator makes transfer of 60 from owner -> receiver
        let mut hook = token
            .transfer_from(
                CAROL,
                ALICE,
                BOB,
                &TokenAmount::from_atto(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(&token.runtime).unwrap();

        let summary = token.assert_invariants().unwrap();
        // remaining balance 100 - 60
        let balance_map = summary.balance_map.unwrap();
        assert_eq!(
            balance_map.get(&ALICE.id().unwrap()).unwrap().clone(),
            TokenAmount::from_atto(40)
        );
        // received balance = 0 + 60
        assert_eq!(
            balance_map.get(&BOB.id().unwrap()).unwrap().clone(),
            TokenAmount::from_atto(60)
        );
        // remaining allowance = 100 - 60
        assert_eq!(
            summary
                .allowance_map
                .unwrap()
                .get(&ALICE.id().unwrap())
                .unwrap()
                .get(&CAROL.id().unwrap())
                .unwrap()
                .clone(),
            TokenAmount::from_atto(40)
        );
    }

    // TODO: test for re-entrancy bugs by implementing a MethodCaller that calls back on the token contract
}
