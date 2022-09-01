use std::ops::{Neg, Rem};

use cid::Cid;
pub use error::TokenError;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::ActorID;
use num_traits::Signed;
use num_traits::Zero;

use self::state::{StateError as TokenStateError, TokenState};
use self::types::BurnFromReturn;
use self::types::BurnReturn;
use self::types::TransferFromReturn;
use self::types::TransferReturn;
use crate::receiver::{types::TokensReceivedParams, ReceiverHook};
use crate::runtime::messaging::{Messaging, MessagingError};
use crate::runtime::messaging::{Result as MessagingResult, RECEIVER_HOOK_METHOD_NUM};
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
pub struct Token<'st, BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    /// Injected blockstore. The blockstore must reference the same underlying storage under Clone
    bs: BS,
    /// Minimal interface to call methods on other actors (i.e. receiver hooks)
    msg: MSG,
    /// Reference to token state that will be inspected/mutated
    state: &'st mut TokenState,
    /// Minimum granularity of token amounts.
    /// All balances and amounts must be a multiple of this granularity.
    /// Set to 1 for standard 18-dp precision, TOKEN_PRECISION for whole units only, or some
    /// value in between.
    granularity: u64,
}

impl<'st, BS, MSG> Token<'st, BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    /// Creates a new clean token state instance
    ///
    /// This should be wrapped in a Token handle for convenience. Must be flushed to the blockstore
    /// explicitly to persist changes
    pub fn create_state(bs: &BS) -> Result<TokenState> {
        Ok(TokenState::new(bs)?)
    }

    /// Wrap an existing token state
    pub fn wrap(bs: BS, msg: MSG, granularity: u64, state: &'st mut TokenState) -> Self {
        Self { bs, msg, granularity, state }
    }

    /// For an already initialised state tree, loads the state tree from the blockstore at a Cid
    pub fn load_state(bs: &BS, state_cid: &Cid) -> Result<TokenState> {
        Ok(TokenState::load(bs, state_cid)?)
    }

    /// Flush state and return Cid for root
    pub fn flush(&mut self) -> Result<Cid> {
        Ok(self.state.save(&self.bs)?)
    }

    /// Get a reference to the wrapped state tree
    pub fn state(&self) -> &TokenState {
        self.state
    }

    /// Get a reference to the Messaging struct we're using
    pub fn msg(&self) -> &MSG {
        &self.msg
    }

    /// Replace the current state reference with another
    /// This is intended for unit tests only (and enforced by the config and visibility limits)
    /// The replacement state needs the same lifetime as the original so cloning it inside
    /// actor method calls generally wouldn't work.
    #[cfg(test)]
    pub(in crate::token) fn replace(&mut self, state: &'st mut TokenState) {
        self.state = state;
    }

    /// Opens an atomic transaction on TokenState which allows a closure to make multiple
    /// modifications to the state tree.
    ///
    /// If the closure returns an error, the transaction is dropped atomically and no change is
    /// observed on token state.
    fn transaction<F, Res>(&mut self, f: F) -> Result<Res>
    where
        F: FnOnce(&mut TokenState, &BS) -> Result<Res>,
    {
        let mut mutable_state = self.state.clone();
        let res = f(&mut mutable_state, &self.bs)?;
        // if closure didn't error, save state
        *self.state = mutable_state;
        Ok(res)
    }
}

