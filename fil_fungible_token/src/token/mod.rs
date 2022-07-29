mod state;
mod types;

use self::state::{StateError, TokenState};
use crate::method::{MethodCallError, MethodCaller};

use cid::Cid;
use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::ActorID;
use num_traits::Signed;
use std::ops::Neg;
use thiserror::Error;

/// Source of a token being sent to an address
#[derive(Debug)]
pub enum TokenSource {
    /// Tokens are coming from a mint action
    Mint,
    /// Tokens are coming from the balance of another actor
    Actor(ActorID),
}

#[derive(Error, Debug)]
pub enum TokenError {
    #[error("error in underlying state {0}")]
    State(#[from] StateError),
    #[error("invalid negative: {0}")]
    InvalidNegative(String),
    #[error("error calling receiver hook: {0}")]
    MethodCall(#[from] MethodCallError),
    #[error("receiver hook aborted when {from:?} sent {value:?} to {to:?} by {by:?} with exit code {exit_code:?}")]
    ReceiverHook {
        from: TokenSource,
        to: ActorID,
        by: ActorID,
        value: TokenAmount,
        exit_code: ExitCode,
    },
}

type Result<T> = std::result::Result<T, TokenError>;

/// Library functions that implement core FRC-??? standards
///
/// Holds injectable services to access/interface with IPLD/FVM layer.
pub struct Token<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    /// Injected blockstore. The blockstore must reference the same underlying storage under Clone
    bs: BS,
    /// Minimal interface to call methods on other actors (i.e. receiver hooks)
    mc: MC,
    /// In-memory cache of the state tree
    state: TokenState,
}

impl<BS, MC> Token<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    /// Creates a new token instance using the given blockstore and creates a new empty state tree
    ///
    /// Returns a Token handle that can be used to interact with the token state tree and the Cid
    /// of the state tree root
    pub fn new(bs: BS, mc: MC) -> Result<(Self, Cid)> {
        let init_state = TokenState::new(&bs)?;
        let cid = init_state.save(&bs)?;
        let token = Self { bs, mc, state: init_state };
        Ok((token, cid))
    }

