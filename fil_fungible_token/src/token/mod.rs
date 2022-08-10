use std::ops::{Neg, Rem};

use cid::Cid;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::Error as SerializationError;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::address::Error as AddressError;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::ActorID;
use num_traits::Signed;
use num_traits::Zero;
use thiserror::Error;

use crate::receiver::types::TokenReceivedParams;
use crate::runtime::messaging::{Messaging, MessagingError};
use crate::runtime::messaging::{Result as MessagingResult, RECEIVER_HOOK_METHOD_NUM};
use crate::token::TokenError::InvalidGranularity;

use self::state::StateInvariantError;
use self::state::{StateError, TokenState};

mod state;
mod types;

/// Ratio of integral units to interpretation as standard token units, as given by FRC-XXXX.
/// Aka "18 decimals".
pub const TOKEN_PRECISION: i64 = 1_000_000_000_000_000_000;

#[derive(Error, Debug)]
pub enum TokenError {
    #[error("error in underlying state {0}")]
    State(#[from] StateError),
    #[error("value {amount:?} for {name:?} must be non-negative")]
    InvalidNegative { name: &'static str, amount: TokenAmount },
    #[error("amount {amount:?} for {name:?} must be a multiple of {granularity:?}")]
    InvalidGranularity { name: &'static str, amount: TokenAmount, granularity: u64 },
    #[error("error calling receiver hook: {0}")]
    Messaging(#[from] MessagingError),
    #[error("receiver hook aborted when {operator:?} sent {amount:?} to {to:?} from {from:?} with exit code {exit_code:?}")]
    ReceiverHook {
        /// Whose balance is being debited
        from: ActorID,
        /// Whose balance is being credited
        to: ActorID,
        /// Who initiated the transfer of funds
        operator: ActorID,
        amount: TokenAmount,
        exit_code: ExitCode,
    },
    #[error("expected {address:?} to be a resolvable id address but threw {source:?} when attempting to resolve")]
    InvalidIdAddress {
        address: Address,
        #[source]
        source: AddressError,
    },
    #[error("error during serialization {0}")]
    Serialization(#[from] SerializationError),
    #[error("error in state invariants {0}")]
    StateInvariant(#[from] StateInvariantError),
}

type Result<T> = std::result::Result<T, TokenError>;

/// Library functions that implement core FRC-??? standards
///
/// Holds injectable services to access/interface with IPLD/FVM layer.
pub struct Token<BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    /// Injected blockstore. The blockstore must reference the same underlying storage under Clone
    bs: BS,
    /// Minimal interface to call methods on other actors (i.e. receiver hooks)
    msg: MSG,
    /// In-memory cache of the state tree
    state: TokenState,
    /// Minimum granularity of token amounts.
    /// All balances and amounts must be a multiple of this granularity.
    /// Set to 1 for standard 18-dp precision, TOKEN_PRECISION for whole units only, or some
    /// value in between.
    granularity: u64,
}

impl<BS, MSG> Token<BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    /// Creates a new token instance using the given blockstore and creates a new empty state tree
    ///
    /// Returns a Token handle that can be used to interact with the token state tree and the Cid
    /// of the state tree root
    pub fn new(bs: BS, msg: MSG, granularity: u64) -> Result<(Self, Cid)> {
        let init_state = TokenState::new(&bs)?;
        let cid = init_state.save(&bs)?;
        let token = Self { bs, msg, state: init_state, granularity };
        Ok((token, cid))
    }

    /// For an already initialised state tree, loads the state tree from the blockstore and returns
    /// a Token handle to interact with it
    pub fn load(bs: BS, msg: MSG, state_cid: Cid, granularity: u64) -> Result<Self> {
        let state = TokenState::load(&bs, &state_cid)?;
        Ok(Self { bs, msg, state, granularity })
    }

    /// Flush state and return Cid for root
    pub fn flush(&mut self) -> Result<Cid> {
        Ok(self.state.save(&self.bs)?)
    }

    /// Opens an atomic transaction on TokenState which allows a closure to make multiple
    /// modifications to the state tree.
    ///
    /// If the closure returns an error, the transaction is dropped atomically and no change is
    /// observed on token state.
    pub fn transaction<F, Res>(&mut self, f: F) -> Result<Res>
    where
        F: FnOnce(&mut TokenState, &BS) -> Result<Res>,
    {
        let mut mutable_state = self.state.clone();
        let res = f(&mut mutable_state, &self.bs)?;
        // if closure didn't error, save state
        self.state = mutable_state;
        Ok(res)
    }

    /// Resolves an address to an ID address, sending a message to initialise an account there if
    /// it doesn't exist
    ///
    /// If the account cannot be created, this function returns MessagingError::AddressNotInitialized
    fn resolve_to_id(&self, address: &Address) -> MessagingResult<ActorID> {
        let id = match self.msg.resolve_id(address) {
            Ok(addr) => addr,
            Err(MessagingError::AddressNotInitialized(_e)) => {
                self.msg.initialize_account(address)?
            }
            Err(e) => return Err(e),
        };
        Ok(id)
    }

    /// Attempts to resolve an address to an ID address, returning MessagingError::AddressNotInitialized
    /// if it wasn't found
    fn get_id(&self, address: &Address) -> MessagingResult<ActorID> {
        self.msg.resolve_id(address)
    }

    /// Checks the state invariants, throwing an error if they are not met
    pub fn check_invariants(&self) -> Result<()> {
        self.state.check_invariants(&self.bs)?;
        Ok(())
    }
}

impl<BS, MSG> Token<BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    /// Mints the specified value of tokens into an account
    ///
    /// The minter is implicitly defined as the caller of the actor, and must be an ID address.
    /// The mint amount must be non-negative or the method returns an error.
    pub fn mint(
        &mut self,
        operator: &Address,
        initial_owner: &Address,
        amount: &TokenAmount,
        data: &RawBytes,
    ) -> Result<()> {
        let amount = validate_amount(amount, "mint", self.granularity)?;
        // Resolve to id addresses
        let operator_id = expect_id(operator)?;
        let owner_id = self.resolve_to_id(initial_owner)?;

        // Increase the balance of the actor and increase total supply
        let old_state = self.state.clone();
        self.transaction(|state, bs| {
            state.change_balance_by(&bs, owner_id, amount)?;
            state.change_supply_by(amount)?;
            Ok(())
        })?;

        // Update state so re-entrant calls see the changes
        self.flush()?;

        // Call receiver hook
        let hook_params = TokenReceivedParams {
            data: data.clone(),
            from: self.msg.actor_id(),
            to: owner_id,
            operator: operator_id,
            amount: amount.clone(),
        };
        match self.msg.send(
            initial_owner,
            RECEIVER_HOOK_METHOD_NUM,
            &RawBytes::serialize(hook_params)?,
            &TokenAmount::zero(),
        ) {
            Ok(receipt) => {
                // hook returned true, so we can continue
                if receipt.exit_code.is_success() {
                    Ok(())
                } else {
                    self.state = old_state;
                    self.flush()?;
                    Err(TokenError::ReceiverHook {
                        operator: operator_id,
                        from: self.msg.actor_id(),
                        to: owner_id,
                        amount: amount.clone(),
                        exit_code: receipt.exit_code,
                    })
                }
            }
            Err(e) => {
                // error calling receiver hook, revert state
                self.state = old_state;
                self.flush()?;
                Err(e.into())
            }
        }
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
        // Load the HAMT holding balances
        let owner = self.get_id(owner);

        match owner {
            Ok(owner) => Ok(self.state.get_balance(&self.bs, owner)?),
            Err(MessagingError::AddressNotInitialized(_)) => {
                // uninitialized address has implicit zero balance
                Ok(TokenAmount::zero())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Gets the allowance between owner and operator
    ///
    /// The allowance is the amount that the operator can transfer or burn out of the owner's account
    /// via the `transfer_from` and `burn_from` methods.
    pub fn allowance(&self, owner: &Address, operator: &Address) -> Result<TokenAmount> {
        let owner = self.get_id(owner);
        let owner = match owner {
            Ok(owner) => owner,
            Err(MessagingError::AddressNotInitialized(_)) => {
                // uninitialized address has implicit zero allowance
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };

        let operator = self.get_id(operator);
        let operator = match operator {
            Ok(operator) => operator,
            Err(MessagingError::AddressNotInitialized(_)) => {
                // uninitialized address has implicit zero allowance
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };

        let allowance = self.state.get_allowance_between(&self.bs, owner, operator)?;
        Ok(allowance)
    }

    /// Increase the allowance that an operator controls of the owner's balance by the requested delta
    ///
    /// The caller of this method is implicitly defined as the owner.
    /// Returns an error if requested delta is negative or there are errors in (de)serialization of
    /// state. Else returns the new allowance.
    pub fn increase_allowance(
        &mut self,
        owner: &Address,
        operator: &Address,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        let delta = validate_amount(delta, "allowance delta", self.granularity)?;
        let owner = expect_id(owner)?;
        let operator = self.resolve_to_id(operator)?;
        let new_amount = self.state.change_allowance_by(&self.bs, owner, operator, delta)?;

        Ok(new_amount)
    }

    /// Decrease the allowance that an operator controls of the owner's balance by the requested delta
    ///
    /// The caller of this method is implicitly defined as the owner.
    /// If the resulting allowance would be negative, the allowance between owner and operator is set
    /// to zero. Returns an error if either the operator or owner address is unresolvable. Returns an
    /// error if requested delta is negative. Else returns the new allowance
    pub fn decrease_allowance(
        &mut self,
        owner: &Address,
        operator: &Address,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        let delta = validate_amount(delta, "allowance delta", self.granularity)?;
        let owner = expect_id(owner)?;
        let operator = self.resolve_to_id(operator)?;
        let new_allowance =
            self.state.change_allowance_by(&self.bs, owner, operator, &delta.neg())?;

        Ok(new_allowance)
    }

    /// Sets the allowance between owner and operator to 0
    pub fn revoke_allowance(&mut self, owner: &Address, operator: &Address) -> Result<()> {
        let owner = expect_id(owner)?;
        let operator = self.resolve_to_id(operator)?;
        self.state.revoke_allowance(&self.bs, owner, operator)?;

        Ok(())
    }

    /// Burns an amount of token from the specified address, decreasing total token supply
    ///
    /// ## For all burn operations
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the target's balance
    ///
    /// Upon successful burn
    /// - The target's balance MUST decrease by the requested value
    /// - The total_supply MUST decrease by the requested value
    ///
    /// ## Operator is the owner address
    /// If the operator is the targeted address, they are implicitly approved to burn an unlimited
    /// amount of tokens (up to their balance)
    ///
    /// ## Operator burning on behalf of another address
    /// If the operator is burning on behalf of the owner the following preconditions
    /// must be met on top of the general burn conditions:
    /// - The operator MUST have an allowance not less than the requested value
    /// In addition to the general postconditions:
    /// - The target-operator allowance MUST decrease by the requested value
    ///
    /// If the burn operation would result in a negative balance for the owner, the burn is
    /// discarded and this method returns an error
    pub fn burn(
        &mut self,
        operator: &Address,
        owner: &Address,
        amount: &TokenAmount,
    ) -> Result<TokenAmount> {
        let amount = validate_amount(amount, "burn", self.granularity)?;
        // owner and operator must exist to burn from
        let operator = expect_id(operator)?;
        let owner = self.resolve_to_id(owner)?;

        let new_amount = self.transaction(|state, bs| {
            if operator != owner {
                // attempt to use allowance and return early if not enough
                state.attempt_use_allowance(&bs, operator, owner, amount)?;
            }
            // attempt to burn the requested amount
            let new_amount = state.change_balance_by(&bs, owner, &amount.clone().neg())?;

            // decrease total_supply
            state.change_supply_by(&amount.neg())?;
            Ok(new_amount)
        })?;

        Ok(new_amount)
    }

    /// Transfers an amount from one actor to another
    ///
    /// ## For all transfer operations
    ///
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the sender's balance
    /// - The receiving actor MUST implement a method called `tokens_received`, corresponding to the
    /// interface specified for FRC-XXX token receivers
    /// - The receiver's `tokens_received` hook MUST NOT abort
    ///
    /// Upon successful transfer:
    /// - The senders's balance MUST decrease by the requested value
    /// - The receiver's balance MUST increase by the requested value
    ///
    /// ## Operator equals owner address
    /// If the operator is the owner address, they are implicitly approved to transfer an unlimited
    /// amount of tokens (up to their balance)
    ///
    /// ## Operator transferring on behalf of owner address
    /// If the operator is transferring on behalf of the target token owner the following preconditions
    /// must be met on top of the general burn conditions:
    /// - The operator MUST have an allowance not less than the requested value
    /// In addition to the general postconditions:
    /// - The owner-operator allowance MUST decrease by the requested value
    pub fn transfer(
        &mut self,
        operator: &Address,
        from: &Address,
        to: &Address,
        amount: &TokenAmount,
        data: &RawBytes,
    ) -> Result<()> {
        let amount = validate_amount(amount, "transfer", self.granularity)?;
        // operator must be an id address
        let operator = expect_id(operator)?;
        // resolve owner and receiver
        let from = self.resolve_to_id(from)?;
        let to_id = self.resolve_to_id(to)?;

        let old_state = self.state.clone();
        self.transaction(|state, bs| {
            if operator != from {
                // attempt to use allowance and return early if not enough
                state.attempt_use_allowance(&bs, operator, from, amount)?;
            }
            state.change_balance_by(&bs, to_id, amount)?;
            state.change_balance_by(&bs, from, &amount.neg())?;
            Ok(())
        })?;

        // flush state as re-entrant call needs to see new balances
        self.flush()?;

        // call receiver hook
        let params = TokenReceivedParams {
            operator,
            from,
            to: to_id,
            amount: amount.clone(),
            data: data.clone(),
        };
        match self.msg.send(
            to,
            RECEIVER_HOOK_METHOD_NUM,
            &RawBytes::serialize(params)?,
            &TokenAmount::zero(),
        ) {
            Ok(receipt) => {
                // hook returned true, so we can continue
                if receipt.exit_code.is_success() {
                    Ok(())
                } else {
                    self.state = old_state;
                    self.flush()?;
                    Err(TokenError::ReceiverHook {
                        operator,
                        from,
                        to: to_id,
                        amount: amount.clone(),
                        exit_code: receipt.exit_code,
                    })
                }
            }
            Err(e) => {
                // error calling receiver hook, revert state
                self.state = old_state;
                self.flush()?;
                Err(e.into())
            }
        }
    }
}

/// Expects an address to be an ID address and returns the ActorID
///
/// If it is not an ID address, this function returns a TokenError::InvalidIdAddress error
fn expect_id(address: &Address) -> Result<ActorID> {
    address.id().map_err(|e| TokenError::InvalidIdAddress { address: *address, source: e })
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
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_shared::address::{Address, BLS_PUB_LEN};
    use fvm_shared::econ::TokenAmount;
    use num_traits::Zero;

    use crate::receiver::types::TokenReceivedParams;
    use crate::runtime::messaging::FakeMessenger;
    use crate::token::TokenError;

    use super::Token;

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

    fn new_token(granularity: u64) -> Token<MemoryBlockstore, FakeMessenger> {
        Token::new(
            MemoryBlockstore::default(),
            FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6),
            granularity,
        )
        .unwrap()
        .0
    }

    fn assert_last_msg_eq(messenger: &FakeMessenger, expected: TokenReceivedParams) {
        let last_called = messenger.last_hook.borrow().clone().unwrap();
        assert_eq!(last_called, expected);
    }

    #[test]
    fn it_instantiates_and_persists() {
        // create a new token
        let bs = MemoryBlockstore::new();
        let (mut token, _) =
            Token::new(&bs, FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6), 1).unwrap();

        // state exists but is empty
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // mint some value
        token.mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(100), &Default::default()).unwrap();
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // flush token to blockstore
        let cid = token.flush().unwrap();

        // the returned cid can be used to reference the same token state
        let token2 =
            Token::load(&bs, FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6), cid, 1).unwrap();
        assert_eq!(token2.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_provides_atomic_transactions() {
        let mut token = new_token(1);

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
        let mut token = new_token(1);

        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        token
            .mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(1_000_000), &Default::default())
            .unwrap();

        // balance and total supply both went up
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // cannot mint a negative amount
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(-1), &Default::default()).unwrap_err();

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // mint zero
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::zero(), &Default::default()).unwrap();

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                data: Default::default(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::zero(),
            },
        );

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // mint again to same address
        token
            .mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(1_000_000), &Default::default())
            .unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(2_000_000));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                data: Default::default(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: TREASURY.id().unwrap(),
                amount: TokenAmount::from(1_000_000),
            },
        );

