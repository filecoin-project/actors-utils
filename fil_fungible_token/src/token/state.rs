use anyhow::anyhow;
use anyhow::bail;
use anyhow::Result;
use cid::multihash::Code;
use cid::Cid;

use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::Cbor;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::Hamt;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;

const HAMT_BIT_WIDTH: u32 = 5;

/// Token state IPLD structure
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Clone, Debug)]
pub struct TokenState {
    /// Total supply of token
    #[serde(with = "bigint_ser")]
    pub supply: TokenAmount,

    /// Map<ActorId, TokenAmount> of balances as a Hamt
    pub balances: Cid,
    /// Map<ActorId, Map<ActorId, TokenAmount>> as a Hamt. Allowances are stored balances[owner][spender]
    pub allowances: Cid,
}

/// An abstraction over the IPLD layer to get and modify token state without dealing with HAMTs etc.
///
/// This is a simple wrapper of state and in general does not account for token protocol level
/// checks such as ensuring necessary approvals are enforced during transfers. This is left for the
/// caller to handle. However, some invariants such as enforcing non-negative balances, allowances
/// and total supply are enforced. Furthermore, this layer returns errors if any of the underlying
/// arithmetic overflows.
///
/// Some methods on TokenState require the caller to pass in a blockstore implementing the Clone
/// trait. It is assumed that when cloning the blockstore implementation does a "shallow-clone"
/// of the blockstore and provides access to the same underlying data.
impl TokenState {
    /// Create a new token state-tree, without committing it to a blockstore
    pub fn new<BS: IpldStore>(store: &BS) -> Result<Self> {
        let empty_balance_map = Hamt::<_, ()>::new_with_bit_width(store, HAMT_BIT_WIDTH).flush()?;
        let empty_allowances_map =
            Hamt::<_, ()>::new_with_bit_width(store, HAMT_BIT_WIDTH).flush()?;

        Ok(Self {
            supply: Default::default(),
            balances: empty_balance_map,
            allowances: empty_allowances_map,
        })
    }

    /// Loads a fresh copy of the state from a blockstore from a given cid
    pub fn load<BS: IpldStore>(bs: &BS, cid: &Cid) -> Result<Self> {
        // Load the actor state from the state tree.
        match bs.get_cbor::<Self>(cid) {
            Ok(Some(state)) => Ok(state),
            Ok(None) => Err(anyhow!("No state at this cid {:?}", cid)),
            Err(err) => Err(anyhow!("failed to get state: {}", err)),
        }
    }

    /// Saves the current state to the blockstore, returning the cid
    /// TODO: should replaced with more targeted saving of different branches of the state tree for efficiency
    /// i.e. only save the balances HAMT if it has changed, only save the allowance HAMTs if they have changed
    pub fn save<BS: IpldStore>(&self, bs: &BS) -> Result<Cid> {
        let serialized = match fvm_ipld_encoding::to_vec(self) {
            Ok(s) => s,
            Err(err) => return Err(anyhow!("failed to serialize state: {:?}", err)),
        };
        let block = Block {
            codec: DAG_CBOR,
            data: serialized,
        };
        let cid = match bs.put(Code::Blake2b256, &block) {
            Ok(cid) => cid,
            Err(err) => return Err(anyhow!("failed to store initial state: {:}", err)),
        };
        Ok(cid)
    }

    /// Get the balance of an ActorID from the currently stored state
    pub fn get_balance<BS: IpldStore + Clone>(
        &self,
        bs: &BS,
        owner: ActorID,
    ) -> Result<TokenAmount> {
        let balances = self.get_balance_map(bs)?;

        let balance = match balances.get(&owner)? {
            Some(amount) => amount.0.clone(),
            None => TokenAmount::zero(),
        };

        Ok(balance)
    }

    /// Attempts to increase the balance of the specified account by the value
    ///
    /// Caller must ensure the requested amount is non-negative.
    /// Returns an error if the balance overflows, else returns the new balance
    pub fn increase_balance<BS: IpldStore + Clone>(
        &mut self,
        bs: &BS,
        actor: ActorID,
        value: &TokenAmount,
    ) -> Result<TokenAmount> {
        let mut balance_map = self.get_balance_map(bs)?;
        let balance = balance_map.get(&actor)?;

        // calculate the new balance
        let new_balance = match balance {
            Some(existing_amount) => {
                let existing_amount = existing_amount.clone().0;
                existing_amount.checked_add(value).ok_or_else(|| {
                    anyhow!(
                        "Overflow when adding {} to {}'s balance of {}",
                        value,
                        actor,
                        existing_amount
                    )
                })?
            }
            None => value.clone(),
        };

        balance_map.set(actor, BigIntDe(new_balance.clone()))?;
        self.balances = balance_map.flush()?;
        let serialised = match fvm_ipld_encoding::to_vec(&balance_map) {
            Ok(s) => s,
            Err(err) => return Err(anyhow!("failed to serialize state: {:?}", err)),
        };
        bs.put_keyed(&self.balances, &serialised)?;
        Ok(new_balance)
    }

