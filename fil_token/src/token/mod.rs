pub mod errors;
pub mod receiver;
pub mod state;
mod types;
use self::errors::ActorError;
use self::state::TokenState;
pub use self::types::*;
use crate::runtime::Runtime;

use anyhow::bail;
use anyhow::Ok;
use anyhow::Result;
use cid::Cid;
use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::bigint::BigInt;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;

/// Library functions that implement core FRC-??? standards
///
/// Holds injectable services to access/interface with IPLD/FVM layer.
pub struct TokenHelper<BS, FVM>
where
    BS: IpldStore + Copy,
    FVM: Runtime,
{
    /// Injected blockstore
    bs: BS,
    /// Access to the runtime
    _runtime: FVM,
    /// Root of the token state tree
    token_state: Cid,
}

impl<BS, FVM> TokenHelper<BS, FVM>
where
    BS: IpldStore + Copy,
    FVM: Runtime,
{
    /// Instantiate a token helper with access to a blockstore and runtime
    pub fn new(bs: BS, runtime: FVM, token_state: Cid) -> Self {
        Self {
            bs,
            _runtime: runtime,
            token_state,
        }
    }

    /// Constructs the token state tree and saves it at a CID
    pub fn init_state(&self) -> Result<Cid> {
        let init_state = TokenState::new(&self.bs)?;
        init_state.save(&self.bs)
    }

    /// Helper function that loads the root of the state tree related to token-accounting
    fn load_state(&self) -> Result<TokenState> {
        TokenState::load(&self.bs, &self.token_state)
    }

    /// Mints the specified value of tokens into an account
    ///
    /// If the total supply or account balance overflows, this method returns an error. The mint
    /// amount must be non-negative or the method returns an error.
    pub fn mint(&self, initial_holder: ActorID, value: TokenAmount) -> Result<()> {
        if value.lt(&TokenAmount::zero()) {
            bail!("value of mint was negative {}", value);
        }

        // Increase the balance of the actor and increase total supply
        let mut state = self.load_state()?;
        state.increase_balance(&self.bs, initial_holder, &value)?;
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
        let state = self.load_state().unwrap();
        state.supply
    }

    /// Returns the balance associated with a particular address
    ///
    /// Accounts that have never received transfers implicitly have a zero-balance
    pub fn balance_of(&self, holder: ActorID) -> Result<TokenAmount> {
        // Load the HAMT holding balances
        let state = self.load_state()?;
        state.get_balance(&self.bs, holder)
    }

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
        if delta.lt(&TokenAmount::zero()) {
            bail!("value of allowance increase was negative {}", delta);
        }

        // Retrieve the HAMT holding balances
        let state = self.load_state()?;
        let mut owner_allowances = state.get_actor_allowance_map(&self.bs, owner)?;

        let new_amount = match owner_allowances.get(&spender)? {
            // Allowance exists - attempt to calculate new allowance
            Some(existing_allowance) => match existing_allowance.0.checked_add(&delta) {
                Some(new_allowance) => {
                    owner_allowances.set(spender, BigIntDe(new_allowance.clone()))?;
                    new_allowance
                }
                None => bail!(ActorError::Arithmetic(format!(
                    "allowance overflowed attempting to add {} to existing allowance of {} between {} {}",
                    delta, existing_allowance.0, owner, spender
                ))),
            },
            // No allowance recorded previously
            None => {
                owner_allowances.set(spender, BigIntDe(delta.clone()))?;
                delta
            }
        };

        state.save(&self.bs)?;

        Ok(new_amount)
    }

    /// Decrease the allowance that a spender controls of the owner's balance by the requested delta
    ///
    /// If the resulting allowance would be negative, the allowance between owner and spender is set
    /// to zero. If resulting allowance is zero, the entry is removed from the state map. Returns an
    /// error if either the spender or owner address is unresolvable. Returns an error if requested
    /// delta is negative. Else returns the new allowance
    pub fn decrease_allowance(
        &self,
        owner: ActorID,
        spender: ActorID,
        delta: TokenAmount,
    ) -> Result<TokenAmount> {
        // Load the HAMT holding balances
        let state = self.load_state()?;

        // TODO: replace this with a higher level abstraction where you can call
        // state.get_allowance (owner, spender)
        let mut allowances_map = state.get_actor_allowance_map(&self.bs, owner)?;

        let new_allowance = match allowances_map.get(&spender)? {
            Some(existing_allowance) => {
                let new_allowance = existing_allowance
                    .0
                    .checked_sub(&delta)
                    .unwrap() // Unwrap should be safe as allowance always > 0
                    .max(BigInt::zero());
                allowances_map.set(spender, BigIntDe(new_allowance.clone()))?;
                new_allowance
            }
            None => {
                // Can't decrease non-existent allowance
                return Ok(TokenAmount::zero());
            }
        };

        state.save(&self.bs)?;

        Ok(new_allowance)
    }

    /// Sets the allowance between owner and spender to 0
    pub fn revoke_allowance(&self, owner: ActorID, spender: ActorID) -> Result<()> {
        // Load the HAMT holding balances
        let state = self.load_state()?;
        let mut allowances_map = state.get_actor_allowance_map(&self.bs, owner)?;
        let new_allowance = TokenAmount::zero();
        allowances_map.set(spender, BigIntDe(new_allowance.clone()))?;

        state.save(&self.bs)?;
        Ok(())
    }

    /// Gets the allowance between owner and spender
    ///
    /// The allowance is the amount that the spender can transfer or burn out of the owner's account
    /// via the `transfer_from` and `burn_from` methods.
    pub fn allowance(&self, owner: ActorID, spender: ActorID) -> Result<TokenAmount> {
        // Load the HAMT holding balances
        let state = self.load_state()?;

        let owner_allowances_map = state.get_actor_allowance_map(&self.bs, owner)?;
        let allowance = match owner_allowances_map.get(&spender)? {
            Some(allowance) => allowance.0.clone(),
            None => TokenAmount::zero(),
        };

        Ok(allowance)
    }

    /// Burns an amount of token from the specified address, decreasing total token supply
    ///
    /// ## For all burn operations
    /// Preconditions:
    /// - The requested value MUST be non-negative
    /// - The requested value MUST NOT exceed the target's balance
    ///
    /// Postconditions:
    /// - The target's balance MUST decrease by the requested value
    /// - The total_supply MUST decrease by the requested value
    ///
    /// ## Operator equals target address
    /// If the operator is the targeted address, they are implicitly approved to burn an unlimited
    /// amount of tokens (up to their balance)
    ///
    /// ## Operator burning on behalf of target address
    /// If the operator is burning on behalf of the target token holder the following preconditions
    /// must be met on top of the general burn conditions:
    /// - The operator MUST have an allowance not less than the requested value
    /// In addition to the general postconditions:
    /// - The target-operator allowance MUST decrease by the requested value
    ///
    /// If the burn operation would result in a negative balance for the targeted address, the burn
    /// is discarded and this method returns an error
    pub fn burn(
        &self,
        operator: ActorID,
        target: ActorID,
        value: TokenAmount,
    ) -> Result<TokenAmount> {
        if value.lt(&TokenAmount::zero()) {
            bail!("Cannot burn a negative amount");
        }

        let state = self.load_state()?;

        if operator != target {
            // attempt to use allowance and return early if not enough
            state.attempt_use_allowance(&self.bs, operator, target, &value)?;
        }
        // attempt to burn the requested amount
        let new_amount = state.attempt_burn(&self.bs, target, &value)?;

        state.save(&self.bs)?;

        Ok(new_amount)
    }

    pub fn transfer(&self, _params: TransferParams) -> Result<TransferReturn> {
        todo!()
    }
}