impl<'st, BS, MSG> Token<'st, BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
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
    pub fn mint(
        &mut self,
        operator: &Address,
        initial_owner: &Address,
        amount: &TokenAmount,
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<(ReceiverHook, MintReturn)> {
        let amount = validate_amount(amount, "mint", self.granularity)?;
        // init the operator account so that its actor ID can be referenced in the receiver hook
        let operator_id = self.resolve_or_init(operator)?;
        // init the owner account as allowance and balance checks are not performed for minting
        let owner_id = self.resolve_or_init(initial_owner)?;

        // Increase the balance of the actor and increase total supply
        let result = self.transaction(|state, bs| {
            let balance = state.change_balance_by(&bs, owner_id, amount)?;
            let supply = state.change_supply_by(amount)?;
            Ok(MintReturn { balance, supply: supply.clone() })
        })?;

        // return the params we'll send to the receiver hook
        let params = TokensReceivedParams {
            operator: operator_id,
            from: self.msg.actor_id(),
            to: owner_id,
            amount: amount.clone(),
            operator_data,
            token_data,
        };

        Ok((ReceiverHook::new(*initial_owner, params), result))
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
        match self.get_id(owner) {
            Ok(owner) => Ok(self.state.get_balance(&self.bs, owner)?),
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
        let owner = match self.get_id(owner) {
            Ok(owner) => owner,
            Err(MessagingError::AddressNotResolved(_)) => {
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };

        // Don't instantiate an account if unable to resolve operator-ID, as non-initialized
        // addresses have an implicit zero allowance
        let operator = match self.get_id(operator) {
            Ok(operator) => operator,
            Err(MessagingError::AddressNotResolved(_)) => {
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };

        // For concretely resolved accounts, retrieve the allowance from the map
        Ok(self.state.get_allowance_between(&self.bs, owner, operator)?)
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
        let delta = validate_amount(delta, "allowance delta", self.granularity)?;
        // Attempt to instantiate the accounts if they don't exist
        let owner = self.resolve_or_init(owner)?;
        let operator = self.resolve_or_init(operator)?;
        let new_amount = self.state.change_allowance_by(&self.bs, owner, operator, delta)?;

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
        let delta = validate_amount(delta, "allowance delta", self.granularity)?;
        // Attempt to instantiate the accounts if they don't exist
        let owner = self.resolve_or_init(owner)?;
        let operator = self.resolve_or_init(operator)?;
        let new_allowance =
            self.state.change_allowance_by(&self.bs, owner, operator, &delta.neg())?;

        Ok(new_allowance)
    }

    /// Sets the allowance between owner and operator to 0
    pub fn revoke_allowance(&mut self, owner: &Address, operator: &Address) -> Result<()> {
        let owner = match self.get_id(owner) {
            Ok(owner) => owner,
            Err(MessagingError::AddressNotResolved(_)) => {
                // uninitialized address has implicit zero allowance already
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        let operator = match self.get_id(operator) {
            Ok(operator) => operator,
            Err(MessagingError::AddressNotResolved(_)) => {
                // uninitialized address has implicit zero allowance already
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        // if both accounts resolved, explicitly set allowance to zero
        self.state.revoke_allowance(&self.bs, owner, operator)?;

        Ok(())
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
        let amount = validate_amount(amount, "burn", self.granularity)?;

        let owner = self.resolve_or_init(owner)?;
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
        let amount = validate_amount(amount, "burn", self.granularity)?;
        if self.same_address(operator, owner) {
            return Err(TokenError::InvalidOperator(*operator));
        }

        // operator must exist to have a non-zero allowance
        let operator = match self.get_id(operator) {
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
        let owner = match self.get_id(owner) {
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
    /// and the updated balances.
    /// ReceiverHook must be called or it will panic and abort the transaction.
    pub fn transfer(
        &mut self,
        from: &Address,
        to: &Address,
        amount: &TokenAmount,
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<(ReceiverHook, TransferReturn)> {
        let amount = validate_amount(amount, "transfer", self.granularity)?;

        // owner-initiated transfer
        let from = self.resolve_or_init(from)?;
        let to_id = self.resolve_or_init(to)?;
        // skip allowance check for self-managed transfers
        let res = self.transaction(|state, bs| {
            // don't change balance if to == from, but must check that the transfer doesn't exceed balance
            if to_id == from {
                let balance = state.get_balance(&bs, from)?;
                if balance.lt(amount) {
                    return Err(TokenStateError::InsufficientBalance {
                        owner: from,
                        balance,
                        delta: amount.clone().neg(),
                    }
                    .into());
                }
                Ok(TransferReturn { from_balance: balance.clone(), to_balance: balance })
            } else {
                let to_balance = state.change_balance_by(&bs, to_id, amount)?;
                let from_balance = state.change_balance_by(&bs, from, &amount.neg())?;
                Ok(TransferReturn { from_balance, to_balance })
            }
        })?;

        let params = TokensReceivedParams {
            operator: from,
            from,
            to: to_id,
            amount: amount.clone(),
            operator_data,
            token_data,
        };

        Ok((ReceiverHook::new(*to, params), res))
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
    /// and the updated allowance and balances.
    /// ReceiverHook must be called or it will panic and abort the transaction.
    pub fn transfer_from(
        &mut self,
        operator: &Address,
        from: &Address,
        to: &Address,
        amount: &TokenAmount,
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<(ReceiverHook, TransferFromReturn)> {
        let amount = validate_amount(amount, "transfer", self.granularity)?;
        if self.same_address(operator, from) {
            return Err(TokenError::InvalidOperator(*operator));
        }

        // operator-initiated transfer must have a resolvable operator
        let operator_id = match self.get_id(operator) {
            // if operator resolved, we can continue with other checks
            Ok(id) => id,
            // if we cannot resolve the operator, they are forbidden to transfer
            Err(MessagingError::AddressNotResolved(operator)) => {
                return Err(TokenError::TokenState(TokenStateError::InsufficientAllowance {
                    operator,
                    owner: *from,
                    allowance: TokenAmount::zero(),
                    delta: amount.clone(),
                }));
            }
            Err(e) => return Err(e.into()),
        };

        // the owner must exist to have specified a non-zero allowance
        let from = match self.get_id(from) {
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
        let to_id = self.resolve_or_init(to)?;

        // update token state
        let ret = self.transaction(|state, bs| {
            let remaining_allowance =
                state.attempt_use_allowance(&bs, operator_id, from, amount)?;
            // don't change balance if to == from, but must check that the transfer doesn't exceed balance
            if to_id == from {
                let balance = state.get_balance(&bs, from)?;
                if balance.lt(amount) {
                    return Err(TokenStateError::InsufficientBalance {
                        owner: from,
                        balance,
                        delta: amount.clone().neg(),
                    }
                    .into());
                }
                Ok(TransferFromReturn {
                    from_balance: balance.clone(),
                    to_balance: balance,
                    allowance: remaining_allowance,
                })
            } else {
                let to_balance = state.change_balance_by(&bs, to_id, amount)?;
                let from_balance = state.change_balance_by(&bs, from, &amount.neg())?;
                Ok(TransferFromReturn { from_balance, to_balance, allowance: remaining_allowance })
            }
        })?;

        let params = TokensReceivedParams {
            operator: operator_id,
            from,
            to: to_id,
            amount: amount.clone(),
            operator_data,
            token_data,
        };

        Ok((ReceiverHook::new(*to, params), ret))
    }
}

impl<'st, BS, MSG> Token<'st, BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    /// Resolves an address to an ID address, sending a message to initialise an account there if
    /// it doesn't exist
    ///
    /// If the account cannot be created, this function returns MessagingError::AddressNotInitialized
    fn resolve_or_init(&self, address: &Address) -> MessagingResult<ActorID> {
        let id = match self.msg.resolve_id(address) {
            Ok(addr) => addr,
            Err(MessagingError::AddressNotResolved(_e)) => self.msg.initialize_account(address)?,
            Err(e) => return Err(e),
        };
        Ok(id)
    }

    /// Attempts to resolve an address to an ActorID, returning MessagingError::AddressNotResolved
    /// if it wasn't found
    fn get_id(&self, address: &Address) -> MessagingResult<ActorID> {
        self.msg.resolve_id(address)
    }

    /// Attempts to compare two addresses, seeing if they would resolve to the same Actor without
    /// actually initiating accounts for them
    ///
    /// If a and b are of the same type, simply do an equality check. Otherwise, attempt to resolve
    /// to an ActorID and compare
    fn same_address(&self, address_a: &Address, address_b: &Address) -> bool {
        let protocol_a = address_a.protocol();
        let protocol_b = address_b.protocol();
        if protocol_a == protocol_b {
            address_a == address_b
        } else {
            // attempt to resolve both to ActorID
            let id_a = match self.get_id(address_a) {
                Ok(id) => id,
                Err(_) => return false,
            };
            let id_b = match self.get_id(address_b) {
                Ok(id) => id,
                Err(_) => return false,
            };
            id_a == id_b
        }
    }

    /// Calls the receiver hook, returning the result
    pub fn call_receiver_hook(
        &mut self,
        token_receiver: &Address,
        params: TokensReceivedParams,
    ) -> Result<()> {
        let receipt = self.msg.send(
            token_receiver,
            RECEIVER_HOOK_METHOD_NUM,
            &RawBytes::serialize(&params)?,
            &TokenAmount::zero(),
        )?;

        match receipt.exit_code {
            ExitCode::OK => Ok(()),
            abort_code => Err(TokenError::ReceiverHook {
                from: params.from,
                to: params.to,
                operator: params.operator,
                amount: params.amount,
                exit_code: abort_code,
            }),
        }
    }

    /// Checks the state invariants, throwing an error if they are not met
    pub fn check_invariants(&self) -> Result<()> {
        self.state.check_invariants(&self.bs)?;
        Ok(())
    }
}

/// Validates that a token amount is non-negative, and an integer multiple of granularity.
/// Returns the argument, or an error.
fn validate_amount<'a>(
    a: &'a TokenAmount,
    name: &'static str,
    granularity: u64,
) -> Result<&'a TokenAmount> {
    if a.is_negative() {
        return Err(TokenError::InvalidNegative { name, amount: a.clone() });
    }
    let modulus = a.rem(granularity);
    if !modulus.is_zero() {
        return Err(InvalidGranularity { name, amount: a.clone(), granularity });
    }
    Ok(a)
}

#[cfg(test)]
mod test {
    use std::ops::Neg;

    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_ipld_encoding::RawBytes;
    use fvm_shared::address::{Address, BLS_PUB_LEN};
    use fvm_shared::econ::TokenAmount;
    use num_traits::Zero;

    use crate::receiver::types::TokensReceivedParams;
    use crate::runtime::messaging::{FakeMessenger, Messaging, MessagingError};
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
        bs: MemoryBlockstore,
        state: &mut TokenState,
    ) -> Token<MemoryBlockstore, FakeMessenger> {
        Token::wrap(bs, FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6), 1, state)
    }

    fn assert_last_hook_call_eq(messenger: &FakeMessenger, expected: TokensReceivedParams) {
        let last_called = messenger.last_message.borrow().clone().unwrap();
        let last_called: TokensReceivedParams = last_called.deserialize().unwrap();
        assert_eq!(last_called, expected);
    }

    #[test]
    fn it_wraps_a_previously_loaded_state_tree() {
        struct ActorState {
            token_state: TokenState,
        }

        // simulate the token state being a node in a larger state tree
        let bs = MemoryBlockstore::default();
        let mut actor_state =
            ActorState { token_state: Token::<_, FakeMessenger>::create_state(&bs).unwrap() };

        // wrap the token state, moving it into a TokenHandle
        let mut token = Token::wrap(
            &bs,
            FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6),
            1,
            &mut actor_state.token_state,
        );

        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from(1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        let state = token.state();
        // gets a read-only state
        assert_eq!(state.supply, TokenAmount::from(1));
        // can get a token_state here but doing so borrows the value making the mutable borrow on line 550 invalid
        assert_eq!(actor_state.token_state.supply, TokenAmount::from(1));

        // therefore, after the above line 560, can no longer use the token handle to read OR mutate state
        // any single one of these lines now causes a compiler error
        // token.mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(1), Default::default(), Default::default()).unwrap();
        // token.balance_of(TREASURY).unwrap();
    }

    #[test]
    fn it_instantiates_and_persists() {
        // create a new token
        let bs = MemoryBlockstore::new();
        let mut state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token =
            Token::wrap(&bs, FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6), 1, &mut state);

        // state exists but is empty
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // mint some value
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // flush token to blockstore
        let cid = token.flush().unwrap();

        // the returned cid can be used to reference the same token state
        let mut state = Token::<_, FakeMessenger>::load_state(&bs, &cid).unwrap();
        let token2 =
            Token::wrap(&bs, FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6), 1, &mut state);
        assert_eq!(token2.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_mutates_externally_loaded_state() {
        let bs = MemoryBlockstore::new();
        let msg = FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6);
        let mut state = TokenState::new(&bs).unwrap();
        let mut token = Token::wrap(&bs, msg, 1, &mut state);

        // mutate state via the handle
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // visible via the handle
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // the underlying state was mutated
        assert_eq!(state.supply, TokenAmount::from(100));
        assert_eq!(state.get_balance(&bs, ALICE.id().unwrap()).unwrap(), TokenAmount::from(100));

        // note: its not allowed here to use the token handle anymore given that we have read from state
        // assert_eq!(token.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_provides_atomic_transactions() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // entire transaction succeeds
        token
            .transaction(|state, _bs| {
                state.change_supply_by(&TokenAmount::from(100))?;
                state.change_supply_by(&TokenAmount::from(100))?;
                Ok(())
            })
            .unwrap();
        assert_eq!(token.total_supply(), TokenAmount::from(200));

        // entire transaction fails
        token
            .transaction(|state, _bs| {
                state.change_supply_by(&TokenAmount::from(-100))?;
                state.change_supply_by(&TokenAmount::from(-100))?;
                // this makes supply negative and should revert the entire transaction
                state.change_supply_by(&TokenAmount::from(-100))?;
                Ok(())
            })
            .unwrap_err();
        // total_supply should be unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(200));
    }

    #[test]
    fn it_mints() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        let (mut hook, result) = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        assert_eq!(TokenAmount::from(1_000_000), result.balance);
        assert_eq!(TokenAmount::from(1_000_000), result.supply);

        // balance and total supply both went up
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // cannot mint a negative amount
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(-1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // mint zero
        let (mut hook, _) = token
            .mint(TOKEN_ACTOR, ALICE, &TokenAmount::zero(), Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
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
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // mint again to same address
        let (mut hook, result) = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        assert_eq!(TokenAmount::from(2_000_000), result.balance);
        assert_eq!(TokenAmount::from(2_000_000), result.supply);

        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(2_000_000));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: TREASURY.id().unwrap(),
                amount: TokenAmount::from(1_000_000),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // mint to a different address
        let (mut hook, result) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        assert_eq!(TokenAmount::from(1_000_000), result.balance);
        assert_eq!(TokenAmount::from(3_000_000), result.supply);

        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(3_000_000));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::from(1_000_000),
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
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                &secp_address,
                &TokenAmount::from(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: token.get_id(&secp_address).unwrap(),
                amount: TokenAmount::from(1_000_000),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // can mint to bls address
        let bls_address = bls_address();
        // initially zero
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::zero());
        // minting creates the account
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                &bls_address,
                &TokenAmount::from(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.balance_of(&secp_address).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(5_000_000));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: token.get_id(&bls_address).unwrap(),
                amount: TokenAmount::from(1_000_000),
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
                &TokenAmount::from(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.balance_of(&secp_address).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(5_000_000));

        token.check_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_mint_if_receiver_hook_aborts() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // force hook to abort
        token.msg.abort_next_send();
        let mut original_state = token.state().clone();
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                TREASURY,
                &TokenAmount::from(1_000_000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        let err = hook.call(token.msg()).unwrap_err();

        // check error shape
        match err {
            TokenError::ReceiverHook { from, to, operator, amount, exit_code: _exit_code } => {
                assert_eq!(from, TOKEN_ACTOR.id().unwrap());
                assert_eq!(to, TREASURY.id().unwrap());
                assert_eq!(operator, TOKEN_ACTOR.id().unwrap());
                assert_eq!(amount, TokenAmount::from(1_000_000));
                // restore original pre-mint state
                // in actor code, we'd just abort and let the VM handle this
                token.replace(&mut original_state);
            }
            _ => panic!("expected receiver hook error"),
        };

        // state remained unchanged
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::zero());
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_burns() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(600_000);
        let (mut hook, _) = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        token.burn(TREASURY, &burn_amount).unwrap();

        // total supply decreased
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // cannot burn a negative amount
        token.burn(TREASURY, &TokenAmount::from(-1)).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // burn zero
        token.burn(TREASURY, &TokenAmount::zero()).unwrap();

        // balances and supply were unchanged
        let remaining_balance = token.balance_of(TREASURY).unwrap();
        assert_eq!(remaining_balance, TokenAmount::from(400_000));
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // burn exact amount left
        token.burn(TREASURY, &remaining_balance).unwrap();
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::zero());
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_burn_below_zero() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(2_000_000);
        let (mut hook, _) = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        token.burn(TREASURY, &burn_amount).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_transfers() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 100 for owner
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        // transfer 60 from owner -> receiver
        let (mut hook, _) = token
            .transfer(ALICE, BOB, &TokenAmount::from(60), RawBytes::default(), RawBytes::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // owner has 100 - 60 = 40
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        // receiver has 0 + 60 = 60
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                amount: TokenAmount::from(60),
                to: BOB.id().unwrap(),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // cannot transfer a negative value
        token
            .transfer(ALICE, BOB, &TokenAmount::from(-1), RawBytes::default(), RawBytes::default())
            .unwrap_err();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // transfer zero value
        let (mut hook, _) = token
            .transfer(ALICE, BOB, &TokenAmount::zero(), RawBytes::default(), RawBytes::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
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
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 100 for owner
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        // transfer zero to self
        let (mut hook, _) = token
            .transfer(ALICE, ALICE, &TokenAmount::zero(), RawBytes::default(), RawBytes::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::zero(),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // transfer value to self
        let (mut hook, _) = token
            .transfer(
                ALICE,
                ALICE,
                &TokenAmount::from(10),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::from(10),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );
    }

    #[test]
    fn it_transfers_to_uninitialized_addresses() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // transfer to an uninitialized pubkey
        let secp_address = &secp_address();
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        let (mut hook, _) = token
            .transfer(
                ALICE,
                secp_address,
                &TokenAmount::from(10),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // balances changed
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(90));
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::from(10));

        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: token.get_id(secp_address).unwrap(),
                amount: TokenAmount::from(10),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );
    }

    #[test]
    fn it_transfers_from_uninitialized_addresses() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        let secp_address = &secp_address();
        // non-zero transfer should fail
        assert!(token
            .transfer(
                secp_address,
                ALICE,
                &TokenAmount::from(1),
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
        let (mut hook, _) = token
            .transfer(
                secp_address,
                ALICE,
                &TokenAmount::zero(),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // secp_address was initialized
        assert!(token.get_id(secp_address).is_ok());

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
            panic!("Expected AddressNotInitialized error");
        }

        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // actor address was not initialized
        assert!(token.get_id(actor_address).is_err());
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_transfer_when_receiver_hook_aborts() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 100 for owner
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // transfer 60 from owner -> receiver, but simulate receiver aborting the hook
        token.msg.abort_next_send();
        let mut pre_transfer_state = token.state().clone();
        let (mut hook, _) = token
            .transfer(ALICE, BOB, &TokenAmount::from(60), RawBytes::default(), RawBytes::default())
            .unwrap();
        token.flush().unwrap();
        let err = hook.call(token.msg()).unwrap_err();

        // check error shape
        match err {
            TokenError::ReceiverHook { from, to, operator, amount, exit_code: _exit_code } => {
                assert_eq!(from, ALICE.id().unwrap());
                assert_eq!(to, BOB.id().unwrap());
                assert_eq!(operator, ALICE.id().unwrap());
                assert_eq!(amount, TokenAmount::from(60));
                // revert to pre-transfer state
                // in actor code, we'd just abort and let the VM handle this
                token.replace(&mut pre_transfer_state);
            }
            _ => panic!("expected receiver hook error"),
        };

        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(0));

        // transfer 60 from owner -> self, simulate receiver aborting the hook
        token.msg.abort_next_send();
        let mut pre_transfer_state = token.state().clone();
        let (mut hook, _) = token
            .transfer(
                ALICE,
                ALICE,
                &TokenAmount::from(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        let err = hook.call(token.msg()).unwrap_err();

        // check error shape
        match err {
            TokenError::ReceiverHook { from, to, operator, amount, exit_code: _exit_code } => {
                assert_eq!(from, ALICE.id().unwrap());
                assert_eq!(to, ALICE.id().unwrap());
                assert_eq!(operator, ALICE.id().unwrap());
                assert_eq!(amount, TokenAmount::from(60));
                // revert to pre-transfer state
                // in actor code, we'd just abort and let the VM handle this
                token.replace(&mut pre_transfer_state);
            }
            _ => panic!("expected receiver hook error"),
        };

        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(0));
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_balance() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 50 for the owner
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(50),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // attempt transfer 51 from owner -> receiver
        token
            .transfer(ALICE, BOB, &TokenAmount::from(51), RawBytes::default(), RawBytes::default())
            .unwrap_err();

        // balances remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(50));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_tracks_allowances() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // set allowance between Alice and Carol as 100
        let new_allowance =
            token.increase_allowance(ALICE, CAROL, &TokenAmount::from(100)).unwrap();
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        // return value and allowance should be the same
        assert_eq!(new_allowance, allowance);
        assert_eq!(allowance, TokenAmount::from(100));

        // one-way only
        assert_eq!(token.allowance(CAROL, ALICE).unwrap(), TokenAmount::zero());
        // unrelated allowance unaffected
        assert_eq!(token.allowance(ALICE, BOB).unwrap(), TokenAmount::zero());

        // cannot set negative deltas
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(-1)).unwrap_err();
        token.decrease_allowance(ALICE, CAROL, &TokenAmount::from(-1)).unwrap_err();

        // allowance was unchanged
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(allowance, TokenAmount::from(100));

        // keeps track of decreasing allowances
        let new_allowance = token.decrease_allowance(ALICE, CAROL, &TokenAmount::from(60)).unwrap();
        let allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(new_allowance, allowance);
        assert_eq!(allowance, TokenAmount::from(40));

        // allowance revoking sets to 0
        token.revoke_allowance(ALICE, CAROL).unwrap();
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::zero());

        // allowances cannot be negative, but decreasing an allowance below 0 revokes the allowance
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(10)).unwrap();
        let new_allowance = token.decrease_allowance(ALICE, CAROL, &TokenAmount::from(20)).unwrap();
        assert_eq!(new_allowance, TokenAmount::zero());
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::zero());

        // allowances can be set for a pubkey address
        let resolvable_address = &secp_address();
        assert_eq!(token.allowance(ALICE, resolvable_address).unwrap(), TokenAmount::zero());
        token.increase_allowance(ALICE, resolvable_address, &TokenAmount::from(10)).unwrap();
        assert_eq!(token.allowance(ALICE, resolvable_address).unwrap(), TokenAmount::from(10));

        let initializable_address = &bls_address();
        assert_eq!(token.allowance(ALICE, initializable_address).unwrap(), TokenAmount::zero());
        token.increase_allowance(ALICE, initializable_address, &TokenAmount::from(10)).unwrap();
        assert_eq!(token.allowance(ALICE, initializable_address).unwrap(), TokenAmount::from(10));

        let uninitializable_address = &actor_address();
        assert_eq!(token.allowance(ALICE, uninitializable_address).unwrap(), TokenAmount::zero());
        token
            .increase_allowance(ALICE, uninitializable_address, &TokenAmount::from(10))
            .unwrap_err();
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_allows_delegated_transfer() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 100 for the owner
        let (mut hook, _) = token
            .mint(ALICE, ALICE, &TokenAmount::from(100), Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

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
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(100)).unwrap();
        // operator makes transfer of 60 from owner -> receiver
        let (mut hook, _) = token
            .transfer_from(
                CAROL,
                ALICE,
                BOB,
                &TokenAmount::from(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: CAROL.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: BOB.id().unwrap(),
                amount: TokenAmount::from(60),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // verify allowance is correct
        let operator_allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(operator_allowance, TokenAmount::from(40));

        // operator makes another transfer of 40 from owner -> self
        let (mut hook, _) = token
            .transfer_from(
                CAROL,
                ALICE,
                CAROL,
                &TokenAmount::from(40),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::from(40));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokensReceivedParams {
                operator: CAROL.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: CAROL.id().unwrap(),
                amount: TokenAmount::from(40),
                operator_data: Default::default(),
                token_data: Default::default(),
            },
        );

        // verify allowance is correct
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_allows_delegated_transfer_by_resolvable_pubkey() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 100 for owner
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        let initialised_address = &secp_address();
        token.msg.initialize_account(initialised_address).unwrap();

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
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::zero());
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // initialised pubkey can has zero-allowance, so cannot transfer non-zero amount
        token
            .transfer_from(
                initialised_address,
                ALICE,
                initialised_address,
                &TokenAmount::from(1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        // balances remained same
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::zero());
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // the pubkey can be given an allowance which it can use to transfer tokens
        token.increase_allowance(ALICE, initialised_address, &TokenAmount::from(100)).unwrap();
        let (mut hook, _) = token
            .transfer_from(
                initialised_address,
                ALICE,
                initialised_address,
                &TokenAmount::from(1),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // balances and allowance changed
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(99));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::from(1));
        assert_eq!(token.allowance(ALICE, initialised_address).unwrap(), TokenAmount::from(99));
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_disallows_delgated_transfer_by_uninitialised_pubkey() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 100 for owner
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // non-zero transfer by an uninitialized pubkey
        let secp_address = &secp_address();
        let err = token
            .transfer_from(
                secp_address,
                ALICE,
                ALICE,
                &TokenAmount::from(10),
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
                assert_eq!(delta, TokenAmount::from(10));
            }
            e => panic!("Unexpected error {:?}", e),
        }
        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));
        // account wasn't created
        assert!(token.msg.resolve_id(secp_address).is_err());

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
            e => panic!("Unexpected error {:?}", e),
        }
        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));
        // account wasn't created
        assert!(token.msg.resolve_id(secp_address).is_err());
    }

    #[test]
    fn it_allows_delegated_burns() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        let mint_amount = TokenAmount::from(1_000_000);
        let approval_amount = TokenAmount::from(600_000);
        let burn_amount = TokenAmount::from(600_000);

        // mint the total amount
        let (mut hook, _) = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // approve the burner to spend the allowance
        token.increase_allowance(TREASURY, ALICE, &approval_amount).unwrap();
        // burn the approved amount
        token.burn_from(ALICE, TREASURY, &burn_amount).unwrap();

        // total supply decreased
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        // burner approval decreased
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());

        // disallows another delegated burn as approval is now zero
        // burn the approved amount
        token.burn_from(ALICE, TREASURY, &burn_amount).unwrap_err();

        // balances didn't change
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
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
            e => panic!("unexpected error {:?}", e),
        };

        // balances didn't change
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_allows_delegated_burns_by_resolvable_pubkeys() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        let mint_amount = TokenAmount::from(1_000_000);
        let approval_amount = TokenAmount::from(600_000);
        let burn_amount = TokenAmount::from(600_000);

        // create a resolvable pubkey
        let secp_address = &secp_address();
        let secp_id = token.msg.initialize_account(secp_address).unwrap();

        // mint the total amount
        let (mut hook, _) = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // approve the burner to spend the allowance
        token.increase_allowance(TREASURY, secp_address, &approval_amount).unwrap();
        // burn the approved amount
        token.burn_from(secp_address, TREASURY, &burn_amount).unwrap();

        // total supply decreased
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
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
                assert_eq!(operator, Address::new_id(secp_id));
                assert_eq!(allowance, TokenAmount::zero());
                assert_eq!(delta, burn_amount);
            }
            e => panic!("unexpected error {:?}", e),
        };
        // balances unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());

        // cannot burn zero now that allowance is zero
        let res = token.burn_from(secp_address, TREASURY, &TokenAmount::zero());

        // balances unchanged
        assert!(res.is_err());
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_disallows_delegated_burns_by_uninitialised_pubkeys() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(600_000);

        // create a resolvable pubkey
        let secp_address = &secp_address();

        // mint the total amount
        let (mut hook, _) = token
            .mint(TOKEN_ACTOR, TREASURY, &mint_amount, Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

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
            e => panic!("unexpected error {:?}", e),
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
            e => panic!("unexpected error {:?}", e),
        };
        // balances unchanged
        assert_eq!(token.total_supply(), mint_amount);
        assert_eq!(token.balance_of(TREASURY).unwrap(), mint_amount);
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());

        // account was not initialised
        assert!(token.msg.resolve_id(secp_address).is_err());
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_allowance() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 100 for the owner
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // approve only 40 spending allowance for operator
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(40)).unwrap();
        // operator attempts makes transfer of 60 from owner -> receiver
        // this is within the owner's balance but not within the operator's allowance
        token
            .transfer_from(
                CAROL,
                ALICE,
                BOB,
                &TokenAmount::from(60),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // verify allowance was not spent
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::from(40));
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_doesnt_use_allowance_when_insufficent_balance() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let mut token = new_token(bs, &mut token_state);

        // mint 50 for the owner
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(50),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // allow 100 to be spent by operator
        token.increase_allowance(ALICE, BOB, &TokenAmount::from(100)).unwrap();

        // operator attempts transfer 51 from owner -> operator
        // they have enough allowance, but not enough balance
        token
            .transfer_from(
                BOB,
                ALICE,
                BOB,
                &TokenAmount::from(51),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();

        // attempt burn 51 by operator
        token.burn_from(BOB, ALICE, &TokenAmount::from(51)).unwrap_err();

        // balances remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(50));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        assert_eq!(token.allowance(ALICE, BOB).unwrap(), TokenAmount::from(100));
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_enforces_granularity() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();

        // construct token with 100 granularity
        let mut token = Token::wrap(
            bs,
            FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6),
            100,
            &mut token_state,
        );

        assert_eq!(token.granularity(), 100);

        // Minting
        token
            .mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(1), Default::default(), Default::default())
            .expect_err("minted below granularity");
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(10),
                RawBytes::default(),
                RawBytes::default(),
            )
            .expect_err("minted below granularity");
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(99),
                RawBytes::default(),
                RawBytes::default(),
            )
            .expect_err("minted below granularity");
        token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(101),
                RawBytes::default(),
                RawBytes::default(),
            )
            .expect_err("minted below granularity");
        let (mut hook, _) = token
            .mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(0), Default::default(), Default::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(100),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(200),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        let (mut hook, _) = token
            .mint(
                TOKEN_ACTOR,
                ALICE,
                &TokenAmount::from(1000),
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();

        // Burn
        token.burn(ALICE, &TokenAmount::from(1)).expect_err("burned below granularity");
        token.burn(ALICE, &TokenAmount::from(0)).unwrap();
        token.burn(ALICE, &TokenAmount::from(100)).unwrap();

        // Allowance
        token
            .increase_allowance(ALICE, BOB, &TokenAmount::from(1))
            .expect_err("allowance delta below granularity");
        token.increase_allowance(ALICE, BOB, &TokenAmount::from(0)).unwrap();
        token.increase_allowance(ALICE, BOB, &TokenAmount::from(100)).unwrap();

        token
            .decrease_allowance(ALICE, BOB, &TokenAmount::from(1))
            .expect_err("allowance delta below granularity");
        token.decrease_allowance(ALICE, BOB, &TokenAmount::from(0)).unwrap();
        token.decrease_allowance(ALICE, BOB, &TokenAmount::from(100)).unwrap();

        // Transfer
        token
            .transfer(ALICE, BOB, &TokenAmount::from(1), RawBytes::default(), RawBytes::default())
            .expect_err("transfer delta below granularity");
        let (mut hook, _) = token
            .transfer(ALICE, BOB, &TokenAmount::from(0), RawBytes::default(), RawBytes::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
        let (mut hook, _) = token
            .transfer(ALICE, BOB, &TokenAmount::from(100), RawBytes::default(), RawBytes::default())
            .unwrap();
        token.flush().unwrap();
        hook.call(token.msg()).unwrap();
    }

    #[test]
    fn it_doesnt_initialize_accounts_when_default_values_can_be_returned() {
        let bs = MemoryBlockstore::new();
        let mut token_state = Token::<_, FakeMessenger>::create_state(&bs).unwrap();
        let token = new_token(bs, &mut token_state);

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
        let err = token.get_id(bls).unwrap_err();
        if let MessagingError::AddressNotResolved(e) = err {
            assert_eq!(e, *bls);
        } else {
            panic!("expected AddressNotResolved error");
        }
        let err = token.get_id(secp).unwrap_err();
        if let MessagingError::AddressNotResolved(e) = err {
            assert_eq!(e, *secp);
        } else {
            panic!("expected AddressNotResolved error");
        }

        // balances are zero
        let balance = token.balance_of(secp).unwrap();
        assert_eq!(balance, TokenAmount::zero());
        let balance = token.balance_of(bls).unwrap();
        assert_eq!(balance, TokenAmount::zero());

        // accounts were not initialized
        let err = token.get_id(bls).unwrap_err();
        if let MessagingError::AddressNotResolved(e) = err {
            assert_eq!(e, *bls);
        } else {
            panic!("expected AddressNotResolved error");
        }
        let err = token.get_id(secp).unwrap_err();
        if let MessagingError::AddressNotResolved(e) = err {
            assert_eq!(e, *secp);
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
            bs: MemoryBlockstore,
            state: &'st mut TokenState,
        ) -> Token<'st, MemoryBlockstore, FakeMessenger> {
            // fresh token state
            let mut token = new_token(bs, state);
            // set allowance if not zero (avoiding unecessary account instantiation)
            if !allowance.is_zero() && !(from == operator) {
                token.increase_allowance(from, operator, allowance).unwrap();
            }
            // set balance if not zero (avoiding unecessary account insantiation)
            if !balance.is_zero() {
                let (mut hook, _) = token
                    .mint(from, from, balance, Default::default(), Default::default())
                    .unwrap();
                token.flush().unwrap();
                hook.call(token.msg()).unwrap();
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
            let bs = MemoryBlockstore::default();
            let mut token_state = TokenState::new(&bs).unwrap();
            let mut token = setup_accounts(
                operator,
                from,
                &TokenAmount::from(allowance),
                &TokenAmount::from(balance),
                bs,
                &mut token_state,
            );

            let assert_error = |err: TokenError, token: Token<MemoryBlockstore, FakeMessenger>| {
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
                            assert_eq!(a, TokenAmount::from(allowance));
                            assert_eq!(delta, TokenAmount::from(transfer));
                        } else {
                            panic!("unexpected error {:?}", err);
                        }
                    }
                    "BALANCE_ERR" => {
                        if let TokenError::TokenState(StateError::InsufficientBalance {
                            owner,
                            balance: b,
                            delta,
                        }) = err
                        {
                            assert_eq!(owner, token.msg.resolve_id(from).unwrap());
                            assert_eq!(delta, TokenAmount::from(transfer).neg());
                            assert_eq!(b, TokenAmount::from(balance));
                        } else {
                            panic!("unexpected error {:?}", err);
                        }
                    }
                    "ADDRESS_ERR" => {
                        if let TokenError::Messaging(MessagingError::AddressNotInitialized(addr)) =
                            err
                        {
                            assert!((addr == *operator) || (addr == *from));
                        } else {
                            panic!("unexpected error {:?}", err);
                        }
                    }
                    _ => panic!("test case not implemented"),
                }
            };

            if token.same_address(operator, from) {
                let res = token.transfer(
                    from,
                    operator,
                    &TokenAmount::from(transfer),
                    RawBytes::default(),
                    RawBytes::default(),
                );

                if behaviour != "OK" {
                    assert_error(res.unwrap_err(), token);
                } else {
                    let (mut hook, _) = res.expect("expect transfer to succeed");
                    hook.call(token.msg()).expect("receiver hook should succeed");
                }
            } else {
                let res = token.transfer_from(
                    operator,
                    from,
                    operator,
                    &TokenAmount::from(transfer),
                    RawBytes::default(),
                    RawBytes::default(),
                );

                if behaviour != "OK" {
                    assert_error(res.unwrap_err(), token);
                } else {
                    let (mut hook, _) = res.expect("expect transfer to succeed");
                    hook.call(token.msg()).expect("receiver hook should succeed");
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

    // TODO: test for re-entrancy bugs by implementing a MethodCaller that calls back on the token contract
}