    /// For an already initialised state tree, loads the state tree from the blockstore and returns
    /// a Token handle to interact with it
    pub fn load(bs: BS, mc: MC, state_cid: Cid) -> Result<Self> {
        let state = TokenState::load(&bs, &state_cid)?;
        Ok(Self { bs, mc, state })
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
        F: FnOnce(&mut TokenState, BS) -> Result<Res>,
    {
        let mut mutable_state = self.state.clone();
        let res = f(&mut mutable_state, self.bs.clone())?;
        // if closure didn't error, save state
        self.state = mutable_state;
        Ok(res)
    }
}

impl<BS, MC> Token<BS, MC>
where
    BS: IpldStore + Clone,
    MC: MethodCaller,
{
    /// Mints the specified value of tokens into an account
    ///
    /// The mint amount must be non-negative or the method returns an error
    pub fn mint(
        &mut self,
        minter: ActorID,
        initial_holder: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> Result<()> {
        if value.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "mint amount {} cannot be negative",
                value
            )));
        }

        let old_state = self.state.clone();

        // Increase the balance of the actor and increase total supply
        self.transaction(|state, bs| {
            state.change_balance_by(&bs, initial_holder, value)?;
            state.change_supply_by(value)?;
            Ok(())
        })?;

        // Update state so re-entrant calls see the changes
        self.flush()?;

        // Call receiver hook
        match self.mc.call_receiver_hook(minter, initial_holder, value, data) {
            Ok(receipt) => {
                // hook returned true, so we can continue
                if receipt.exit_code.is_success() {
                    Ok(())
                } else {
                    // TODO: handle missing addresses? tbd
                    self.state = old_state;
                    self.flush()?;
                    Err(TokenError::ReceiverHook {
                        from: TokenSource::Mint,
                        to: initial_holder,
                        by: minter,
                        value: value.clone(),
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
    pub fn balance_of(&self, holder: ActorID) -> Result<TokenAmount> {
        // Load the HAMT holding balances
        Ok(self.state.get_balance(&self.bs, holder)?)
    }

    /// Gets the allowance between owner and spender
    ///
    /// The allowance is the amount that the spender can transfer or burn out of the owner's account
    /// via the `transfer_from` and `burn_from` methods.
    pub fn allowance(&self, owner: ActorID, spender: ActorID) -> Result<TokenAmount> {
        let allowance = self.state.get_allowance_between(&self.bs, owner, spender)?;
        Ok(allowance)
    }

    /// Increase the allowance that a spender controls of the owner's balance by the requested delta
    ///
    /// Returns an error if requested delta is negative or there are errors in (de)serialization of
    /// state. Else returns the new allowance.
    pub fn increase_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "increase allowance delta {} cannot be negative",
                delta
            )));
        }

        let new_amount = self.state.change_allowance_by(&self.bs, owner, spender, delta)?;

        Ok(new_amount)
    }

    /// Decrease the allowance that a spender controls of the owner's balance by the requested delta
    ///
    /// If the resulting allowance would be negative, the allowance between owner and spender is set
    /// to zero. Returns an error if either the spender or owner address is unresolvable. Returns an
    /// error if requested delta is negative. Else returns the new allowance
    pub fn decrease_allowance(
        &mut self,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "decrease allowance delta {} cannot be negative",
                delta
            )));
        }

        let new_allowance =
            self.state.change_allowance_by(&self.bs, owner, spender, &delta.neg())?;

        Ok(new_allowance)
    }

    /// Sets the allowance between owner and spender to 0
    pub fn revoke_allowance(&mut self, owner: ActorID, spender: ActorID) -> Result<()> {
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
        spender: ActorID,
        owner: ActorID,
        value: &TokenAmount,
    ) -> Result<TokenAmount> {
        if value.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "burn amount {} cannot be negative",
                value
            )));
        }

        let new_amount = self.transaction(|state, bs| {
            if spender != owner {
                // attempt to use allowance and return early if not enough
                state.attempt_use_allowance(&bs, spender, owner, value)?;
            }
            // attempt to burn the requested amount
            let new_amount = state.change_balance_by(&bs, owner, &value.clone().neg())?;

            // decrease total_supply
            state.change_supply_by(&value.neg())?;
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
        spender: ActorID,
        owner: ActorID,
        receiver: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> Result<()> {
        if value.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "transfer amount {} cannot be negative",
                value
            )));
        }

        let old_state = self.state.clone();

        self.transaction(|state, bs| {
            if spender != owner {
                // attempt to use allowance and return early if not enough
                state.attempt_use_allowance(&bs, spender, owner, value)?;
            }
            state.change_balance_by(&bs, receiver, value)?;
            state.change_balance_by(&bs, owner, &value.neg())?;
            Ok(())
        })?;

        // flush state as re-entrant call needs to see new balances
        self.flush()?;

        // call receiver hook
        match self.mc.call_receiver_hook(owner, receiver, value, data) {
            Ok(receipt) => {
                // hook returned true, so we can continue
                if receipt.exit_code.is_success() {
                    Ok(())
                } else {
                    // TODO: handle missing addresses? tbd
                    // receiver hook aborted, revert state
                    self.state = old_state;
                    self.flush()?;
                    Err(TokenError::ReceiverHook {
                        from: TokenSource::Actor(owner),
                        to: receiver,
                        by: spender,
                        value: value.clone(),
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

#[cfg(test)]
mod test {
    use fvm_shared::{econ::TokenAmount, ActorID};
    use num_traits::Zero;

    use crate::{blockstore::SharedMemoryBlockstore, method::FakeMethodCaller};

    use super::Token;

    const TOKEN_ACTOR_ADDRESS: ActorID = ActorID::MAX;
    const TREASURY: ActorID = 1;
    const ALICE: ActorID = 2;
    const BOB: ActorID = 3;
    const CAROL: ActorID = 4;

    fn new_token() -> Token<SharedMemoryBlockstore, FakeMethodCaller> {
        Token::new(SharedMemoryBlockstore::new(), FakeMethodCaller::default()).unwrap().0
    }

    #[test]
    fn it_instantiates_and_persists() {
        // create a new token
        let bs = SharedMemoryBlockstore::new();
        let mc = FakeMethodCaller::default();
        let (mut token, _) = Token::new(bs.clone(), FakeMethodCaller::default()).unwrap();

        // state exists but is empty
        assert_eq!(token.total_supply(), TokenAmount::zero());

        // mint some value
        token.mint(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(100), &[]).unwrap();
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // flush token to blockstore
        let cid = token.flush().unwrap();

        // the returned cid can be used to reference the same token state
        let token2 = Token::load(bs, mc, cid).unwrap();
        assert_eq!(token2.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_provides_atomic_transactions() {
        // create a new token
        let bs = SharedMemoryBlockstore::new();
        let mc = FakeMethodCaller::default();
        let (mut token, _) = Token::new(bs, mc).unwrap();

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

        token.mint(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(1_000_000), &[]).unwrap();

        // balance and total supply both went up
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // cannot mint a negative amount
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::from(-1), &[]).unwrap_err();

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // mint zero
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::zero(), &[]).unwrap();

        // state remained unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));

        // mint again to same address
        token.mint(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(1_000_000), &[]).unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(2_000_000));

        // mint to a different address
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::from(1_000_000), &[]).unwrap();
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(2_000_000));
        assert_eq!(token.total_supply(), TokenAmount::from(3_000_000));

        // carols account was unaffected
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());
    }

    #[test]
    fn it_fails_to_mint_if_receiver_hook_aborts() {
        let mut token = new_token();

        // force hook to abort
        token
            .mint(TOKEN_ACTOR_ADDRESS, TREASURY, &TokenAmount::from(1_000_000), "abort".as_bytes())
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
        token.mint(TOKEN_ACTOR_ADDRESS, TREASURY, &mint_amount, &[]).unwrap();
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
        token.mint(TOKEN_ACTOR_ADDRESS, TREASURY, &mint_amount, &[]).unwrap();
        token.burn(TREASURY, TREASURY, &burn_amount).unwrap_err();

        // balances and supply were unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(1_000_000));
        assert_eq!(token.balance_of(TREASURY).unwrap(), TokenAmount::from(1_000_000));
    }

    #[test]
    fn it_transfers() {
        let mut token = new_token();

        // mint 100 for owner
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::from(100), &[]).unwrap();
        // transfer 60 from owner -> receiver
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(60), &[]).unwrap();

        // owner has 100 - 60 = 40
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        // receiver has 0 + 60 = 60
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

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

        // transfer zero to self
        token.transfer(ALICE, ALICE, ALICE, &TokenAmount::zero(), &[]).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // transfer value to self
        token.transfer(ALICE, ALICE, ALICE, &TokenAmount::from(10), &[]).unwrap();
        // balances are unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        // total supply is unchanged
        assert_eq!(token.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_fails_to_transfer_when_receiver_hook_aborts() {
        let mut token = new_token();

        // mint 100 for owner
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::from(100), &[]).unwrap();
        // transfer 60 from owner -> receiver, but simulate receiver aborting the hook
        token.transfer(ALICE, ALICE, BOB, &TokenAmount::from(60), "abort".as_bytes()).unwrap_err();

        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(0));

        // transfer 60 from owner -> self, simulate receiver aborting the hook
        token
            .transfer(ALICE, ALICE, ALICE, &TokenAmount::from(60), "abort".as_bytes())
            .unwrap_err();
        // balances unchanged
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(100));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(0));
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_balance() {
        let mut token = new_token();

        // mint 50 for the owner
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::from(50), &[]).unwrap();

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
    }

    #[test]
    fn it_allows_delegated_transfer() {
        let mut token = new_token();

        // mint 100 for the owner
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::from(100), &[]).unwrap();
        // approve 100 spending allowance for spender
        token.increase_allowance(ALICE, CAROL, &TokenAmount::from(100)).unwrap();
        // spender makes transfer of 60 from owner -> receiver
        token.transfer(CAROL, ALICE, BOB, &TokenAmount::from(60), &[]).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::from(40));
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::zero());

        // verify allowance is correct
        let spender_allowance = token.allowance(ALICE, CAROL).unwrap();
        assert_eq!(spender_allowance, TokenAmount::from(40));

        // spender makes another transfer of 40 from owner -> self
        token.transfer(CAROL, ALICE, CAROL, &TokenAmount::from(40), &[]).unwrap();

        // verify all balances are correct
        assert_eq!(token.balance_of(ALICE).unwrap(), TokenAmount::zero());
        assert_eq!(token.balance_of(BOB).unwrap(), TokenAmount::from(60));
        assert_eq!(token.balance_of(CAROL).unwrap(), TokenAmount::from(40));

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
        token.mint(TOKEN_ACTOR_ADDRESS, TREASURY, &mint_amount, &[]).unwrap();
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
    }

    #[test]
    fn it_fails_to_transfer_when_insufficient_allowance() {
        let mut token = new_token();

        // mint 100 for the owner
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::from(100), &[]).unwrap();
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
        token.mint(TOKEN_ACTOR_ADDRESS, ALICE, &TokenAmount::from(50), &[]).unwrap();

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
