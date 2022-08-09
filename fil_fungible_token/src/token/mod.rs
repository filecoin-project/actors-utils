mod state;
mod types;

use self::state::{StateError as TokenStateError, TokenState};
use crate::receiver::types::TokenReceivedParams;
use crate::runtime::messaging::{Messaging, MessagingError};
use crate::runtime::messaging::{Result as MessagingResult, RECEIVER_HOOK_METHOD_NUM};

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
use std::ops::Neg;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TokenError {
    #[error("error in underlying state {0}")]
    TokenState(#[from] TokenStateError),
    #[error("invalid negative: {0}")]
    InvalidNegative(String),
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
    pub fn new(bs: BS, msg: MSG) -> Result<(Self, Cid)> {
        let init_state = TokenState::new(&bs)?;
        let cid = init_state.save(&bs)?;
        let token = Self { bs, msg, state: init_state };
        Ok((token, cid))
    }

    /// For an already initialised state tree, loads the state tree from the blockstore and returns
    /// a Token handle to interact with it
    pub fn load(bs: BS, msg: MSG, state_cid: Cid) -> Result<Self> {
        let state = TokenState::load(&bs, &state_cid)?;
        Ok(Self { bs, msg, state })
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

    /// Calls the receiver hook, reverting the state if it aborts or there is a messaging error
    fn call_receiver_hook_or_revert(
        &mut self,
        token_receiver: &Address,
        params: TokenReceivedParams,
        old_state: TokenState,
    ) -> Result<()> {
        let receipt = match self.msg.send(
            token_receiver,
            RECEIVER_HOOK_METHOD_NUM,
            &RawBytes::serialize(&params)?,
            &TokenAmount::zero(),
        ) {
            Ok(receipt) => receipt,
            Err(e) => {
                self.state = old_state;
                self.flush()?;
                return Err(e.into());
            }
        };

        match receipt.exit_code {
            ExitCode::OK => Ok(()),
            abort_code => {
                self.state = old_state;
                self.flush()?;
                Err(TokenError::ReceiverHook {
                    from: params.from,
                    to: params.to,
                    operator: params.operator,
                    amount: params.amount,
                    exit_code: abort_code,
                })
            }
        }
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
        if amount.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "mint amount {} cannot be negative",
                amount
            )));
        }

        // init the operator account so that its actor ID can be referenced in the receiver hook
        let operator_id = self.resolve_or_init(operator)?;
        // init the owner account as allowance and balance checks are not performed for minting
        let owner_id = self.resolve_or_init(initial_owner)?;

        let old_state = self.state.clone();

        // Increase the balance of the actor and increase total supply
        self.transaction(|state, bs| {
            state.change_balance_by(&bs, owner_id, amount)?;
            state.change_supply_by(amount)?;
            Ok(())
        })?;

        // Update state so re-entrant calls see the changes
        self.flush()?;
        // Call receiver hook
        self.call_receiver_hook_or_revert(
            initial_owner,
            TokenReceivedParams {
                data: data.clone(),
                from: self.msg.actor_id(),
                to: owner_id,
                operator: operator_id,
                amount: amount.clone(),
            },
            old_state,
        )
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
        if delta.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "increase allowance delta {} cannot be negative",
                delta
            )));
        }

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
        if delta.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "decrease allowance delta {} cannot be negative",
                delta
            )));
        }

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
    /// ## For all burn operations
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the target's balance
    /// - If the burn operation would result in a negative balance for the owner, the burn is
    /// discarded and this method returns an error
    ///
    /// Upon successful burn
    /// - The target's balance decreases by the requested value
    /// - The total_supply decreases by the requested value
    ///
    /// ## Operator is the owner address
    /// If the operator is the targeted address, they are implicitly approved to burn an unlimited
    /// amount of tokens (up to their balance)
    ///
    /// ## Operator burning on behalf of another address
    /// If the operator is burning on behalf of the owner, then additionally, the operator MUST have
    /// an allowance not less than the requested value
    ///
    /// Upon successful burn
    /// - The total_supply decreases by the requested value
    pub fn burn(
        &mut self,
        operator: &Address,
        owner: &Address,
        amount: &TokenAmount,
    ) -> Result<TokenAmount> {
        if amount.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "burn amount {} cannot be negative",
                amount
            )));
        }

        // owner-initiated burn
        if self.same_address(owner, operator) {
            let owner = self.resolve_or_init(owner)?;
            return self.transaction(|state, bs| {
                // attempt to burn the requested amount
                let new_amount = state.change_balance_by(&bs, owner, &amount.clone().neg())?;
                // decrease total_supply
                state.change_supply_by(&amount.neg())?;
                Ok(new_amount)
            });
        }

        // operator must be existing to have a non-zero allowance
        let operator = match self.get_id(operator) {
            Ok(operator) => operator,
            Err(MessagingError::AddressNotResolved(addr)) => {
                // if not resolved, implicit allowance zero is not permitted to burn, so return an
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

        // owner must be existing to have set a non-zero allowance
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
            state.attempt_use_allowance(&bs, operator, owner, amount)?;
            // attempt to burn the requested amount
            let new_amount = state.change_balance_by(&bs, owner, &amount.clone().neg())?;
            // decrease total_supply
            state.change_supply_by(&amount.neg())?;
            Ok(new_amount)
        })
    }

    /// Transfers an amount from one actor to another
    ///
    /// ## For all transfer operations
    ///
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the sender's balance
    /// - The receiving actor MUST implement a method called `tokens_received`, corresponding to the
    /// interface specified for FRC-XXX token receiver. If the receiving hook aborts, when called,
    /// the transfer is discarded and this method returns an error
    ///
    /// Upon successful transfer:
    /// - The senders's balance decreases by the requested value
    /// - The receiver's balance increases by the requested value
    ///
    /// ## Operator equals owner address
    /// If the operator is the owner address, they are implicitly approved to transfer an unlimited
    /// amount of tokens (up to their balance)
    ///
    /// ## Operator transferring on behalf of owner address
    /// If the operator is transferring on behalf of the target token owner, then additionally, the
    /// operator MUST be initialised AND have an allowance not less than the requested value
    ///
    /// Upon successful transfer:
    /// - The owner-operator allowance decreases by the requested value
    pub fn transfer(
        &mut self,
        operator: &Address,
        from: &Address,
        to: &Address,
        amount: &TokenAmount,
        data: &RawBytes,
    ) -> Result<()> {
        if amount.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "transfer amount {} cannot be negative",
                amount
            )));
        }
        let old_state = self.state.clone();

        // owner-initiated transfer
        if self.same_address(operator, from) {
            let from = self.resolve_or_init(from)?;
            let to_id = self.resolve_or_init(to)?;
            // skip allowance check for self-managed transfers
            self.transaction(|state, bs| {
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
                } else {
                    state.change_balance_by(&bs, to_id, amount)?;
                    state.change_balance_by(&bs, from, &amount.neg())?;
                }
                Ok(())
            })?;

            // call receiver hook
            self.flush()?;
            return self.call_receiver_hook_or_revert(
                to,
                TokenReceivedParams {
                    operator: from,
                    from,
                    to: to_id,
                    amount: amount.clone(),
                    data: data.clone(),
                },
                old_state,
            );
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
        self.transaction(|state, bs| {
            state.attempt_use_allowance(&bs, operator, from, amount)?;
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
            } else {
                state.change_balance_by(&bs, to_id, amount)?;
                state.change_balance_by(&bs, from, &amount.neg())?;
            }
            Ok(())
        })?;

        // flush state as receiver hook needs to see new balances
        self.flush()?;
        self.call_receiver_hook_or_revert(
            to,
            TokenReceivedParams {
                operator: operator_id,
                from,
                to: to_id,
                amount: amount.clone(),
                data: data.clone(),
            },
            old_state,
        )
    }
}

