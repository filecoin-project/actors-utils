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
        from: ActorID,
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
    /// Token-contract address
    actor_id: ActorID,
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
    pub fn new(bs: BS, mc: MC, actor_id: ActorID) -> Result<(Self, Cid)> {
        let init_state = TokenState::new(&bs)?;
        let cid = init_state.save(&bs)?;
        let token = Self { bs, mc, actor_id, state: init_state };
        Ok((token, cid))
    }

    /// For an already initialised state tree, loads the state tree from the blockstore and returns
    /// a Token handle to interact with it
    pub fn load(bs: BS, mc: MC, actor_id: ActorID, state_cid: Cid) -> Result<Self> {
        let state = TokenState::load(&bs, &state_cid)?;
        Ok(Self { bs, mc, state, actor_id })
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
        match self.mc.call_receiver_hook(self.actor_id, initial_holder, value, data) {
            Ok(receipt) => {
                // hook returned true, so we can continue
                if receipt.exit_code.is_success() {
                    Ok(())
                } else {
                    // TODO: handle missing addresses? tbd
                    self.state = old_state;
                    self.flush()?;
                    Err(TokenError::ReceiverHook {
                        from: self.actor_id,
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
                        from: owner,
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

    const TOKEN_ACTOR_ADDRESS: ActorID = ActorID::max_value();

    fn new_token() -> Token<SharedMemoryBlockstore, FakeMethodCaller> {
        Token::new(SharedMemoryBlockstore::new(), FakeMethodCaller::default(), TOKEN_ACTOR_ADDRESS)
            .unwrap()
            .0
    }

    #[test]
    fn it_instantiates() {
        // create a new token
        let bs = SharedMemoryBlockstore::new();
        let mc = FakeMethodCaller::default();
        let (mut token, _) =
            Token::new(bs.clone(), FakeMethodCaller::default(), TOKEN_ACTOR_ADDRESS).unwrap();

        // state exists but is empty
        assert_eq!(token.total_supply(), TokenAmount::zero());
        token.mint(TOKEN_ACTOR_ADDRESS, 1, &TokenAmount::from(100), &[]).unwrap();
        assert_eq!(token.total_supply(), TokenAmount::from(100));

        // flush token to blockstore
        let cid = token.flush().unwrap();

        // the returned cid can be used to reference the same token state
        let token2 = Token::load(bs, mc, TOKEN_ACTOR_ADDRESS, cid).unwrap();
        assert_eq!(token2.total_supply(), TokenAmount::from(100));
    }

    #[test]
    fn it_provides_atomic_transactions() {
        // create a new token
        let bs = SharedMemoryBlockstore::new();
        let mc = FakeMethodCaller::default();
        let (mut token, _) = Token::new(bs, mc, TOKEN_ACTOR_ADDRESS).unwrap();

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
                // this makes supply negative and should rever the entire transaction
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

        let treasury = 1;
        token.mint(TOKEN_ACTOR_ADDRESS, treasury, &TokenAmount::from(1_000_000), &[]).unwrap();

        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(1_000_000));

        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(1_000_000));
    }

    #[test]
    fn it_fails_to_mint_if_receiver_hook_aborts() {
        let mut token = new_token();

        let treasury = 1;
        // force hook to abort
        token
            .mint(TOKEN_ACTOR_ADDRESS, treasury, &TokenAmount::from(1_000_000), "abort".as_bytes())
            .unwrap_err();

        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::zero());

        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::zero());
    }

    #[test]
    fn it_burns() {
        let mut token = new_token();

        let treasury = 1;
        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(600_000);
        token.mint(TOKEN_ACTOR_ADDRESS, treasury, &mint_amount, &[]).unwrap();
        token.burn(treasury, treasury, &burn_amount).unwrap();

        // total supply decreased
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(400_000));

        // treasury balance decreased
        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(400_000));
    }

    #[test]
    fn it_allows_delegated_burns() {
        let mut token = new_token();

        let treasury = 1;
        let burner = 2;
        let mint_amount = TokenAmount::from(1_000_000);
        let approval_amount = TokenAmount::from(600_000);
        let burn_amount = TokenAmount::from(600_000);

        // mint the total amount
        token.mint(TOKEN_ACTOR_ADDRESS, treasury, &mint_amount, &[]).unwrap();
        // approve the burner to spend the allowance
        token.increase_allowance(treasury, burner, &approval_amount).unwrap();
        // burn the approved amount
        token.burn(burner, treasury, &burn_amount).unwrap();

        // total supply decreased
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(400_000));
        // treasury balance decreased
        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(400_000));
        // burner approval decreased
        let new_allowance = token.allowance(treasury, burner).unwrap();
        assert_eq!(new_allowance, TokenAmount::zero());

        // disallows another delegated burn as approval is zero
        // burn the approved amount
        token.burn(burner, treasury, &burn_amount).expect_err("unable to burn more than allowance");

        // balances didn't change
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(400_000));
        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(400_000));
        let new_allowance = token.allowance(treasury, burner).unwrap();
        assert_eq!(new_allowance, TokenAmount::zero());
    }

    #[test]
    fn it_cannot_burn_below_zero() {
        let mut token = new_token();

        let treasury = 1;
        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(2_000_000);
        token.mint(TOKEN_ACTOR_ADDRESS, treasury, &mint_amount, &[]).unwrap();
        token.burn(treasury, treasury, &burn_amount).unwrap_err();

        // total supply remained same
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(1_000_000));

        // treasury balance remained same
        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(1_000_000));
    }

    #[test]
    fn it_allows_transfer() {
        let mut token = new_token();

        let owner = 1;
        let receiver = 2;
        // mint 100 for owner
        token.mint(TOKEN_ACTOR_ADDRESS, owner, &TokenAmount::from(100), &[]).unwrap();
        // transfer 60 from owner -> receiver
        token.transfer(owner, owner, receiver, &TokenAmount::from(60), &[]).unwrap();

        // owner has 100 - 60 = 40
        let balance = token.balance_of(owner).unwrap();
        assert_eq!(balance, TokenAmount::from(40));

        // receiver has 0 + 60 = 60
        let balance = token.balance_of(receiver).unwrap();
        assert_eq!(balance, TokenAmount::from(60));
    }

    #[test]
    fn it_disallows_transfer_when_receiver_hook_aborts() {
        let mut token = new_token();

        let owner = 1;
        let receiver = 2;
        // mint 100 for owner
        token.mint(TOKEN_ACTOR_ADDRESS, owner, &TokenAmount::from(100), &[]).unwrap();
        // transfer 60 from owner -> receiver, but simulate receiver aborting the hook
        token
            .transfer(owner, owner, receiver, &TokenAmount::from(60), "abort".as_bytes())
            .unwrap_err();

        // balances didn't change
        let balance = token.balance_of(owner).unwrap();
        assert_eq!(balance, TokenAmount::from(100));
        let balance = token.balance_of(receiver).unwrap();
        assert_eq!(balance, TokenAmount::from(0));
    }

    #[test]
    fn it_disallows_transfer_when_insufficient_balance() {
        let mut token = new_token();

        let owner = 1;
        let receiver = 2;
        // mint 50 for the owner
        token.mint(TOKEN_ACTOR_ADDRESS, owner, &TokenAmount::from(50), &[]).unwrap();

        // attempt transfer 51 from owner -> receiver
        token
            .transfer(owner, owner, receiver, &TokenAmount::from(51), &[])
            .expect_err("transfer should have failed");

        // balances remained unchanged
        let balance = token.balance_of(owner).unwrap();
        assert_eq!(balance, TokenAmount::from(50));
        let balance = token.balance_of(receiver).unwrap();
        assert_eq!(balance, TokenAmount::zero());
    }

    #[test]
    fn it_doesnt_use_allowance_when_insufficent_balance() {
        let mut token = new_token();

        let owner = 1;
        let spender = 2;
        // mint 50 for the owner
        token.mint(TOKEN_ACTOR_ADDRESS, owner, &TokenAmount::from(50), &[]).unwrap();

        // allow 100 to be spent by spender
        token.increase_allowance(owner, spender, &TokenAmount::from(100)).unwrap();

        // spender attempts transfer 51 from owner -> spender
        // they have enough allowance, but not enough balance
        token.transfer(spender, owner, spender, &TokenAmount::from(51), &[]).unwrap_err();

        // attempt burn 51 by spender
        token.burn(spender, owner, &TokenAmount::from(51)).unwrap_err();

        // balances remained unchanged
        let balance = token.balance_of(owner).unwrap();
        assert_eq!(balance, TokenAmount::from(50));
        let balance = token.balance_of(spender).unwrap();
        assert_eq!(balance, TokenAmount::zero());
        let allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(allowance, TokenAmount::from(100));
    }

    #[test]
    fn it_allows_delegated_transfer() {
        let mut token = new_token();

        let owner = 1;
        let receiver = 2;
        let spender = 3;

        // mint 100 for the owner
        token.mint(TOKEN_ACTOR_ADDRESS, owner, &TokenAmount::from(100), &[]).unwrap();
        // approve 100 spending allowance for spender
        token.increase_allowance(owner, spender, &TokenAmount::from(100)).unwrap();
        // spender makes transfer of 60 from owner -> receiver
        token.transfer(spender, owner, receiver, &TokenAmount::from(60), &[]).unwrap();

        // verify all balances are correct
        let owner_balance = token.balance_of(owner).unwrap();
        let receiver_balance = token.balance_of(receiver).unwrap();
        let spender_balance = token.balance_of(spender).unwrap();
        assert_eq!(owner_balance, TokenAmount::from(40));
        assert_eq!(receiver_balance, TokenAmount::from(60));
        assert_eq!(spender_balance, TokenAmount::zero());

        // verify allowance is correct
        let spender_allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(spender_allowance, TokenAmount::from(40));

        // spender makes another transfer of 40 from owner -> self
        token.transfer(spender, owner, spender, &TokenAmount::from(40), &[]).unwrap();

        // verify all balances are correct
        let owner_balance = token.balance_of(owner).unwrap();
        let receiver_balance = token.balance_of(receiver).unwrap();
        let spender_balance = token.balance_of(spender).unwrap();
        assert_eq!(owner_balance, TokenAmount::zero());
        assert_eq!(receiver_balance, TokenAmount::from(60));
        assert_eq!(spender_balance, TokenAmount::from(40));

        // verify allowance is correct
        let spender_allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(spender_allowance, TokenAmount::zero());
    }

    #[test]
    fn it_allows_revoking_allowances() {
        let mut token = new_token();

        let owner = 1;
        let receiver = 2;
        let spender = 3;

        // mint 100 for the owner
        token.mint(TOKEN_ACTOR_ADDRESS, owner, &TokenAmount::from(100), &[]).unwrap();
        // approve 100 spending allowance for spender
        token.increase_allowance(owner, spender, &TokenAmount::from(100)).unwrap();

        // before spending, owner decreases allowance
        token.decrease_allowance(owner, spender, &TokenAmount::from(90)).unwrap();

        // spender fails to makes transfer of 60 from owner -> receiver
        token.transfer(spender, owner, receiver, &TokenAmount::from(60), &[]).unwrap_err();

        // because the allowance is only 10
        let allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(allowance, TokenAmount::from(10));

        // spender can transfer 1
        token.transfer(spender, owner, receiver, &TokenAmount::from(1), &[]).unwrap();

        let allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(allowance, TokenAmount::from(9));

        // owner revokes the rest
        token.revoke_allowance(owner, spender).unwrap();

        let allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(allowance, TokenAmount::from(0));

        // spender can no longer transfer 1
        token.transfer(spender, owner, receiver, &TokenAmount::from(1), &[]).unwrap_err();

        // only the 1 token transfer should have succeeded
        // verify all balances are correct
        let owner_balance = token.balance_of(owner).unwrap();
        let receiver_balance = token.balance_of(receiver).unwrap();
        let spender_balance = token.balance_of(spender).unwrap();
        assert_eq!(owner_balance, TokenAmount::from(99));
        assert_eq!(receiver_balance, TokenAmount::from(1));
        assert_eq!(spender_balance, TokenAmount::from(0));
    }

    #[test]
    fn it_disallows_transfer_when_insufficient_allowance() {
        let mut token = new_token();

        let owner = 1;
        let receiver = 2;
        let spender = 3;

        // mint 100 for the owner
        token.mint(TOKEN_ACTOR_ADDRESS, owner, &TokenAmount::from(100), &[]).unwrap();
        // approve only 40 spending allowance for spender
        token.increase_allowance(owner, spender, &TokenAmount::from(40)).unwrap();
        // spender attempts makes transfer of 60 from owner -> receiver
        // this is within the owner's balance but not within the spender's allowance
        token.transfer(spender, owner, receiver, &TokenAmount::from(60), &[]).unwrap_err();

        // verify all balances are correct
        let owner_balance = token.balance_of(owner).unwrap();
        let receiver_balance = token.balance_of(receiver).unwrap();
        let spender_balance = token.balance_of(spender).unwrap();
        assert_eq!(owner_balance, TokenAmount::from(100));
        assert_eq!(receiver_balance, TokenAmount::zero());
        assert_eq!(spender_balance, TokenAmount::zero());

        // verify allowance was not spent
        let spender_allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(spender_allowance, TokenAmount::from(40));
    }

    #[test]
    fn it_checks_for_invalid_negatives() {
        let mut token = new_token();

        let owner = 1;
        let receiver = 2;
        let spender = 3;

        token.mint(TOKEN_ACTOR_ADDRESS, owner, &TokenAmount::from(-1), &[]).unwrap_err();
        token.burn(owner, owner, &TokenAmount::from(-1)).unwrap_err();
        token.transfer(owner, owner, receiver, &TokenAmount::from(-1), &[]).unwrap_err();
        token.increase_allowance(owner, spender, &TokenAmount::from(-1)).unwrap_err();
        token.decrease_allowance(owner, spender, &TokenAmount::from(-1)).unwrap_err();

        // spender attempts makes transfer of 60 from owner -> receiver
        // this is within the owner's balance but not within the spender's allowance
        token.transfer(spender, owner, receiver, &TokenAmount::from(60), &[]).unwrap_err();
    }

    // TODO: test for re-entrancy bugs by implementing a MethodCaller that calls back on the token contract
}
