mod state;
mod types;

use self::state::{StateError, TokenState};
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
    State(#[from] StateError),
    #[error("invalid negative: {0}")]
    InvalidNegative(String),
    #[error("error calling receiver hook: {0}")]
    Messaging(#[from] MessagingError),
    #[error("receiver hook aborted when {from:?} sent {amount:?} to {to:?} by {sender:?} with exit code {exit_code:?}")]
    ReceiverHook {
        from: ActorID,
        to: ActorID,
        sender: ActorID,
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
    fn resolve_to_id(&self, address: &Address) -> MessagingResult<ActorID> {
        let holder_id = match self.msg.resolve_id(address) {
            Ok(addr) => addr,
            Err(MessagingError::AddressNotInitialized(_e)) => {
                self.msg.initialize_account(address)?
            }
            Err(e) => return Err(e),
        };
        Ok(holder_id)
    }

    /// Attempts to resolve an address to an ID address, returning MessagingError::AddressNotInitialized
    /// if it wasn't found
    fn get_id(&self, address: &Address) -> MessagingResult<ActorID> {
        self.msg.resolve_id(address)
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
        minter: &Address,
        initial_holder: &Address,
        amount: &TokenAmount,
        data: &[u8],
    ) -> Result<()> {
        if amount.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "mint amount {} cannot be negative",
                amount
            )));
        }

        // Resolve to id addresses
        let minter_id = expect_id(minter)?;
        let holder_id = self.resolve_to_id(initial_holder)?;

        let old_state = self.state.clone();

        // Increase the balance of the actor and increase total supply
        self.transaction(|state, bs| {
            state.change_balance_by(&bs, holder_id, amount)?;
            state.change_supply_by(amount)?;
            Ok(())
        })?;

        // Update state so re-entrant calls see the changes
        self.flush()?;

        // Call receiver hook
        let hook_params = TokenReceivedParams {
            data: RawBytes::from(data.to_vec()),
            from: self.msg.actor_id(),
            to: holder_id,
            sender: minter_id,
            amount: amount.clone(),
        };
        match self.msg.send(
            initial_holder,
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
                        from: self.msg.actor_id(),
                        to: holder_id,
                        sender: minter_id,
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
    pub fn balance_of(&self, holder: &Address) -> Result<TokenAmount> {
        // Load the HAMT holding balances
        let holder = self.get_id(holder);

        match holder {
            Ok(holder) => Ok(self.state.get_balance(&self.bs, holder)?),
            Err(MessagingError::AddressNotInitialized(_)) => {
                // uninitialized address has implicit zero balance
                Ok(TokenAmount::zero())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Gets the allowance between owner and spender
    ///
    /// The allowance is the amount that the spender can transfer or burn out of the owner's account
    /// via the `transfer_from` and `burn_from` methods.
    pub fn allowance(&self, owner: &Address, spender: &Address) -> Result<TokenAmount> {
        let owner = self.get_id(owner);
        let owner = match owner {
            Ok(owner) => owner,
            Err(MessagingError::AddressNotInitialized(_)) => {
                // uninitialized address has implicit zero allowance
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };

        let spender = self.get_id(spender);
        let spender = match spender {
            Ok(spender) => spender,
            Err(MessagingError::AddressNotInitialized(_)) => {
                // uninitialized address has implicit zero allowance
                return Ok(TokenAmount::zero());
            }
            Err(e) => return Err(e.into()),
        };

        let allowance = self.state.get_allowance_between(&self.bs, owner, spender)?;
        Ok(allowance)
    }

    /// Increase the allowance that a spender controls of the owner's balance by the requested delta
    ///
    /// The caller of this method is implicitly defined as the owner.
    /// Returns an error if requested delta is negative or there are errors in (de)serialization of
    /// state. Else returns the new allowance.
    pub fn increase_allowance(
        &mut self,
        owner: &Address,
        spender: &Address,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "increase allowance delta {} cannot be negative",
                delta
            )));
        }

        let owner = expect_id(owner)?;
        let spender = self.resolve_to_id(spender)?;
        let new_amount = self.state.change_allowance_by(&self.bs, owner, spender, delta)?;

        Ok(new_amount)
    }

    /// Decrease the allowance that a spender controls of the owner's balance by the requested delta
    ///
    /// The caller of this method is implicitly defined as the owner.
    /// If the resulting allowance would be negative, the allowance between owner and spender is set
    /// to zero. Returns an error if either the spender or owner address is unresolvable. Returns an
    /// error if requested delta is negative. Else returns the new allowance
    pub fn decrease_allowance(
        &mut self,
        owner: &Address,
        spender: &Address,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "decrease allowance delta {} cannot be negative",
                delta
            )));
        }

        let owner = expect_id(owner)?;
        let spender = self.resolve_to_id(spender)?;
        let new_allowance =
            self.state.change_allowance_by(&self.bs, owner, spender, &delta.neg())?;

        Ok(new_allowance)
    }

    /// Sets the allowance between owner and spender to 0
    pub fn revoke_allowance(&mut self, owner: &Address, spender: &Address) -> Result<()> {
        let owner = expect_id(owner)?;
        let spender = self.resolve_to_id(spender)?;
        self.state.revoke_allowance(&self.bs, owner, spender)?;

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
    /// ## Spender equals owner address
    /// If the spender is the targeted address, they are implicitly approved to burn an unlimited
    /// amount of tokens (up to their balance)
    ///
    /// ## Spender burning on behalf of owner address
    /// If the spender is burning on behalf of the owner the following preconditions
    /// must be met on top of the general burn conditions:
    /// - The spender MUST have an allowance not less than the requested value
    /// In addition to the general postconditions:
    /// - The target-spender allowance MUST decrease by the requested value
    ///
    /// If the burn operation would result in a negative balance for the owner, the burn is
    /// discarded and this method returns an error
    pub fn burn(
        &mut self,
        spender: &Address,
        owner: &Address,
        amount: &TokenAmount,
    ) -> Result<TokenAmount> {
        if amount.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "burn amount {} cannot be negative",
                amount
            )));
        }

        // owner and spender must exist to burn from
        let spender = expect_id(spender)?;
        let owner = self.resolve_to_id(owner)?;

        let new_amount = self.transaction(|state, bs| {
            if spender != owner {
                // attempt to use allowance and return early if not enough
                state.attempt_use_allowance(&bs, spender, owner, amount)?;
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
    /// - The receiver actor MUST implement a method called `tokens_received`, corresponding to the
    /// interface specified for FRC-XXX token receivers
    /// - The receiver's `tokens_received` hook MUST NOT abort
    ///
    /// Upon successful transfer:
    /// - The senders's balance MUST decrease by the requested value
    /// - The receiver's balance MUST increase by the requested value
    ///
    /// ## Spender equals owner address
    /// If the spender is the owner address, they are implicitly approved to transfer an unlimited
    /// amount of tokens (up to their balance)
    ///
    /// ## Spender transferring on behalf of owner address
    /// If the spender is transferring on behalf of the target token holder the following preconditions
    /// must be met on top of the general burn conditions:
    /// - The spender MUST have an allowance not less than the requested value
    /// In addition to the general postconditions:
    /// - The owner-spender allowance MUST decrease by the requested value
    pub fn transfer(
        &mut self,
        spender: &Address,
        owner: &Address,
        receiver: &Address,
        amount: &TokenAmount,
        data: &[u8],
    ) -> Result<()> {
        if amount.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "transfer amount {} cannot be negative",
                amount
            )));
        }

        let old_state = self.state.clone();

        // spender must be an id address
        let spender = expect_id(spender)?;
        // resolve owner and receiver
        let owner_id = self.resolve_to_id(owner)?;
        let receiver_id = self.resolve_to_id(receiver)?;

        self.transaction(|state, bs| {
            if spender != owner_id {
                // attempt to use allowance and return early if not enough
                state.attempt_use_allowance(&bs, spender, owner_id, amount)?;
            }
            state.change_balance_by(&bs, receiver_id, amount)?;
            state.change_balance_by(&bs, owner_id, &amount.neg())?;
            Ok(())
        })?;

        // flush state as re-entrant call needs to see new balances
        self.flush()?;

        // call receiver hook
        let params = TokenReceivedParams {
            sender: spender,
            from: owner_id,
            to: receiver_id,
            amount: amount.clone(),
            data: RawBytes::new(data.to_vec()),
        };
        match self.msg.send(
            receiver,
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
                        from: owner_id,
                        to: receiver_id,
                        sender: spender,
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

#[cfg(test)]
mod test {
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_ipld_encoding::RawBytes;
    use fvm_shared::address::{Address, BLS_PUB_LEN};
    use fvm_shared::econ::TokenAmount;
    use num_traits::Zero;

    use super::Token;
    use crate::receiver::types::TokenReceivedParams;
    use crate::runtime::messaging::FakeMessenger;

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
        Address::new_actor(&[])
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

    fn assert_last_msg_eq(messenger: &FakeMessenger, expected: TokenReceivedParams) {
        let last_called = messenger.last_hook.borrow().clone().unwrap();
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
        token.mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(100), &[]).unwrap();
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
        token.mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(1_000_000), &[]).unwrap();

        // balance and total supply both went up
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // cannot mint a negative amount
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(-1), &[]).unwrap_err();

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // mint zero
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::zero(), &[]).unwrap();

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: TOKEN_ACTOR.id().unwrap(),
                data: RawBytes::default(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::zero(),
            },
        );

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // mint again to same address
        token.mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(1_000_000), &[]).unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(2_000_000));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: TOKEN_ACTOR.id().unwrap(),
                data: RawBytes::default(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: TREASURY.id().unwrap(),
                amount: TokenAmount::from(1_000_000),
            },
        );

        // mint to a different address
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(1_000_000), &[]).unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(3_000_000));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: TOKEN_ACTOR.id().unwrap(),
                data: RawBytes::default(),
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
        token.mint(TOKEN_ACTOR, &secp_address, &TokenAmount::from(1_000_000), &[]).unwrap();

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: TOKEN_ACTOR.id().unwrap(),
                data: RawBytes::default(),
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
        token.mint(TOKEN_ACTOR, &bls_address, &TokenAmount::from(1_000_000), &[]).unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.balance_of(&secp_address).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(&bls_address).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(5_000_000));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: TOKEN_ACTOR.id().unwrap(),
                data: RawBytes::default(),
                from: TOKEN_ACTOR.id().unwrap(),
                to: token.get_id(&bls_address).unwrap(),
                amount: TokenAmount::from(1_000_000),
            },
        );

        // mint fails if actor address cannot be initialised
        let actor_address: Address = actor_address();
        token.mint(TOKEN_ACTOR, &actor_address, &TokenAmount::from(1_000_000), &[]).unwrap_err();
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
        token
            .mint(TOKEN_ACTOR, TREASURY, &TokenAmount::from(1_000_000), Default::default())
            .unwrap_err();

        // state remained unchanged
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::zero());
    }

    #[test]
    fn it_burns() {
        let mut token = new_token();

        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(600_000);
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &[]).unwrap();
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
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &[]).unwrap();
        token.burn(TREASURY, TREASURY, &burn_amount).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
    }

    #[test]
    fn it_transfers() {
        let mut token = new_token();

        // mint 100 for owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &[]).unwrap();
        // transfer 60 from owner -> receiver
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(60), &[]).unwrap();

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
                sender: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                amount: TokenAmount::from(60),
                to: BOB.id().unwrap(),
                data: RawBytes::default(),
            }
        );

        // cannot transfer a negative value
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(-1), &[]).unwrap_err();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // transfer zero value
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::zero(), &[]).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: BOB.id().unwrap(),
                amount: TokenAmount::zero(),
                data: RawBytes::default(),
            },
        );

        // transfer zero to self
        token.transfer(ALICE, ALICE, ALICE, &TokenAmount::zero(), &[]).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::zero(),
                data: RawBytes::default(),
            },
        );

        // transfer value to self
        token.transfer(ALICE, ALICE, ALICE, &TokenAmount::from(10), &[]).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: ALICE.id().unwrap(),
                amount: TokenAmount::from(10),
                data: RawBytes::default(),
            },
        );

        // transfer to pubkey
        let secp_address = &secp_address();
        assert_eq!(token.balance_of(secp_address).unwrap(), TokenAmount::zero());
        token.transfer(ALICE, ALICE, secp_address, &TokenAmount::from(10), &[]).unwrap();
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
                sender: ALICE.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: token.get_id(secp_address).unwrap(),
                amount: TokenAmount::from(10),
                data: RawBytes::default(),
            },
        );

        // transfer to uninitialized pubkey
        let bls_address = &bls_address();
        assert_eq!(token.balance_of(bls_address).unwrap(), TokenAmount::zero());
        token.transfer(ALICE, ALICE, bls_address, &TokenAmount::from(10), &[]).unwrap();
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
                sender: ALICE.id().unwrap(),
                to: token.get_id(bls_address).unwrap(),
                from: ALICE.id().unwrap(),
                amount: TokenAmount::from(10),
                data: RawBytes::default(),
            },
        );
    }

    #[test]
    fn it_fails_to_transfer_when_receiver_hook_aborts() {
        let mut token = new_token();

        // mint 100 for owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &[]).unwrap();
        // transfer 60 from owner -> receiver, but simulate receiver aborting the hook
        token.msg.abort_next_send();
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(60), Default::default()).unwrap_err();

        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(0));

        // transfer 60 from owner -> self, simulate receiver aborting the hook
        token.msg.abort_next_send();
        token
            .transfer(ALICE, ALICE, ALICE, &TokenAmount::from(60), Default::default())
            .unwrap_err();
        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(0));
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_balance() {
        let mut token = new_token();

        // mint 50 for the owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(50), &[]).unwrap();

        // attempt transfer 51 from owner -> receiver
        token
            .transfer(ALICE, ALICE, BOB, &TokenAmount::from(51), &[])
            .expect_err("transfer should have failed");

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
        token.mint(ALICE, ALICE, &TokenAmount::from(100), &[]).unwrap();
        // approve 100 spending allowance for spender
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(100)).unwrap();
        // spender makes transfer of 60 from owner -> receiver
        token.transfer(CAROL, ALICE, BOB, &TokenAmount::from(60), &[]).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: CAROL.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: BOB.id().unwrap(),
                amount: TokenAmount::from(60),
                data: RawBytes::default(),
            },
        );

        // verify allowance is correct
        let spender_allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(spender_allowance, TokenAmount::from(40));

        // spender makes another transfer of 40 from owner -> self
        token.transfer(CAROL, ALICE, CAROL, &TokenAmount::from(40), &[]).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::from(40));

        // check receiver hook was called with correct shape
        assert_last_msg_eq(
            &token.msg,
            TokenReceivedParams {
                sender: CAROL.id().unwrap(),
                from: ALICE.id().unwrap(),
                to: CAROL.id().unwrap(),
                amount: TokenAmount::from(40),
                data: RawBytes::default(),
            },
        );

        // verify allowance is correct
        assert_eq!(token.allowance(ALICE, CAROL).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_allows_delegated_burns() {
        let mut token = new_token();

        let mint_amount = TokenAmount::from(1_000_000);
        let approval_amount = TokenAmount::from(600_000);
        let burn_amount = TokenAmount::from(600_000);

        // mint the total amount
        token.mint(TOKEN_ACTOR, TREASURY, &mint_amount, &[]).unwrap();
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
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_allowance() {
        let mut token = new_token();

        // mint 100 for the owner
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(100), &[]).unwrap();
        // approve only 40 spending allowance for spender
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(40)).unwrap();
        // spender attempts makes transfer of 60 from owner -> receiver
        // this is within the owner's balance but not within the spender's allowance
        token.transfer(CAROL, ALICE, BOB, &TokenAmount::from(60), &[]).unwrap_err();

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
        token.mint(TOKEN_ACTOR, ALICE, &TokenAmount::from(50), &[]).unwrap();

        // allow 100 to be spent by spender
        token.increase_allowance(ALICE, BOB, &TokenAmount::from(100)).unwrap();

        // spender attempts transfer 51 from owner -> spender
        // they have enough allowance, but not enough balance
        token.transfer(BOB, ALICE, BOB, &TokenAmount::from(51), &[]).unwrap_err();

        // attempt burn 51 by spender
        token.burn(BOB, ALICE, &TokenAmount::from(51)).unwrap_err();

        // balances remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(50));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::zero());
        assert_eq!(token.allowance(ALICE, BOB).unwrap(), TokenAmount::from(100));
    }

    // TODO: test for re-entrancy bugs by implementing a MethodCaller that calls back on the token contract
}
