pub mod receiver;
mod state;
mod types;

use self::state::{StateError, TokenState};
pub use self::types::*;

use cid::Cid;
use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_shared::econ::TokenAmount;
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
}

type Result<T> = std::result::Result<T, TokenError>;

/// Library functions that implement core FRC-??? standards
///
/// Holds injectable services to access/interface with IPLD/FVM layer.
pub struct Token<BS>
where
    BS: IpldStore + Clone,
{
    /// Injected blockstore. The blockstore must reference the same underlying storage under Clone
    bs: BS,
    /// Root of the token state tree
    state_cid: Cid,
}

impl<BS> Token<BS>
where
    BS: IpldStore + Clone,
{
    /// Instantiate an empty token helper with access to a blockstore and runtime
    pub fn new(bs: BS, state_cid: Cid) -> Self {
        Self { bs, state_cid }
    }

    /// Constructs the token state tree and saves it at a CID
    pub fn init_state(&mut self) -> Result<Cid> {
        let init_state = TokenState::new(&self.bs)?;
        self.save_state(&init_state)
    }

    /// Helper function that loads the root of the state tree related to token-accounting
    ///
    /// Actors can't usefully recover if state wasn't initialized (failure to call `init_state`) in
    /// the constructor so this method panics if the state tree if missing
    fn load_state(&self) -> TokenState {
        TokenState::load(&self.bs, &self.state_cid).unwrap()
    }

    /// Helper function that saves the state tree and returns the CID of the root
    fn save_state(&mut self, state: &TokenState) -> Result<Cid> {
        self.state_cid = state.save(&self.bs)?;
        Ok(self.state_cid)
    }

    /// Mints the specified value of tokens into an account
    ///
    /// The mint amount must be non-negative or the method returns an error
    pub fn mint(&mut self, initial_holder: ActorID, value: TokenAmount) -> Result<()> {
        if value.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "mint amount {} cannot be negative",
                value
            )));
        }

        // Increase the balance of the actor and increase total supply
        let mut state = self.load_state();
        state.change_balance_by(&self.bs, initial_holder, &value)?;

        // TODO: invoke the receiver hook on the initial_holder

        state.change_supply_by(&value)?;

        // Commit the state atomically if supply and balance increased
        self.save_state(&state)?;
        Ok(())
    }

    /// Gets the total number of tokens in existence
    ///
    /// This equals the sum of `balance_of` called on all addresses. This equals sum of all
    /// successful `mint` calls minus the sum of all successful `burn`/`burn_from` calls
    pub fn total_supply(&self) -> TokenAmount {
        let state = self.load_state();
        state.supply
    }

    /// Returns the balance associated with a particular address
    ///
    /// Accounts that have never received transfers implicitly have a zero-balance
    pub fn balance_of(&self, holder: ActorID) -> Result<TokenAmount> {
        // Load the HAMT holding balances
        let state = self.load_state();
        Ok(state.get_balance(&self.bs, holder)?)
    }

    /// Gets the allowance between owner and spender
    ///
    /// The allowance is the amount that the spender can transfer or burn out of the owner's account
    /// via the `transfer_from` and `burn_from` methods.
    pub fn allowance(&self, owner: ActorID, spender: ActorID) -> Result<TokenAmount> {
        let state = self.load_state();
        let allowance = state.get_allowance_between(&self.bs, owner, spender)?;
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
        delta: TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "increase allowance delta {} cannot be negative",
                delta
            )));
        }

        let mut state = self.load_state();
        let new_amount = state.change_allowance_by(&self.bs, owner, spender, &delta)?;

        self.save_state(&state)?;
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
        delta: TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "decrease allowance delta {} cannot be negative",
                delta
            )));
        }

        let mut state = self.load_state();
        let new_allowance = state.change_allowance_by(&self.bs, owner, spender, &delta.neg())?;

        self.save_state(&state)?;
        Ok(new_allowance)
    }

    /// Sets the allowance between owner and spender to 0
    pub fn revoke_allowance(&mut self, owner: ActorID, spender: ActorID) -> Result<()> {
        let mut state = self.load_state();
        state.revoke_allowance(&self.bs, owner, spender)?;

        self.save_state(&state)?;
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
        value: TokenAmount,
    ) -> Result<TokenAmount> {
        if value.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "burn amount {} cannot be negative",
                value
            )));
        }

        let mut state = self.load_state();

        if spender != owner {
            // attempt to use allowance and return early if not enough
            state.attempt_use_allowance(&self.bs, spender, owner, &value)?;
        }
        // attempt to burn the requested amount
        let new_amount = state.change_balance_by(&self.bs, owner, &value.clone().neg())?;

        // decrease total_supply
        state.change_supply_by(&value.neg())?;

        // if both succeeded, atomically commit the transaction
        self.save_state(&state)?;
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
        value: TokenAmount,
    ) -> Result<()> {
        if value.is_negative() {
            return Err(TokenError::InvalidNegative(format!(
                "transfer amount {} cannot be negative",
                value
            )));
        }

        let mut state = self.load_state();

        if spender != owner {
            // attempt to use allowance and return early if not enough
            state.attempt_use_allowance(&self.bs, spender, owner, &value)?;
        }

        // attempt to credit the receiver
        state.change_balance_by(&self.bs, receiver, &value)?;
        // attempt to debit from the sender
        state.change_balance_by(&self.bs, owner, &value.neg())?;

        // call the receiver hook
        // FIXME: use fvm_dispatch to make a standard runtime call to the receiver
        // - ensure the hook did not abort
        // - receiver hook should see the new balances...

        // if all succeeded, atomically commit the transaction
        self.save_state(&state)?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use cid::Cid;
    use fvm_shared::econ::TokenAmount;
    use num_traits::Zero;

    use crate::blockstore::SharedMemoryBlockstore;

    use super::Token;

    #[test]
    fn it_instantiates() {
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().expect("state init failed");
        token.load_state();
    }

    #[test]
    fn it_persists_loads() {
        let state_cid = Cid::default();
        let bs = SharedMemoryBlockstore::new();
        let mut token = Token::new(bs.clone(), state_cid);
        token.init_state().unwrap();

        // simulate actor calls mint method
        let treasury = 1;
        token.mint(treasury, TokenAmount::from(100)).unwrap();
        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(100));

        // here the actor would save the cid to it's state tree
        // TODO: state_cid needs to be exported so that actors can keep track of it
        let new_state_cid = token.state_cid;

        // simulate actor is invoked again: load a new instance of the Token from the blockstore
        let token_clone = Token::new(bs, new_state_cid);
        let balance_from_cloned = token_clone.balance_of(treasury).unwrap();
        // it has the balances from before
        assert_eq!(balance_from_cloned, TokenAmount::from(100));
    }

    #[test]
    fn it_mints() {
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let treasury = 1;
        token.mint(treasury, TokenAmount::from(1_000_000)).unwrap();

        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(1_000_000));

        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(1_000_000));
    }

    #[test]
    fn it_burns() {
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let treasury = 1;
        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(600_000);
        token.mint(treasury, mint_amount).unwrap();
        token.burn(treasury, treasury, burn_amount).unwrap();

        // total supply decreased
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(400_000));

        // treasury balance decreased
        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(400_000));
    }

    #[test]
    fn it_allows_delegated_burns() {
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let treasury = 1;
        let burner = 2;
        let mint_amount = TokenAmount::from(1_000_000);
        let approval_amount = TokenAmount::from(600_000);
        let burn_amount = TokenAmount::from(600_000);

        // mint the total amount
        token.mint(treasury, mint_amount).unwrap();
        // approve the burner to spend the allowance
        token
            .increase_allowance(treasury, burner, approval_amount)
            .unwrap();
        // burn the approved amount
        token.burn(burner, treasury, burn_amount.clone()).unwrap();

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
        token
            .burn(burner, treasury, burn_amount)
            .expect_err("unable to burn more than allowance");

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
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let treasury = 1;
        let mint_amount = TokenAmount::from(1_000_000);
        let burn_amount = TokenAmount::from(2_000_000);
        token.mint(treasury, mint_amount).unwrap();
        token.burn(treasury, treasury, burn_amount).unwrap_err();

        // total supply remained same
        let total_supply = token.total_supply();
        assert_eq!(total_supply, TokenAmount::from(1_000_000));

        // treasury balance remained same
        let balance = token.balance_of(treasury).unwrap();
        assert_eq!(balance, TokenAmount::from(1_000_000));
    }

    #[test]
    fn it_allows_transfer() {
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let owner = 1;
        let receiver = 2;
        // mint 100 for owner
        token.mint(owner, TokenAmount::from(100)).unwrap();
        // TODO: token needs some injectable layer to handle and mock the receive hook behaviour
        // transfer 60 from owner -> receiver
        token
            .transfer(owner, owner, receiver, TokenAmount::from(60))
            .unwrap();

        // owner has 100 - 60 = 40
        let balance = token.balance_of(owner).unwrap();
        assert_eq!(balance, TokenAmount::from(40));

        // receiver has 0 + 60 = 60
        let balance = token.balance_of(receiver).unwrap();
        assert_eq!(balance, TokenAmount::from(60));
    }

    // TODO
    // fn it_disallows_transfer_when_receiver_hook_aborts() {}

    #[test]
    fn it_disallows_transfer_when_insufficient_balance() {
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let owner = 1;
        let receiver = 2;
        // mint 50 for the owner
        token.mint(owner, TokenAmount::from(50)).unwrap();

        // attempt transfer 51 from owner -> receiver
        token
            .transfer(owner, owner, receiver, TokenAmount::from(51))
            .expect_err("transfer should have failed");

        // balances remained unchanged
        let balance = token.balance_of(owner).unwrap();
        assert_eq!(balance, TokenAmount::from(50));
        let balance = token.balance_of(receiver).unwrap();
        assert_eq!(balance, TokenAmount::zero());
    }

    #[test]
    fn it_allows_delegated_transfer() {
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let owner = 1;
        let receiver = 2;
        let spender = 3;

        // mint 100 for the owner
        token.mint(owner, TokenAmount::from(100)).unwrap();
        // approve 100 spending allowance for spender
        token
            .increase_allowance(owner, spender, TokenAmount::from(100))
            .unwrap();
        // spender makes transfer of 60 from owner -> receiver
        token
            .transfer(spender, owner, receiver, TokenAmount::from(60))
            .unwrap();

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
        token
            .transfer(spender, owner, spender, TokenAmount::from(40))
            .unwrap();

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
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let owner = 1;
        let receiver = 2;
        let spender = 3;

        // mint 100 for the owner
        token.mint(owner, TokenAmount::from(100)).unwrap();
        // approve 100 spending allowance for spender
        token
            .increase_allowance(owner, spender, TokenAmount::from(100))
            .unwrap();

        // before spending, owner decreases allowance
        token
            .decrease_allowance(owner, spender, TokenAmount::from(90))
            .unwrap();

        // spender fails to makes transfer of 60 from owner -> receiver
        token
            .transfer(spender, owner, receiver, TokenAmount::from(60))
            .unwrap_err();

        // because the allowance is only 10
        let allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(allowance, TokenAmount::from(10));

        // spender can transfer 1
        token
            .transfer(spender, owner, receiver, TokenAmount::from(1))
            .unwrap();

        let allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(allowance, TokenAmount::from(9));

        // owner revokes the rest
        token.revoke_allowance(owner, spender).unwrap();

        let allowance = token.allowance(owner, spender).unwrap();
        assert_eq!(allowance, TokenAmount::from(0));

        // spender can no longer transfer 1
        token
            .transfer(spender, owner, receiver, TokenAmount::from(1))
            .unwrap_err();

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
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let owner = 1;
        let receiver = 2;
        let spender = 3;

        // mint 100 for the owner
        token.mint(owner, TokenAmount::from(100)).unwrap();
        // approve only 40 spending allowance for spender
        token
            .increase_allowance(owner, spender, TokenAmount::from(40))
            .unwrap();
        // spender attempts makes transfer of 60 from owner -> receiver
        // this is within the owner's balance but not within the spender's allowance
        token
            .transfer(spender, owner, receiver, TokenAmount::from(60))
            .unwrap_err();

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
        let state_cid = Cid::default();
        let mut token = Token::new(SharedMemoryBlockstore::new(), state_cid);
        token.init_state().unwrap();

        let owner = 1;
        let receiver = 2;
        let spender = 3;

        token.mint(owner, TokenAmount::from(-1)).unwrap_err();
        token.burn(owner, owner, TokenAmount::from(-1)).unwrap_err();
        token
            .transfer(owner, owner, receiver, TokenAmount::from(-1))
            .unwrap_err();
        token
            .increase_allowance(owner, spender, TokenAmount::from(-1))
            .unwrap_err();
        token
            .decrease_allowance(owner, spender, TokenAmount::from(-1))
            .unwrap_err();

        // spender attempts makes transfer of 60 from owner -> receiver
        // this is within the owner's balance but not within the spender's allowance
        token
            .transfer(spender, owner, receiver, TokenAmount::from(60))
            .unwrap_err();
    }
}