        // mint to a different address
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(1_000_000), &Default::default()).unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(3_000_000));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                data: Default::default(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::from(1_000_000),
            },
        );

        // carols account was unaffected
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // can mint to secp address
        let secp_address = secp_address();
        // initially zero
        assert_eq!(token.balance_of(&secp_address).unwrap(), TokenAmount::zero());
        // self-mint to secp address
        token
            .mint(TOKEN_ACTOR, &secp_address, &TokenAmount::from(1_000_000), &Default::default())
            .unwrap();

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                data: Default::default(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: token.get_id(&secp_address).unwrap(),
                amount: TokenAmount::from(1_000_000),
            },
        );

        // can mint to bls address
        let bls_address = bls_address();
        // initially zero
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::zero());
        // minting creates the account
        token
            .mint(TOKEN_ACTOR, &bls_address, &TokenAmount::from(1_000_000), &Default::default())
            .unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.balance_of(&secp_address).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(5_000_000));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: TOKEN_ACTOR.id().unwrap(),
                data: Default::default(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: token.get_id(&bls_address).unwrap(),
                amount: TokenAmount::from(1_000_000),
            },
        );

        // mint fails if actor address cannot be initialised
        let actor_address: Address = actor_address();
        token
            .mint(TOKEN_ACTOR, &actor_address, &TokenAmount::from(1_000_000), &Default::default())
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
        let mut token = new_token(1);

        // force hook to abort
        token.msg.abort_next_send();
        let err = token
            .mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(1_000_000), &Default::default())
            .unwrap_err();

        // check error shape
        match err {
            TokenError::ReceiverHook { from, to, operator, amount, exit_code: _exit_code } => {
                assert_eq!(from, TOKEN_ACTOR.id().unwrap());
                assert_eq!(to, TREASURY.id().unwrap());
                assert_eq!(operator, TOKEN_ACTOR.id().unwrap());
                assert_eq!(amount, TokenAmount::from(1_000_000));
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
        let mut token = new_token(1);

        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(600_000);
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &Default::default()).unwrap();
        token.burn(TREASURY, TREASURY, &burn_amount).unwrap();

        // total supply decreased
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // cannot burn a negative amount
        token.burn(TREASURY, TREASURY, &TokenAmount::from(-1)).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // burn zero
        token.burn(TREASURY, TREASURY, &TokenAmount::zero()).unwrap();

        // balances and supply were unchanged
        let remaining_balance = token.balance_of(TREASURY).unwrap();
        assert_eq!(remaining_balance, TokenAmount::from(400_000));
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());

        // burn exact amount left
        token.burn(TREASURY, TREASURY, &remaining_balance).unwrap();
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::zero());
        // alice's account unaffected
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_burn_below_zero() {
        let mut token = new_token(1);

        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(2_000_000);
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &Default::default()).unwrap();
        token.burn(TREASURY, TREASURY, &burn_amount).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_transfers() {
        let mut token = new_token(1);

        // mint 100 for owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();
        // transfer 60 from owner -> receiver
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(60), &Default::default()).unwrap();

        // owner has 100 - 60 = 40
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        // receiver has 0 + 60 = 60
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_eq!(
            token.msg.last_hook.borrow().clone().unwrap(),
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                amount: TokenAmount::from(60),
                to: BOB.id().unwrap(),
                data: Default::default(),
            }
        );

        // cannot transfer a negative value
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(-1), &Default::default()).unwrap_err();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // transfer zero value
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::zero(), &Default::default()).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: BOB.id().unwrap(),
                amount: TokenAmount::zero(),
                data: Default::default(),
            },
        );

        // transfer zero to self
        token.transfer(ALICE, ALICE, ALICE, &TokenAmount::zero(), &Default::default()).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::zero(),
                data: Default::default(),
            },
        );

        // transfer value to self
        token.transfer(ALICE, ALICE, ALICE, &TokenAmount::from(10), &Default::default()).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::from(10),
                data: Default::default(),
            },
        );

        // transfer to pubkey
        let secp_address = &secp_address();
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        token
            .transfer(ALICE, ALICE, secp_address, &TokenAmount::from(10), &Default::default())
            .unwrap();
        // alice supply dropped
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(30));
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::from(10));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: token.get_id(secp_address).unwrap(),
                amount: TokenAmount::from(10),
                data: Default::default(),
            },
        );

        // transfer to uninitialized pubkey
        let bls_address = &bls_address();
        assert_eq!(token.balance_of(bls_address).unwrap(), TokenAmount::zero());
        token
            .transfer(ALICE, ALICE, bls_address, &TokenAmount::from(10), &Default::default())
            .unwrap();
        // alice supply dropped
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(20));
        // new address has balance
        assert_eq!(token.balance_of(bls_address).unwrap(), TokenAmount::from(10));
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::from(10));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                to: token.get_id(bls_address).unwrap(),
                from: ALICE.id().unwrap(),
                amount: TokenAmount::from(10),
                data: Default::default(),
            },
        );
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_transfer_when_receiver_hook_aborts() {
        let mut token = new_token(1);

        // mint 100 for owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();

        // transfer 60 from owner -> receiver, but simulate receiver aborting the hook
        token.msg.abort_next_send();
        let err = token
            .transfer(ALICE, ALICE, BOB, &TokenAmount::from(60), &Default::default())
            .unwrap_err();

        // check error shape
        match err {
            TokenError::ReceiverHook { from, to, operator, amount, exit_code: _exit_code } => {
                assert_eq!(from, ALICE.id().unwrap());
                assert_eq!(to, BOB.id().unwrap());
                assert_eq!(operator, ALICE.id().unwrap());
                assert_eq!(amount, TokenAmount::from(60));
            }
            _ => panic!("expected receiver hook error"),
        };

        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(0));

        // transfer 60 from owner -> self, simulate receiver aborting the hook
        token.msg.abort_next_send();
        let err = token
            .transfer(ALICE, ALICE, ALICE, &TokenAmount::from(60), &Default::default())
            .unwrap_err();

        // check error shape
        match err {
            TokenError::ReceiverHook { from, to, operator, amount, exit_code: _exit_code } => {
                assert_eq!(from, ALICE.id().unwrap());
                assert_eq!(to, ALICE.id().unwrap());
                assert_eq!(operator, ALICE.id().unwrap());
                assert_eq!(amount, TokenAmount::from(60));
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
        let mut token = new_token(1);

        // mint 50 for the owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(50), &Default::default()).unwrap();

        // attempt transfer 51 from owner -> receiver
        token
            .transfer(ALICE, ALICE, BOB, &TokenAmount::from(51), &Default::default())
            .expect_err("transfer should have failed");

        // balances remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(50));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_tracks_allowances() {
        let mut token = new_token(1);

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
        let mut token = new_token(1);

        // mint 100 for the owner
        token.mint(ALICE, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();
        // approve 100 spending allowance for operator
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(100)).unwrap();
        // operator makes transfer of 60 from owner -> receiver
        token.transfer(CAROL, ALICE, BOB, &TokenAmount::from(60), &Default::default()).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: CAROL.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: BOB.id().unwrap(),
                amount: TokenAmount::from(60),
                data: Default::default(),
            },
        );

        // verify allowance is correct
        let operator_allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(operator_allowance, TokenAmount::from(40));

        // operator makes another transfer of 40 from owner -> self
        token.transfer(CAROL, ALICE, CAROL, &TokenAmount::from(40), &Default::default()).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::from(40));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                operator: CAROL.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: CAROL.id().unwrap(),
                amount: TokenAmount::from(40),
                data: Default::default(),
            },
        );

        // verify allowance is correct
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::zero());
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_allows_delegated_burns() {
        let mut token = new_token(1);

        let mint_amount = TokenAmount::from(1_000_000);
        let approval_amount = TokenAmount::from(600_000);
        let burn_amount = TokenAmount::from(600_000);

        // mint the total amount
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &Default::default()).unwrap();
        // approve the burner to spend the allowance
        token.increase_allowance(TREASURY, ALICE, &approval_amount).unwrap();
        // burn the approved amount
        token.burn(ALICE, TREASURY, &burn_amount).unwrap();

        // total supply decreased
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        // burner approval decreased
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());

        // disallows another delegated burn as approval is zero
        // burn the approved amount
        token.burn(ALICE, TREASURY, &burn_amount).expect_err("unable to burn more than allowance");

        // balances didn't change
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());

        // cannot burn on uninitialized account
        let initializable = bls_address();
        token.burn(&initializable, &initializable, &TokenAmount::from(1)).unwrap_err();

        // balances didn't change
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_allowance() {
        let mut token = new_token(1);

        // mint 100 for the owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();
        // approve only 40 spending allowance for operator
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(40)).unwrap();
        // operator attempts makes transfer of 60 from owner -> receiver
        // this is within the owner's balance but not within the operator's allowance
        token.transfer(CAROL, ALICE, BOB, &TokenAmount::from(60), &Default::default()).unwrap_err();

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
        let mut token = new_token(1);

        // mint 50 for the owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(50), &Default::default()).unwrap();

        // allow 100 to be spent by operator
        token.increase_allowance(ALICE, BOB, &TokenAmount::from(100)).unwrap();

        // operator attempts transfer 51 from owner -> operator
        // they have enough allowance, but not enough balance
        token.transfer(BOB, ALICE, BOB, &TokenAmount::from(51), &Default::default()).unwrap_err();

        // attempt burn 51 by operator
        token.burn(BOB, ALICE, &TokenAmount::from(51)).unwrap_err();

        // balances remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(50));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        assert_eq!(token.allowance(ALICE, BOB).unwrap(), TokenAmount::from(100));
        token.check_invariants().unwrap();
    }

    #[test]
    fn it_enforces_granularity() {
        let mut token = new_token(100);

        // Minting
        token
            .mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(1), &Default::default())
            .expect_err("minted below granularity");
        token
            .mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(10), &Default::default())
            .expect_err("minted below granularity");
        token
            .mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(99), &Default::default())
            .expect_err("minted below granularity");
        token
            .mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(101), &Default::default())
            .expect_err("minted below granularity");
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(0), &Default::default()).unwrap();
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(200), &Default::default()).unwrap();
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(1000), &Default::default()).unwrap();

        // Burn
        token.burn(ALICE, ALICE, &TokenAmount::from(1)).expect_err("burned below granularity");
        token.burn(ALICE, ALICE, &TokenAmount::from(0)).unwrap();
        token.burn(ALICE, ALICE, &TokenAmount::from(100)).unwrap();

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
            .transfer(ALICE, ALICE, BOB, &TokenAmount::from(1), &Default::default())
            .expect_err("transfer delta below granularity");
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(0), &Default::default()).unwrap();
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(100), &Default::default()).unwrap();
    }

    // TODO: test for re-entrancy bugs by implementing a MethodCaller that calls back on the token contract
}