#[cfg(test)]
mod test {
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_shared::address::{Address, BLS_PUB_LEN};
    use fvm_shared::econ::TokenAmount;
    use num_traits::Zero;
    use std::ops::Neg;

    use super::state::StateError;
    use super::Token;
    use crate::receiver::types::TokenReceivedParams;
    use crate::runtime::messaging::{FakeMessenger, Messaging, MessagingError};
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

    fn new_token() -> Token<MemoryBlockstore, FakeMessenger> {
        Token::new(MemoryBlockstore::default(), FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6))
            .unwrap()
            .0
    }

    fn assert_last_hook_call_eq(messenger: &FakeMessenger, expected: TokenReceivedParams) {
        let last_called = messenger.last_message.borrow().clone().unwrap();
        let last_called: TokenReceivedParams = last_called.deserialize().unwrap();
        assert_eq!(last_called, expected);
    }

    #[test]
    fn it_instantiates_and_persists() {
        // create a new token
        let bs = MemoryBlockstore::new();
        let (mut token, _) =
            Token::new(&bs, FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6)).unwrap();

        // state exists but is empty
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // mint some value
        token.mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(100), &Default::default()).unwrap();
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // flush token to blockstore
        let cid = token.flush().unwrap();

        // the returned cid can be used to reference the same token state
        let token2 =
            Token::load(&bs, FakeMessenger::new(TOKEN_ACTOR.id().unwrap(), 6), cid).unwrap();
        assert_eq!(token2.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_provides_atomic_transactions() {
        let mut token = new_token();

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
        let mut token = new_token();

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
        assert_last_hook_call_eq(
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
        assert_last_hook_call_eq(
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
        assert_last_hook_call_eq(
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
        assert_last_hook_call_eq(
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
        assert_last_hook_call_eq(
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
    }

    #[test]
    fn it_fails_to_mint_if_receiver_hook_aborts() {
        let mut token = new_token();

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
    }

    #[test]
    fn it_burns() {
        let mut token = new_token();

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
    }

    #[test]
    fn it_fails_to_burn_below_zero() {
        let mut token = new_token();

        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(2_000_000);
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &Default::default()).unwrap();
        token.burn(TREASURY, TREASURY, &burn_amount).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
    }

    #[test]
    fn it_transfers() {
        let mut token = new_token();

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
        assert_last_hook_call_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                amount: TokenAmount::from(60),
                to: BOB.id().unwrap(),
                data: Default::default(),
            },
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
        assert_last_hook_call_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: BOB.id().unwrap(),
                amount: TokenAmount::zero(),
                data: Default::default(),
            },
        );
    }

    #[test]
    fn it_transfers_to_self() {
        let mut token = new_token();

        // mint 100 for owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();
        // transfer zero to self
        token.transfer(ALICE, ALICE, ALICE, &TokenAmount::zero(), &Default::default()).unwrap();

        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
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
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::from(10),
                data: Default::default(),
            },
        );
    }

    #[test]
    fn it_transfers_to_uninitialized_addresses() {
        let mut token = new_token();

        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();

        // transfer to an uninitialized pubkey
        let secp_address = &secp_address();
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        token
            .transfer(ALICE, ALICE, secp_address, &TokenAmount::from(10), &Default::default())
            .unwrap();

        // balances changed
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(90));
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::from(10));

        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
            &token.msg,
            TokenReceivedParams {
                operator: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: token.get_id(secp_address).unwrap(),
                amount: TokenAmount::from(10),
                data: Default::default(),
            },
        );
    }

    #[test]
    fn it_transfers_from_uninitialized_addresses() {
        let mut token = new_token();

        let secp_address = &secp_address();
        // non-zero transfer should fail
        assert!(token
            .transfer(secp_address, secp_address, ALICE, &TokenAmount::from(1), &Default::default())
            .is_err());
        // balances unchanged
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        // supply unchanged
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // zero-transfer should succeed
        token
            .transfer(secp_address, secp_address, ALICE, &TokenAmount::zero(), &Default::default())
            .unwrap();
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
                actor_address,
                ALICE,
                &TokenAmount::zero(),
                &Default::default(),
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
        assert!(token.get_id(actor_address).is_err())
    }

    #[test]
    fn it_fails_to_transfer_when_receiver_hook_aborts() {
        let mut token = new_token();

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
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_balance() {
        let mut token = new_token();

        // mint 50 for the owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(50), &Default::default()).unwrap();

        // attempt transfer 51 from owner -> receiver
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(51), &Default::default()).unwrap_err();

        // balances remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(50));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_tracks_allowances() {
        let mut token = new_token();

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
    }

    #[test]
    fn it_allows_delegated_transfer() {
        let mut token = new_token();

        // mint 100 for the owner
        token.mint(ALICE, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();

        // operator can't transfer without allowance, even if amount is zero
        token.transfer(CAROL, ALICE, ALICE, &TokenAmount::zero(), &Default::default()).unwrap_err();

        // approve 100 spending allowance for operator
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(100)).unwrap();
        // operator makes transfer of 60 from owner -> receiver
        token.transfer(CAROL, ALICE, BOB, &TokenAmount::from(60), &Default::default()).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // check receiver hook was called with correct shape
        assert_last_hook_call_eq(
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
        assert_last_hook_call_eq(
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
    }

    #[test]
    fn it_allows_delegated_transfer_by_resolvable_pubkey() {
        let mut token = new_token();

        // mint 100 for owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();

        let initialised_address = &secp_address();
        token.msg.initialize_account(initialised_address).unwrap();

        // an initialised pubkey cannot transfer zero out of Alice balance without an allowance
        token
            .transfer(
                initialised_address,
                ALICE,
                initialised_address,
                &TokenAmount::zero(),
                &Default::default(),
            )
            .unwrap_err();

        // balances remained same
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::zero());
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // initialised pubkey can has zero-allowance, so cannot transfer non-zero amount
        token
            .transfer(
                initialised_address,
                ALICE,
                initialised_address,
                &TokenAmount::from(1),
                &Default::default(),
            )
            .unwrap_err();
        // balances remained same
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::zero());
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // the pubkey can be given an allowance which it can use to transfer tokens
        token.increase_allowance(ALICE, initialised_address, &TokenAmount::from(100)).unwrap();
        token
            .transfer(
                initialised_address,
                ALICE,
                initialised_address,
                &TokenAmount::from(1),
                &Default::default(),
            )
            .unwrap();
        // balances and allowance changed
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(99));
        assert_eq!(token.balance_of(initialised_address).unwrap(), TokenAmount::from(1));
        assert_eq!(token.allowance(ALICE, initialised_address).unwrap(), TokenAmount::from(99));
        // supply remains same
        assert_eq!(token.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_disallows_delgated_transfer_by_uninitialised_pubkey() {
        let mut token = new_token();

        // mint 100 for owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &Default::default()).unwrap();

        // non-zero transfer by an uninitialized pubkey
        let secp_address = &secp_address();
        let err = token
            .transfer(secp_address, ALICE, ALICE, &TokenAmount::from(10), &Default::default())
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
            .transfer(secp_address, ALICE, ALICE, &TokenAmount::zero(), &Default::default())
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
        let mut token = new_token();

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

        // disallows another delegated burn as approval is now zero
        // burn the approved amount
        token.burn(ALICE, TREASURY, &burn_amount).unwrap_err();

        // balances didn't change
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.allowance(TREASURY, ALICE).unwrap(), TokenAmount::zero());

        // cannot burn again due to insufficient balance
        let err = token.burn(ALICE, TREASURY, &burn_amount).unwrap_err();

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
        let mut token = new_token();

        let mint_amount = TokenAmount::from(1_000_000);
        let approval_amount = TokenAmount::from(600_000);
        let burn_amount = TokenAmount::from(600_000);

        // create a resolvable pubkey
        let secp_address = &secp_address();
        let secp_id = token.msg.initialize_account(secp_address).unwrap();

        // mint the total amount
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &Default::default()).unwrap();
        // approve the burner to spend the allowance
        token.increase_allowance(TREASURY, secp_address, &approval_amount).unwrap();
        // burn the approved amount
        token.burn(secp_address, TREASURY, &burn_amount).unwrap();

        // total supply decreased
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        // treasury balance decreased
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        // burner approval decreased
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());

        // cannot burn non-zero again
        let err = token.burn(secp_address, TREASURY, &burn_amount).unwrap_err();
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
        let res = token.burn(secp_address, TREASURY, &TokenAmount::zero());

        // balances unchanged
        assert!(res.is_err());
        assert_eq!(token.total_supply(), TokenAmount::from(400_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(400_000));
        assert_eq!(token.allowance(TREASURY, secp_address).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_disallows_delegated_burns_by_uninitialised_pubkeys() {
        let mut token = new_token();

        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(600_000);

        // create a resolvable pubkey
        let secp_address = &secp_address();

        // mint the total amount
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &Default::default()).unwrap();

        // cannot burn non-zero
        let err = token.burn(secp_address, TREASURY, &burn_amount).unwrap_err();
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
        let err = token.burn(secp_address, TREASURY, &TokenAmount::zero()).unwrap_err();
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
        let mut token = new_token();

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
    }

    #[test]
    fn it_doesnt_use_allowance_when_insufficent_balance() {
        let mut token = new_token();

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
    }

    #[test]
    fn it_doesnt_initialize_accounts_when_default_values_can_be_returned() {
        let token = new_token();
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
        fn setup_accounts(
            operator: &Address,
            from: &Address,
            allowance: &TokenAmount,
            balance: &TokenAmount,
        ) -> Token<MemoryBlockstore, FakeMessenger> {
            // fresh token state
            let mut token = new_token();
            // set allowance if not zero (avoiding unecessary account instantiation)
            if !allowance.is_zero() && !(from == operator) {
                token.increase_allowance(from, operator, allowance).unwrap();
            }
            // set balance if not zero (avoiding unecessary account insantiation)
            if !balance.is_zero() {
                token.mint(from, from, balance, &Default::default()).unwrap();
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
            let mut token = setup_accounts(
                operator,
                from,
                &TokenAmount::from(allowance),
                &TokenAmount::from(balance),
            );
            let res = token.transfer(
                operator,
                from,
                operator,
                &TokenAmount::from(transfer),
                &Default::default(),
            );

            match behaviour {
                "OK" => res.expect("expected transfer to succeed"),
                "ALLOWANCE_ERR" => {
                    let err = res.unwrap_err();
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
                    let err = res.unwrap_err();
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
                    let err = res.unwrap_err();
                    if let TokenError::Messaging(MessagingError::AddressNotInitialized(addr)) = err
                    {
                        assert!((addr == *operator) || (addr == *from));
                    } else {
                        panic!("unexpected error {:?}", err);
                    }
                }
                _ => panic!("test case not implemented"),
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

        // actor addresses are currently never initialisable, so they have different errors to pubkey addresses
        // the error here is from attempting to call the receiver hook on an uninitialised address
        // id address operates on actor address
        assert_behaviour(ALICE, &actor_address(), 0, 0, 0, "ADDRESS_ERR");
        assert_behaviour(ALICE, &actor_address(), 0, 0, 1, "ADDRESS_ERR");
        // impossible for actor to have balance (for now)
        // impossible for actor to have allowance (for now)
        // even the same actor will fail to transfer to itself
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
