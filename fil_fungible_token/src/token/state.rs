use std::ops::Neg;

use anyhow::bail;
use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::Cbor;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::Error as HamtError;
use fvm_ipld_hamt::Hamt;
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;
use thiserror::Error;

const HAMT_BIT_WIDTH: u32 = 5;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("ipld hamt error: {0}")]
    IpldHamt(#[from] HamtError),
    #[error("missing state at cid: {0}")]
    MissingState(Cid),
    #[error("underlying serialization error: {0}")]
    Serialization(String),
    #[error(
        "negative balance caused by decreasing {owner:?}'s balance of {balance:?} by {delta:?}"
    )]
    InsufficientBalance { owner: ActorID, balance: TokenAmount, delta: TokenAmount },
    #[error(
        "{operator:?} attempted to utilise {delta:?} of allowance {allowance:?} set by {owner:?}"
    )]
    InsufficientAllowance {
        owner: Address,
        operator: Address,
        allowance: TokenAmount,
        delta: TokenAmount,
    },
    #[error("total_supply cannot be negative, cannot apply delta of {delta:?} to {supply:?}")]
    NegativeTotalSupply { supply: TokenAmount, delta: TokenAmount },
}

#[derive(Error, Debug)]
pub enum StateInvariantError {
    #[error("total supply was negative: {0}")]
    SupplyNegative(TokenAmount),
    #[error("the account for {account:?} had a negative balance of {balance:?}")]
    BalanceNegative { account: ActorID, balance: TokenAmount },
    #[error("the total supply {supply:?} does not match the sum of all balances {balance_sum:?}")]
    BalanceSupplyMismatch { supply: TokenAmount, balance_sum: TokenAmount },
    #[error(
        "a negative allowance of {allowance:?} was specified between {owner:?} and {operator:?}"
    )]
    NegativeAllowance { owner: ActorID, operator: ActorID, allowance: TokenAmount },
    #[error("stored a zero balance which should have been removed for {0}")]
    ExplicitZeroBalance(ActorID),
    #[error(
        "stored a zero allowance which should have been removed between {owner:?} and {operator:?}"
    )]
    ExplicitZeroAllowance { owner: ActorID, operator: ActorID },
    #[error("stored an allowance map for {0} though they have specified no allowances")]
    ExplicitEmptyAllowance(ActorID),
    #[error("stored an allowance for self {account:?} for {allowance:?}")]
    ExplicitSelfAllowance { account: ActorID, allowance: TokenAmount },
    #[error("underlying state error {0}")]
    State(#[from] StateError),
}

type Result<T> = std::result::Result<T, StateError>;

type Map<'bs, BS, K, V> = Hamt<&'bs BS, V, K>;

/// Token state IPLD structure
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct TokenState {
    /// Total supply of token
    pub supply: TokenAmount,

    /// Map<ActorId, TokenAmount> of balances as a Hamt
    pub balances: Cid,
    /// Map<ActorId, Map<ActorId, TokenAmount>> as a Hamt. Allowances are stored balances[owner][operator]
    pub allowances: Cid,
}