    /// Attempts to decrease the balance of the specified account by the value
    ///
    /// Caller must ensure the requested amount is non-negative.
    /// Returns an error if the balance overflows, or if resulting balance would be negative.
    /// Else returns the new balance
    pub fn decrease_balance<BS: IpldStore + Clone>(
        &mut self,
        bs: &BS,
        actor: ActorID,
        value: &TokenAmount,
    ) -> Result<TokenAmount> {
        let mut balance_map = self.get_balance_map(bs)?;
        let balance = balance_map.get(&actor)?;

        if balance.is_none() {
            bail!(
                "Balance would be negative after subtracting {} from {}'s balance of {}",
                value,
                actor,
                TokenAmount::zero()
            );
        }

        let existing_amount = balance.unwrap().clone().0;
        let new_amount = existing_amount.checked_sub(value).ok_or_else(|| {
            anyhow!(
                "Overflow when subtracting {} from {}'s balance of {}",
                value,
                actor,
                existing_amount
            )
        })?;

        if new_amount.lt(&TokenAmount::zero()) {
            bail!(
                "Balance would be negative after subtracting {} from {}'s balance of {}",
                value,
                actor,
                existing_amount
            );
        }

        balance_map.set(actor, BigIntDe(new_amount.clone()))?;
        self.balances = balance_map.flush()?;

        let serialised = match fvm_ipld_encoding::to_vec(&balance_map) {
            Ok(s) => s,
            Err(err) => return Err(anyhow!("failed to serialize state: {:?}", err)),
        };
        bs.put_keyed(&self.balances, &serialised)?;

        Ok(new_amount)
    }

    /// Retrieve the balance map as a HAMT
    fn get_balance_map<BS: IpldStore + Clone>(
        &self,
        bs: &BS,
    ) -> Result<Hamt<BS, BigIntDe, ActorID>> {
        match Hamt::<BS, BigIntDe, ActorID>::load(&self.balances, (*bs).clone()) {
            Ok(map) => Ok(map),
            Err(err) => return Err(anyhow!("Failed to load balances hamt: {:?}", err)),
        }
    }

    /// Increase the total supply by the specified value
    ///
    /// The requested amount must be non-negative.
    /// Returns an error if the total supply overflows, else returns the new total supply
    pub fn increase_supply(&mut self, value: &TokenAmount) -> Result<TokenAmount> {
        let new_supply = self.supply.checked_add(value).ok_or_else(|| {
            anyhow!(
                "Overflow when adding {} to the total_supply of {}",
                value,
                self.supply
            )
        })?;
        self.supply = new_supply.clone();
        Ok(new_supply)
    }

    /// Get the allowance that an owner has approved for a spender
    ///
    /// If an existing allowance cannot be found, it is implicitly assumed to be zero
    pub fn get_allowance_between<BS: IpldStore + Clone>(
        &self,
        bs: &BS,
        owner: ActorID,
        spender: ActorID,
    ) -> Result<TokenAmount> {
        let owner_allowances = self.get_owner_allowance_map(bs, owner)?;
        match owner_allowances {
            Some(hamt) => {
                let maybe_allowance = hamt.get(&spender)?;
                if let Some(allowance) = maybe_allowance {
                    return Ok(allowance.clone().0);
                }
                Ok(TokenAmount::zero())
            }
            None => Ok(TokenAmount::zero()),
        }
    }

