pub mod errors;
pub mod receiver;
mod state;
mod types;
use std::ops::Neg;

use self::state::TokenState;
pub use self::types::*;

use anyhow::bail;
use anyhow::Ok;
use anyhow::Result;
use cid::Cid;
use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;
use num_traits::Signed;

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
    /// Instantiate a token helper with access to a blockstore and runtime
    pub fn new(bs: BS, token_state: Cid) -> Self {
        Self {
            bs,
            state_cid: token_state,
        }
    }

    /// Constructs the token state tree and saves it at a CID
    pub fn init_state(&self) -> Result<Cid> {
        let init_state = TokenState::new(&self.bs)?;
        init_state.save(&self.bs)
    }

    /// Helper function that loads the root of the state tree related to token-accounting
    ///
    /// Actors can't usefully recover if state wasn't initialized (failure to call `init_state`) in
    /// the constructor so this method panics if the state tree if missing
    fn load_state(&self) -> TokenState {
        TokenState::load(&self.bs, &self.state_cid).unwrap()
    }

    /// Mints the specified value of tokens into an account
    ///
    /// If the total supply or account balance overflows, this method returns an error. The mint
    /// amount must be non-negative or the method returns an error.
    pub fn mint(&self, initial_holder: ActorID, value: TokenAmount) -> Result<()> {
        if value.is_negative() {
            bail!("value of mint was negative {}", value);
        }

        // Increase the balance of the actor and increase total supply
        let mut state = self.load_state();
        state.change_balance_by(&self.bs, initial_holder, &value)?;
        state.increase_supply(&value)?;

        // Commit the state atomically if supply and balance increased
        state.save(&self.bs)?;

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
        state.get_balance(&self.bs, holder)
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

    /// Changes the allowance that

    /// Increase the allowance that a spender controls of the owner's balance by the requested delta
    ///
    /// Returns an error if requested delta is negative or there are errors in (de)sereliazation of
    /// state. Else returns the new allowance.
    pub fn increase_allowance(
        &self,
        owner: ActorID,
        spender: ActorID,
        delta: TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_negative() {
            bail!("value of delta was negative {}", delta);
        }

        let mut state = self.load_state();
        let new_amount = state.change_allowance_by(&self.bs, owner, spender, &delta)?;
        state.save(&self.bs)?;

        Ok(new_amount)
    }

    /// Decrease the allowance that a spender controls of the owner's balance by the requested delta
    ///
    /// If the resulting allowance would be negative, the allowance between owner and spender is set
    /// to zero. Returns an error if either the spender or owner address is unresolvable. Returns an
    /// error if requested delta is negative. Else returns the new allowance
    pub fn decrease_allowance(
        &self,
        owner: ActorID,
        spender: ActorID,
        delta: TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_negative() {
            bail!("value of delta was negative {}", delta);
        }

        let mut state = self.load_state();
        let new_allowance = state.change_allowance_by(&self.bs, owner, spender, &delta.neg())?;
        state.save(&self.bs)?;

        Ok(new_allowance)
    }

    /// Sets the allowance between owner and spender to 0
    pub fn revoke_allowance(&self, owner: ActorID, spender: ActorID) -> Result<()> {
        let mut state = self.load_state();
        state.revoke_allowance(&self.bs, owner, spender)?;
        state.save(&self.bs)?;
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
        &self,
        spender: ActorID,
        owner: ActorID,
        value: TokenAmount,
    ) -> Result<TokenAmount> {
        if value.is_negative() {
            bail!("cannot burn a negative amount");
        }

        let mut state = self.load_state();

        if spender != owner {
            // attempt to use allowance and return early if not enough
            state.attempt_use_allowance(&self.bs, spender, owner, &value)?;
        }
        // attempt to burn the requested amount
        let new_amount = state.change_balance_by(&self.bs, owner, &value.neg())?;

        // if both succeeded, atomically commit the transaction
        state.save(&self.bs)?;
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
        &self,
        spender: ActorID,
        owner: ActorID,
        receiver: ActorID,
        value: TokenAmount,
    ) -> Result<()> {
        if value.is_negative() {
            bail!("cannot transfer a negative amount");
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
        state.save(&self.bs)?;

        Ok(())
    }
}