/// An abstraction over the IPLD layer to get and modify token state without dealing with HAMTs etc.
///
/// This is a simple wrapper of state and in general does not account for token protocol level
/// checks such as ensuring necessary approvals are enforced during transfers. This is left for the
/// caller to handle. However, some invariants such as non-negative balances, allowances and total
/// supply are enforced.
///
/// Some methods on TokenState require the caller to pass in a blockstore implementing the Clone
/// trait. It is assumed that when cloning the blockstore implementation does a "shallow-clone"
/// of the blockstore and provides access to the same underlying data.
impl TokenState {
    /// Create a new token state-tree, without committing it (the root Cid) to a blockstore
    pub fn new<BS: Blockstore>(store: &BS) -> Result<Self> {
        // Blockstore is still needed to create valid Cids for the Hamts
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
    pub fn load<BS: Blockstore>(bs: &BS, cid: &Cid) -> Result<Self> {
        // Load the actor state from the state tree.
        match bs.get_cbor::<Self>(cid) {
            Ok(Some(state)) => Ok(state),
            Ok(None) => Err(StateError::MissingState(*cid)),
            Err(err) => Err(StateError::Serialization(err.to_string())),
        }
    }

    /// Saves the current state to the blockstore, returning the cid
    pub fn save<BS: Blockstore>(&self, bs: &BS) -> Result<Cid> {
        let serialized = match fvm_ipld_encoding::to_vec(self) {
            Ok(s) => s,
            Err(err) => return Err(StateError::Serialization(err.to_string())),
        };
        let block = Block { codec: DAG_CBOR, data: serialized };
        let cid = match bs.put(Code::Blake2b256, &block) {
            Ok(cid) => cid,
            Err(err) => return Err(StateError::Serialization(err.to_string())),
        };
        Ok(cid)
    }

    /// Get the balance of an ActorID from the currently stored state
    pub fn get_balance<BS: Blockstore>(&self, bs: &BS, owner: ActorID) -> Result<TokenAmount> {
        let balances = self.get_balance_map(bs)?;

        let balance = match balances.get(&owner)? {
            Some(amount) => amount.clone(),
            None => TokenAmount::zero(),
        };

        Ok(balance)
    }

    /// Changes the balance of the specified account by the delta
    ///
    /// Caller must ensure that the sign of of the delta is consistent with token rules (i.e.
    /// negative transfers, burns etc. are not allowed). Returns the new balance of the account.
    pub fn change_balance_by<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_zero() {
            // This is a no-op as far as mutating state
            return self.get_balance(bs, owner);
        }

        let mut balance_map = self.get_balance_map(bs)?;
        let balance = balance_map.get(&owner)?;
        let balance = match balance {
            Some(amount) => amount.clone(),
            None => TokenAmount::zero(),
        };

        let new_balance = &balance + delta;

        // if the new_balance is negative, return an error
        if new_balance.is_negative() {
            return Err(StateError::InsufficientBalance { balance, delta: delta.clone(), owner });
        }

        if new_balance.is_zero() {
            balance_map.delete(&owner)?;
        } else {
            balance_map.set(owner, new_balance.clone())?;
        }

        self.balances = balance_map.flush()?;

        Ok(new_balance)
    }