    /// Increase the allowance between owner and spender by the specified value
    ///
    /// Caller must ensure that value is non-negative.
    pub fn increase_allowance<BS: IpldStore + Clone>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        spender: ActorID,
        value: &TokenAmount,
    ) -> Result<TokenAmount> {
        if value.is_zero() {
            // This is a no-op as far as mutating state
            return self.get_allowance_between(bs, owner, spender);
        }

        let allowance_map = self.get_owner_allowance_map(bs, owner)?;

        // If allowance map exists, modify or insert the allowance
        if let Some(mut hamt) = allowance_map {
            let previous_allowance = hamt.get(&spender)?;

            // Calculate the new allowance
            let new_allowance = match previous_allowance {
                Some(prev_allowance) => prev_allowance.0.checked_add(value).ok_or_else(|| {
                    anyhow!(
                        "Overflow when adding {} to {}'s allowance of {}",
                        value,
                        spender,
                        prev_allowance.0
                    )
                })?,
                None => value.clone(),
            };

            hamt.set(spender, BigIntDe(new_allowance.clone()))?;

            {
                // TODO: helper functions for saving hamts?, this can probably be done more efficiently rather than
                // getting the root allowance map again, by abstracting the nested hamt structure
                let new_cid = hamt.flush()?;
                let mut root_allowance_map = self.get_allowances_map(bs)?;
                root_allowance_map.set(owner, new_cid)?;
                let new_cid = root_allowance_map.flush();
                self.allowances = new_cid?;
            }

            return Ok(new_allowance);
        }

        // If allowance map does not exist, create it and insert the allowance
        let mut owner_allowances =
            Hamt::<BS, BigIntDe, ActorID>::new_with_bit_width((*bs).clone(), HAMT_BIT_WIDTH);
        owner_allowances.set(spender, BigIntDe(value.clone()))?;

        {
            // TODO: helper functions for saving hamts?, this can probably be done more efficiently rather than
            // getting the root allowance map again, by abstracting the nested hamt structure
            let mut root_allowance_map = self.get_allowances_map(bs)?;
            root_allowance_map.set(owner, owner_allowances.flush()?)?;
            self.allowances = root_allowance_map.flush()?;
        }

        Ok((*value).clone())
    }

    /// Decrease the allowance between owner and spender by the specified value. If the resulting
    /// allowance is negative, it is set to zero.
    ///
    /// Caller must ensure that value is non-negative.
    ///
    /// If the allowance is decreased to zero, the entry is removed from the map.
    /// If the map is empty, it is removed from the root map.
    pub fn decrease_allowance<BS: IpldStore + Clone>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        spender: ActorID,
        value: &TokenAmount,
    ) -> Result<TokenAmount> {
        if value.is_zero() {
            // This is a no-op as far as mutating state
            return self.get_allowance_between(bs, owner, spender);
        }

        let allowance_map = self.get_owner_allowance_map(bs, owner)?;

        // If allowance map exists, modify or insert the allowance
        if let Some(mut hamt) = allowance_map {
            let previous_allowance = hamt.get(&spender)?;

            // Calculate the new allowance, and max with zero
            let new_allowance = match previous_allowance {
                Some(prev_allowance) => prev_allowance.0.checked_sub(value).ok_or_else(|| {
                    anyhow!(
                        "Overflow when adding {} to {}'s allowance of {}",
                        value,
                        spender,
                        prev_allowance.0
                    )
                })?,
                None => value.clone(),
            }
            .max(TokenAmount::zero());

            // Update the Hamts
            let mut root_allowance_map = self.get_allowances_map(bs)?;

            if new_allowance.is_zero() {
                hamt.delete(&spender)?;

                if hamt.is_empty() {
                    root_allowance_map.delete(&owner)?;
                } else {
                    root_allowance_map.set(owner, hamt.flush()?)?;
                }

                self.allowances = root_allowance_map.flush()?;
                return Ok(TokenAmount::zero());
            }

            hamt.set(spender, BigIntDe(new_allowance.clone()))?;
            {
                // TODO: helper functions for saving hamts?, this can probably be done more efficiently rather than
                // getting the root allowance map again, by abstracting the nested hamt structure
                root_allowance_map.set(owner, hamt.flush()?)?;
                self.allowances = root_allowance_map.flush()?;
            }

            return Ok(new_allowance);
        }

        // If allowance map does not exist, decreasing is a no-op
        Ok(TokenAmount::zero())
    }

    /// Revokes an approved allowance by removing the entry from the owner-spender map
    ///
    /// If that map becomes empty, it is removed from the root map.
    pub fn revoke_allowance<BS: IpldStore + Clone>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        spender: ActorID,
    ) -> Result<()> {
        let allowance_map = self.get_owner_allowance_map(bs, owner)?;
        if let Some(mut map) = allowance_map {
            map.delete(&spender)?;
            if map.is_empty() {
                let mut root_allowance_map = self.get_allowances_map(bs)?;
                root_allowance_map.delete(&owner)?;
                self.allowances = root_allowance_map.flush()?;
            } else {
                let new_cid = map.flush()?;
                let mut root_allowance_map = self.get_allowances_map(bs)?;
                root_allowance_map.set(owner, new_cid)?;
                self.allowances = root_allowance_map.flush()?;
            }
        }

        Ok(())
    }

    /// Atomically checks if value is less than the allowance and deducts it if so
    ///
    /// Returns new allowance if successful, else returns an error and the allowance is unchanged
    pub fn attempt_use_allowance<BS: IpldStore + Clone>(
        &mut self,
        bs: &BS,
        spender: u64,
        owner: u64,
        value: &TokenAmount,
    ) -> Result<TokenAmount> {
        let current_allowance = self.get_allowance_between(bs, owner, spender)?;

        if value.is_zero() {
            return Ok(current_allowance);
        }

        let new_allowance = current_allowance.checked_sub(value).ok_or_else(|| {
            anyhow!(
                "Overflow when subtracting {} from {}'s allowance of {}",
                value,
                owner,
                current_allowance
            )
        })?;

        if new_allowance.lt(&TokenAmount::zero()) {
            return Err(anyhow!(
                "Attempted to use {} of {}'s tokens from {}'s allowance of {}",
                value,
                owner,
                spender,
                current_allowance
            ));
        }

        // TODO: helper function to set a new allowance and flush hamts
        let owner_allowances = self.get_owner_allowance_map(bs, owner)?;
        // to reach here, allowance must have been previously non zero; so safe to assume the map exists
        let mut owner_allowances = owner_allowances.unwrap();
        owner_allowances.set(spender, BigIntDe(new_allowance.clone()))?;
        let mut allowance_map = self.get_allowances_map(bs)?;
        allowance_map.set(owner, owner_allowances.flush()?)?;
        self.allowances = allowance_map.flush()?;

        Ok(new_allowance)
    }

    /// Get the allowances map of a specific actor, resolving the CID link to a Hamt
    ///
    /// Ok(Some) if the owner has allocated allowances to other actors
    /// Ok(None) if the owner has no current non-zero allowances to other actors
    /// Err if operations on the underlying Hamt failed
    fn get_owner_allowance_map<BS: IpldStore + Clone>(
        &self,
        bs: &BS,
        owner: ActorID,
    ) -> Result<Option<Hamt<BS, BigIntDe, ActorID>>> {
        let allowances_map = self.get_allowances_map(bs)?;
        let owner_allowances = match allowances_map.get(&owner)? {
            Some(cid) => Some(Hamt::<BS, BigIntDe, ActorID>::load(cid, (*bs).clone())?),
            None => None,
        };
        Ok(owner_allowances)
    }

    /// Get the global allowances map
    ///
    /// Gets a HAMT with CIDs linking to other HAMTs
    fn get_allowances_map<BS: IpldStore + Clone>(&self, bs: &BS) -> Result<Hamt<BS, Cid, ActorID>> {
        Hamt::<BS, Cid, ActorID>::load(&self.allowances, (*bs).clone())
            .map_err(|e| anyhow!("Failed to load base allowances map {}", e))
    }
}

impl Cbor for TokenState {}

#[cfg(test)]
mod test {
    use fvm_shared::{
        bigint::{BigInt, Zero},
        ActorID,
    };

    use super::TokenState;
    use crate::blockstore::MemoryBlockstore;

    #[test]
    fn it_instantiates() {
        let bs = MemoryBlockstore::new();
        let state = TokenState::new(&bs).unwrap();
        let cid = state.save(&bs).unwrap();
        let saved_state = TokenState::load(&bs, &cid).unwrap();
        assert_eq!(state, saved_state);
    }

    #[test]
    fn it_increases_balance_of_new_actor() {
        let bs = MemoryBlockstore::new();
        let mut state = TokenState::new(&bs).unwrap();
        let actor: ActorID = 1;

        // Initially any actor has an implicit balance of 0
        assert_eq!(state.get_balance(&bs, actor).unwrap(), BigInt::zero());

        let amount = BigInt::from(100);
        state.increase_balance(&bs, actor, &amount).unwrap();
        let new_cid = state.save(&bs).unwrap();

        let state = TokenState::load(&bs, &new_cid).unwrap();
        assert_eq!(state.get_balance(&bs, actor).unwrap(), amount);
    }
}