    /// Retrieve the balance map as a HAMT
    fn get_balance_map<'bs, BS: Blockstore>(
        &self,
        bs: &'bs BS,
    ) -> Result<Map<'bs, BS, ActorID, TokenAmount>> {
        Ok(Hamt::load_with_bit_width(&self.balances, bs, HAMT_BIT_WIDTH)?)
    }

    /// Increase/decrease the total supply by the specified value
    ///
    /// Returns the new total supply
    pub fn change_supply_by(&mut self, delta: &TokenAmount) -> Result<&TokenAmount> {
        let new_supply = &self.supply + delta;
        if new_supply.is_negative() {
            return Err(StateError::NegativeTotalSupply {
                supply: self.supply.clone(),
                delta: delta.clone(),
            });
        }

        self.supply = new_supply;
        Ok(&self.supply)
    }

    /// Get the allowance that an owner has approved for a operator
    ///
    /// If an existing allowance cannot be found, it is implicitly assumed to be zero
    pub fn get_allowance_between<BS: Blockstore>(
        &self,
        bs: &BS,
        owner: ActorID,
        operator: ActorID,
    ) -> Result<TokenAmount> {
        let owner_allowances = self.get_owner_allowance_map(bs, owner)?;
        match owner_allowances {
            Some(hamt) => {
                let maybe_allowance = hamt.get(&operator)?;
                if let Some(allowance) = maybe_allowance {
                    return Ok(allowance.clone());
                }
                Ok(TokenAmount::zero())
            }
            None => Ok(TokenAmount::zero()),
        }
    }

    /// Change the allowance between owner and operator by the specified delta
    pub fn change_allowance_by<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        operator: ActorID,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_zero() {
            // This is a no-op as far as mutating state
            return self.get_allowance_between(bs, owner, operator);
        }

        let mut global_allowances_map = self.get_allowances_map(bs)?;

        // get or create the owner's allowance map
        let mut allowance_map = match global_allowances_map.get(&owner)? {
            Some(hamt) => Hamt::load_with_bit_width(hamt, bs, HAMT_BIT_WIDTH)?,
            None => {
                // the owner doesn't have any allowances, and the delta is negative, this is a no-op
                if delta.is_negative() {
                    return Ok(TokenAmount::zero());
                }

                // else create a new map for the owner
                Hamt::<&BS, TokenAmount, ActorID>::new_with_bit_width(bs, HAMT_BIT_WIDTH)
            }
        };

        // calculate new allowance (max with zero)
        let new_allowance = match allowance_map.get(&operator)? {
            Some(existing_allowance) => existing_allowance + delta,
            None => (*delta).clone(),
        }
        .max(TokenAmount::zero());

        // if the new allowance is zero, we can remove the entry from the state tree
        if new_allowance.is_zero() {
            allowance_map.delete(&operator)?;
        } else {
            allowance_map.set(operator, new_allowance.clone())?;
        }

        // if the owner-allowance map is empty, remove it from the global allowances map
        if allowance_map.is_empty() {
            global_allowances_map.delete(&owner)?;
        } else {
            // else update the global-allowance map
            global_allowances_map.set(owner, allowance_map.flush()?)?;
        }

        // update the state with the updated global map
        self.allowances = global_allowances_map.flush()?;

        Ok(new_allowance)
    }

    /// Revokes an approved allowance by removing the entry from the owner-operator map
    ///
    /// If that map becomes empty, it is removed from the root map.
    pub fn revoke_allowance<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        operator: ActorID,
    ) -> Result<()> {
        let allowance_map = self.get_owner_allowance_map(bs, owner)?;
        if let Some(mut map) = allowance_map {
            map.delete(&operator)?;
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
    pub fn attempt_use_allowance<BS: Blockstore>(
        &mut self,
        bs: &BS,
        operator: u64,
        owner: u64,
        amount: &TokenAmount,
    ) -> Result<TokenAmount> {
        let current_allowance = self.get_allowance_between(bs, owner, operator)?;

        // defensive check for operator != owner, really allowance should never be checked here
        if current_allowance.is_zero() && operator != owner {
            return Err(StateError::InsufficientAllowance {
                owner: Address::new_id(owner),
                operator: Address::new_id(operator),
                allowance: current_allowance,
                delta: amount.clone(),
            });
        }

        if amount.is_zero() {
            return Ok(current_allowance);
        }

        if current_allowance.lt(amount) {
            return Err(StateError::InsufficientAllowance {
                owner: Address::new_id(owner),
                operator: Address::new_id(operator),
                allowance: current_allowance,
                delta: amount.clone(),
            });
        }

        // let new_allowance = current_allowance - amount;
        let new_allowance = self.change_allowance_by(bs, owner, operator, &amount.neg())?;

        Ok(new_allowance)
    }

    /// Get the allowances map of a specific actor, resolving the CID link to a Hamt
    ///
    /// Ok(Some) if the owner has allocated allowances to other actors
    /// Ok(None) if the owner has no current non-zero allowances to other actors
    /// Err if operations on the underlying Hamt failed
    fn get_owner_allowance_map<'bs, BS: Blockstore>(
        &self,
        bs: &'bs BS,
        owner: ActorID,
    ) -> Result<Option<Map<'bs, BS, ActorID, TokenAmount>>> {
        let allowances_map = self.get_allowances_map(bs)?;
        let owner_allowances = match allowances_map.get(&owner)? {
            Some(cid) => Some(Hamt::load_with_bit_width(cid, bs, HAMT_BIT_WIDTH)?),
            None => None,
        };
        Ok(owner_allowances)
    }

    /// Get the global allowances map
    ///
    /// Gets a HAMT with CIDs linking to other HAMTs
    fn get_allowances_map<'bs, BS: Blockstore>(
        &self,
        bs: &'bs BS,
    ) -> Result<Map<'bs, BS, ActorID, Cid>> {
        Ok(Hamt::load_with_bit_width(&self.allowances, bs, HAMT_BIT_WIDTH)?)
    }

    /// Checks that the current state obeys all system invariants
    ///
    /// Checks that there are no zero balances, zero allowances or empty allowance maps explicitly
    /// stored in the blockstore. Checks that balances, total supply, allowances are never negative.
    /// Checks that sum of all balances matches total_supply. Checks that no allowances are stored
    /// where operator == owner.
    pub fn check_invariants<BS: Blockstore>(
        &self,
        bs: &BS,
    ) -> std::result::Result<(), StateInvariantError> {
        // check total supply
        if self.supply.is_negative() {
            return Err(StateInvariantError::SupplyNegative(self.supply.clone()));
        }

        // check balances
        let mut balance_sum = TokenAmount::zero();
        let mut maybe_err: Option<StateInvariantError> = None;
        let balances = self.get_balance_map(bs)?;
        let res = balances.for_each(|owner, balance| {
            // all balances must be positive
            if balance.is_negative() {
                maybe_err = Some(StateInvariantError::BalanceNegative {
                    account: *owner,
                    balance: balance.clone(),
                });
                bail!("invariant failed")
            }
            // zero balances should not be stored in the Hamt
            if balance.is_zero() {
                maybe_err = Some(StateInvariantError::ExplicitZeroBalance(*owner));
                bail!("invariant failed")
            }
            balance_sum = balance_sum.clone() + balance.clone();
            Ok(())
        });
        if res.is_err() {
            return Err(maybe_err.unwrap());
        }

        // all balances must add up to total supply
        if balance_sum.ne(&self.supply) {
            return Err(StateInvariantError::BalanceSupplyMismatch {
                supply: self.supply.clone(),
                balance_sum,
            });
        }

        let mut maybe_err: Option<StateInvariantError> = None;
        // check allowances are all non-negative
        let allowances_map = self.get_allowances_map(bs)?;
        let res = allowances_map.for_each(|owner, _| {
            let allowance_map = self.get_owner_allowance_map(bs, *owner)?;
            // check that the allowance map isn't empty
            if allowance_map.is_none() {
                maybe_err = Some(StateInvariantError::ExplicitEmptyAllowance(*owner));
                bail!("invariant failed")
            }

            let allowance_map = allowance_map.unwrap();
            allowance_map.for_each(|operator, allowance| {
                // check there's no stored self-stored allowance
                if *owner == *operator {
                    maybe_err = Some(StateInvariantError::ExplicitSelfAllowance {
                        account: *owner,
                        allowance: allowance.clone(),
                    });
                    bail!("invariant failed")
                }
                // check the allowance isn't negative
                if allowance.is_negative() {
                    maybe_err = Some(StateInvariantError::NegativeAllowance {
                        owner: *owner,
                        operator: *operator,
                        allowance: allowance.clone(),
                    });
                    bail!("invariant failed")
                }
                // check there's no explicit zero allowance
                if allowance.is_zero() {
                    maybe_err = Some(StateInvariantError::ExplicitZeroAllowance {
                        owner: *owner,
                        operator: *operator,
                    });
                    bail!("invariant failed")
                }
                Ok(())
            })?;
            Ok(())
        });

        if res.is_err() {
            return Err(maybe_err.unwrap());
        }

        Ok(())
    }
}

impl Cbor for TokenState {}

#[cfg(test)]
mod test {
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_shared::econ::TokenAmount;
    use fvm_shared::{bigint::Zero, ActorID};

    use super::TokenState;

    #[test]
    fn it_instantiates() {
        let bs = &MemoryBlockstore::new();
        let state = TokenState::new(bs).unwrap();
        let cid = state.save(bs).unwrap();
        let saved_state = TokenState::load(bs, &cid).unwrap();
        assert_eq!(state, saved_state);
    }

    #[test]
    fn it_increases_balance_from_zero() {
        let bs = &MemoryBlockstore::new();
        let mut state = TokenState::new(bs).unwrap();
        let actor: ActorID = 1;

        // Initially any actor has an implicit balance of 0
        assert_eq!(state.get_balance(bs, actor).unwrap(), TokenAmount::zero());

        let amount = TokenAmount::from_atto(100);
        state.change_balance_by(bs, actor, &amount).unwrap();

        assert_eq!(state.get_balance(bs, actor).unwrap(), amount);
    }

    #[test]
    fn it_fails_to_decrease_balance_below_zero() {
        let bs = &MemoryBlockstore::new();
        let mut state = TokenState::new(bs).unwrap();
        let actor: ActorID = 1;

        // can't decrease from zero
        state.change_balance_by(bs, actor, &TokenAmount::from_atto(-1)).unwrap_err();
        let balance = state.get_balance(bs, actor).unwrap();
        assert_eq!(balance, TokenAmount::zero());

        // can't become negative from a positive balance
        state.change_balance_by(bs, actor, &TokenAmount::from_atto(50)).unwrap();
        state.change_balance_by(bs, actor, &TokenAmount::from_atto(-100)).unwrap_err();
    }

    #[test]
    fn it_sets_allowances_between_actors() {
        let bs = &MemoryBlockstore::new();
        let mut state = TokenState::new(&bs).unwrap();
        let owner: ActorID = 1;
        let operator: ActorID = 2;

        // initial allowance is zero
        let initial_allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(initial_allowance, TokenAmount::zero());

        // can set a positive allowance
        let delta = TokenAmount::from_atto(100);
        let ret = state.change_allowance_by(bs, owner, operator, &delta).unwrap();
        assert_eq!(ret, delta);
        let allowance_1 = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(allowance_1, delta);

        // vice-versa allowance was unaffected
        let reverse_allowance = state.get_allowance_between(bs, operator, owner).unwrap();
        assert_eq!(reverse_allowance, TokenAmount::zero());

        // can subtract an allowance
        let delta = TokenAmount::from_atto(-50);
        let ret = state.change_allowance_by(bs, owner, operator, &delta).unwrap();
        assert_eq!(ret, TokenAmount::from_atto(50));
        let allowance_2 = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(allowance_2, allowance_1 + delta);
        assert_eq!(allowance_2, TokenAmount::from_atto(50));

        // allowance won't go negative
        let delta = TokenAmount::from_atto(-100);
        let ret = state.change_allowance_by(bs, owner, operator, &delta).unwrap();
        assert_eq!(ret, TokenAmount::zero());
        let allowance_3 = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(allowance_3, TokenAmount::zero());
    }

    #[test]
    fn it_consumes_allowances_atomically() {
        let bs = &MemoryBlockstore::new();
        let mut state = TokenState::new(bs).unwrap();
        let owner: ActorID = 1;
        let operator: ActorID = 2;

        // set a positive allowance
        let delta = TokenAmount::from_atto(100);
        state.change_allowance_by(bs, owner, operator, &delta).unwrap();

        // can consume an allowance
        let new_allowance =
            state.attempt_use_allowance(bs, operator, owner, &TokenAmount::from_atto(60)).unwrap();
        assert_eq!(new_allowance, TokenAmount::from_atto(40));
        let new_allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(new_allowance, TokenAmount::from_atto(40));

        // cannot consume more allowance than approved
        state.attempt_use_allowance(bs, operator, owner, &TokenAmount::from_atto(50)).unwrap_err();
        // allowance was unchanged
        let new_allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(new_allowance, TokenAmount::from_atto(40));
    }

    #[test]
    fn it_revokes_allowances() {
        let bs = &MemoryBlockstore::new();
        let mut state = TokenState::new(bs).unwrap();
        let owner: ActorID = 1;
        let operator: ActorID = 2;

        // set a positive allowance
        let delta = TokenAmount::from_atto(100);
        state.change_allowance_by(bs, owner, operator, &delta).unwrap();
        state.change_allowance_by(bs, owner, operator, &delta).unwrap();
        let allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(allowance, TokenAmount::from_atto(200));

        state.revoke_allowance(bs, owner, operator).unwrap();
        let allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(allowance, TokenAmount::zero());
    }
}
