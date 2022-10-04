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
use fvm_ipld_hamt::Hamt;
use fvm_ipld_hamt::{BytesKey, Error as HamtError};
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;
use integer_encoding::VarInt;
use thiserror::Error;

/// This value has been chosen to optimise to reduce gas-costs when accessing the balances map. Non-
/// standard use cases of the token library might find a different value to be more efficient.
pub const DEFAULT_HAMT_BIT_WIDTH: u32 = 3;

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
    #[error("allowance cannot be negative, cannot set allowance between {owner:?} and {operator:?} to {amount:?}")]
    NegativeAllowance { amount: TokenAmount, owner: ActorID, operator: ActorID },
    #[error("balance cannot be negative, cannot set balance of {owner:?} to {amount:?}")]
    NegativeBalance { amount: TokenAmount, owner: ActorID },
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
    #[error("invalid serialized owner key {0:?}")]
    InvalidOwnerKey(BytesKey),
    #[error("owner {owner:?} had a balance {balance:?} which is not a multiple of the granularity {granularity:?}")]
    InvalidGranularity { owner: ActorID, balance: TokenAmount, granularity: u64 },
    #[error("underlying state error {0}")]
    State(#[from] StateError),
}

type Result<T> = std::result::Result<T, StateError>;

type Map<'bs, BS, K, V> = Hamt<&'bs BS, V, K>;
type BalanceMap<'bs, BS> = Map<'bs, BS, BytesKey, TokenAmount>;
type AllowanceMap<'bs, BS> = Map<'bs, BS, BytesKey, Cid>;
type OwnerAllowanceMap<'bs, BS> = Map<'bs, BS, BytesKey, TokenAmount>;

/// Token state IPLD structure
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct TokenState {
    /// Total supply of token
    pub supply: TokenAmount,
    /// Map<ActorId, TokenAmount> of balances as a Hamt
    pub balances: Cid,
    /// Map<ActorId, Map<ActorId, TokenAmount>> as a Hamt. Allowances are stored balances[owner][operator]
    pub allowances: Cid,
    /// Bit-width to use when loading Hamts
    hamt_bit_width: u32,
}

/// An abstraction over the IPLD layer to get and modify token state without dealing with HAMTs etc.
///
/// This is a simple wrapper of state and in general does not account for token protocol level
/// checks such as ensuring necessary approvals are enforced during transfers. This is left for the
/// caller to handle. However, some invariants such as non-negative balances, allowances and total
/// supply are enforced.
impl TokenState {
    /// Create a new token state-tree, without committing it (the root cid) to a blockstore
    pub fn new<BS: Blockstore>(store: &BS) -> Result<Self> {
        Self::new_with_bit_width(store, DEFAULT_HAMT_BIT_WIDTH)
    }

    /// Create a new token state-tree, without committing it (the root cid) to a blockstore
    ///
    /// Explicitly sets the bit width of underlying Hamt structures. Caller must ensure
    /// 1 <= hamt_bit_width <= 8.
    pub fn new_with_bit_width<BS: Blockstore>(store: &BS, hamt_bit_width: u32) -> Result<Self> {
        // Blockstore is still needed to create valid Cids for the Hamts
        let empty_balance_map = BalanceMap::new_with_bit_width(store, hamt_bit_width).flush()?;
        let empty_allowances_map =
            AllowanceMap::new_with_bit_width(store, hamt_bit_width).flush()?;

        Ok(Self {
            supply: Default::default(),
            balances: empty_balance_map,
            allowances: empty_allowances_map,
            hamt_bit_width,
        })
    }

    /// Loads a fresh copy of the state from a blockstore from a given cid
    pub fn load<BS: Blockstore>(bs: &BS, cid: &Cid) -> Result<Self> {
        // Load the actor state from the state tree.
        let state = match bs.get_cbor::<Self>(cid) {
            Ok(Some(state)) => Ok(state),
            Ok(None) => Err(StateError::MissingState(*cid)),
            Err(err) => Err(StateError::Serialization(err.to_string())),
        }?;

        Ok(state)
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

        let balance = match balances.get(&actor_id_key(owner))? {
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
        let owner_key = actor_id_key(owner);
        let balance = balance_map.get(&owner_key)?;
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
            balance_map.delete(&owner_key)?;
        } else {
            balance_map.set(owner_key, new_balance.clone())?;
        }

        self.balances = balance_map.flush()?;

        Ok(new_balance)
    }

    /// Set the balance of the account returning the old balance
    pub fn set_balance<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        new_balance: &TokenAmount,
    ) -> Result<TokenAmount> {
        // if the new balance is negative, return an error
        if new_balance.is_negative() {
            return Err(StateError::NegativeBalance { amount: new_balance.clone(), owner });
        }

        let mut balance_map = self.get_balance_map(bs)?;
        let owner_key = actor_id_key(owner);
        let old_balance = match balance_map.get(&owner_key)? {
            Some(amount) => amount.clone(),
            None => TokenAmount::zero(),
        };

        // if the new balance is zero, remove from balance map
        if new_balance.is_zero() {
            balance_map.delete(&owner_key)?;
            self.balances = balance_map.flush()?;
            return Ok(old_balance);
        }

        // else, set the new balance
        balance_map.set(owner_key, new_balance.clone())?;
        self.balances = balance_map.flush()?;
        Ok(old_balance)
    }

    /// Retrieve the balance map as a HAMT
    pub fn get_balance_map<'bs, BS: Blockstore>(&self, bs: &'bs BS) -> Result<BalanceMap<'bs, BS>> {
        Ok(BalanceMap::load_with_bit_width(&self.balances, bs, self.hamt_bit_width)?)
    }

    /// Retrieve the number of token holders
    ///
    /// This involves iterating through the entire HAMT
    pub fn count_balances<BS: Blockstore>(&self, bs: &BS) -> Result<usize> {
        let balance_map = self.get_balance_map(bs)?;
        Ok(balance_map.for_each(|_, _| Ok(())).into_iter().count())
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
            Some(map) => {
                let maybe_allowance = map.get(&actor_id_key(operator))?;
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
        let owner_key = actor_id_key(owner);
        let mut allowance_map = match global_allowances_map.get(&owner_key)? {
            Some(cid) => OwnerAllowanceMap::load_with_bit_width(cid, bs, self.hamt_bit_width)?,
            None => {
                // the owner doesn't have any allowances, and the delta is negative, this is a no-op
                if delta.is_negative() {
                    return Ok(TokenAmount::zero());
                }

                // else create a new map for the owner
                OwnerAllowanceMap::new_with_bit_width(bs, self.hamt_bit_width)
            }
        };

        // calculate new allowance (max with zero)
        let operator_key = actor_id_key(operator);
        let new_allowance = match allowance_map.get(&operator_key)? {
            Some(existing_allowance) => existing_allowance + delta,
            None => (*delta).clone(),
        }
        .max(TokenAmount::zero());

        // if the new allowance is zero, we can remove the entry from the state tree
        if new_allowance.is_zero() {
            allowance_map.delete(&operator_key)?;
        } else {
            allowance_map.set(operator_key, new_allowance.clone())?;
        }

        // if the owner-allowance map is empty, remove it from the global allowances map
        if allowance_map.is_empty() {
            global_allowances_map.delete(&owner_key)?;
        } else {
            // else update the global-allowance map
            global_allowances_map.set(owner_key, allowance_map.flush()?)?;
        }

        // update the state with the updated global map
        self.allowances = global_allowances_map.flush()?;

        Ok(new_allowance)
    }

    /// Revokes an approved allowance by removing the entry from the owner-operator map
    ///
    /// If that map becomes empty, it is removed from the root map. Returns the old allowance
    pub fn revoke_allowance<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        operator: ActorID,
    ) -> Result<TokenAmount> {
        let allowance_map = self.get_owner_allowance_map(bs, owner)?;
        if let Some(mut map) = allowance_map {
            // revoke the allowance
            let operator_key = actor_id_key(operator);
            let old_allowance = match map.delete(&operator_key)? {
                Some((_, amount)) => amount,
                None => TokenAmount::zero(),
            };

            // if the allowance map has become empty it can be dropped entirely
            let owner_key = actor_id_key(owner);
            if map.is_empty() {
                let mut root_allowance_map = self.get_allowances_map(bs)?;
                root_allowance_map.delete(&owner_key)?;
                self.allowances = root_allowance_map.flush()?;
            } else {
                let new_cid = map.flush()?;
                let mut root_allowance_map = self.get_allowances_map(bs)?;
                root_allowance_map.set(owner_key, new_cid)?;
                self.allowances = root_allowance_map.flush()?;
            }

            Ok(old_allowance)
        } else {
            // no allowance map exists, there is nothing to do
            Ok(TokenAmount::zero())
        }
    }

    /// Set the allowance between owner and operator to a specific amount, returning the old allowance
    pub fn set_allowance<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        operator: ActorID,
        amount: &TokenAmount,
    ) -> Result<TokenAmount> {
        if amount.is_negative() {
            return Err(StateError::NegativeAllowance { owner, operator, amount: amount.clone() });
        }

        let mut root_allowances_map = self.get_allowances_map(bs)?;

        // get or create the owner's allowance map
        let owner_key = actor_id_key(owner);
        let mut allowance_map = match root_allowances_map.get(&owner_key)? {
            Some(cid) => OwnerAllowanceMap::load_with_bit_width(cid, bs, self.hamt_bit_width)?,
            None => OwnerAllowanceMap::new_with_bit_width(bs, self.hamt_bit_width),
        };

        // determine the existing allowance
        let operator_key = actor_id_key(operator);
        let old_allowance = match allowance_map.get(&operator_key)? {
            Some(a) => a.clone(),
            None => TokenAmount::zero(),
        };

        if amount.is_zero() {
            // zero allowance may have special handling for cleaning up
            self.revoke_allowance(bs, owner, operator)?;
            return Ok(old_allowance);
        }

        // set the new allowance
        allowance_map.set(operator_key, amount.clone())?;
        // update the root map
        root_allowances_map.set(owner_key, allowance_map.flush()?)?;
        // update the state with the updated global map
        self.allowances = root_allowances_map.flush()?;

        Ok(old_allowance)
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
    pub fn get_owner_allowance_map<'bs, BS: Blockstore>(
        &self,
        bs: &'bs BS,
        owner: ActorID,
    ) -> Result<Option<OwnerAllowanceMap<'bs, BS>>> {
        let allowances_map = self.get_allowances_map(bs)?;
        let owner_allowances = match allowances_map.get(&actor_id_key(owner))? {
            Some(cid) => {
                Some(OwnerAllowanceMap::load_with_bit_width(cid, bs, self.hamt_bit_width)?)
            }
            None => None,
        };
        Ok(owner_allowances)
    }

    /// Get the global allowances map
    ///
    /// Gets a HAMT with CIDs linking to other HAMTs
    pub fn get_allowances_map<'bs, BS: Blockstore>(
        &self,
        bs: &'bs BS,
    ) -> Result<AllowanceMap<'bs, BS>> {
        Ok(AllowanceMap::load_with_bit_width(&self.allowances, bs, self.hamt_bit_width)?)
    }

    /// Checks that the current state obeys all system invariants
    ///
    /// Checks that there are no zero balances, zero allowances or empty allowance maps explicitly
    /// stored in the blockstore. Checks that balances, total supply, allowances are never negative.
    /// Checks that sum of all balances matches total_supply. Checks that no allowances are stored
    /// where operator == owner. Checks that all balances are a multiple of the granularity.
    ///
    /// Returns a state summary that can be used to check application specific invariants.
    pub fn check_invariants<'bs, BS: Blockstore>(
        &self,
        bs: &'bs BS,
        granularity: u64,
    ) -> std::result::Result<StateSummary<'bs, BS>, StateInvariantError> {
        // check total supply
        if self.supply.is_negative() {
            return Err(StateInvariantError::SupplyNegative(self.supply.clone()));
        }

        // check balances
        let mut balance_sum = TokenAmount::zero();
        let mut maybe_err: Option<StateInvariantError> = None;
        let balances = self.get_balance_map(bs)?;
        let res = balances.for_each(|owner_key, balance| {
            let owner = match decode_actor_id(owner_key) {
                None => {
                    maybe_err = Some(StateInvariantError::InvalidOwnerKey(owner_key.clone()));
                    bail!("invariant failed");
                }
                Some(a) => a,
            };
            // all balances must be positive
            if balance.is_negative() {
                maybe_err = Some(StateInvariantError::BalanceNegative {
                    account: owner,
                    balance: balance.clone(),
                });
                bail!("invariant failed")
            }
            // zero balances should not be stored in the Hamt
            if balance.is_zero() {
                maybe_err = Some(StateInvariantError::ExplicitZeroBalance(owner));
                bail!("invariant failed")
            }

            let (_, modulus) = balance.div_rem(granularity);
            if !modulus.is_zero() {
                maybe_err = Some(StateInvariantError::InvalidGranularity {
                    balance: balance.clone(),
                    owner,
                    granularity,
                });
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
            let owner = match decode_actor_id(owner) {
                None => {
                    bail!("invalid owner key in allowances map")
                }
                Some(a) => a,
            };
            let allowance_map = self.get_owner_allowance_map(bs, owner)?;
            // check that the allowance map isn't empty
            if allowance_map.is_none() {
                maybe_err = Some(StateInvariantError::ExplicitEmptyAllowance(owner));
                bail!("invariant failed")
            }

            let allowance_map = allowance_map.unwrap();
            allowance_map.for_each(|operator, allowance| {
                let operator = match decode_actor_id(operator) {
                    None => {
                        bail!("invalid operator key in allowances map")
                    }
                    Some(a) => a,
                };

                // check there's no stored self-stored allowance
                if owner == operator {
                    maybe_err = Some(StateInvariantError::ExplicitSelfAllowance {
                        account: owner,
                        allowance: allowance.clone(),
                    });
                    bail!("invariant failed")
                }
                // check the allowance isn't negative
                if allowance.is_negative() {
                    maybe_err = Some(StateInvariantError::NegativeAllowance {
                        owner,
                        operator,
                        allowance: allowance.clone(),
                    });
                    bail!("invariant failed")
                }
                // check there's no explicit zero allowance
                if allowance.is_zero() {
                    maybe_err =
                        Some(StateInvariantError::ExplicitZeroAllowance { owner, operator });
                    bail!("invariant failed")
                }
                Ok(())
            })?;
            Ok(())
        });

        if res.is_err() {
            return Err(maybe_err.unwrap());
        }

        Ok(StateSummary {
            balance_map: self.get_balance_map(bs)?,
            allowance_map: self.get_allowances_map(bs)?,
            total_supply: self.supply.clone(),
        })
    }
}

pub fn actor_id_key(a: ActorID) -> BytesKey {
    a.encode_var_vec().into()
}

pub fn decode_actor_id(key: &BytesKey) -> Option<ActorID> {
    u64::decode_var(key.0.as_slice()).map(|a| a.0)
}

impl Cbor for TokenState {}

/// A summary of the current state to allow checking application specific invariants
pub struct StateSummary<'bs, BS>
where
    BS: Blockstore,
{
    pub balance_map: BalanceMap<'bs, BS>,
    pub allowance_map: AllowanceMap<'bs, BS>,
    pub total_supply: TokenAmount,
}

#[cfg(test)]
mod test {
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_shared::econ::TokenAmount;
    use fvm_shared::{bigint::Zero, ActorID};

    use super::TokenState;
    use crate::token::state::{actor_id_key, StateError};

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
    fn it_sets_balances() {
        let bs = &MemoryBlockstore::new();
        let mut state = TokenState::new(bs).unwrap();
        let actor: ActorID = 1;

        // can set a positive balance
        let old_balance = state.set_balance(bs, actor, &TokenAmount::from_atto(1)).unwrap();
        assert_eq!(old_balance, TokenAmount::from_atto(0));
        let balance = state.get_balance(bs, actor).unwrap();
        assert_eq!(balance, TokenAmount::from_atto(1));

        // can set a new positive balance, overwriting the old one
        let old_balance = state.set_balance(bs, actor, &TokenAmount::from_atto(100)).unwrap();
        assert_eq!(old_balance, TokenAmount::from_atto(1));
        let balance = state.get_balance(bs, actor).unwrap();
        assert_eq!(balance, TokenAmount::from_atto(100));

        // cannot set a negative balance
        state.set_balance(bs, actor, &TokenAmount::from_atto(-1)).unwrap_err();
    }

    #[test]
    fn it_changes_allowances_between_actors() {
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
    fn it_sets_allowances_between_actors() {
        let bs = &MemoryBlockstore::new();
        let mut state = TokenState::new(&bs).unwrap();
        let owner: ActorID = 1;
        let operator: ActorID = 2;

        // initial allowance is zero
        let initial_allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(initial_allowance, TokenAmount::zero());

        // can set a positive allowance
        let allowance = TokenAmount::from_atto(100);
        let old_allowance = state.set_allowance(bs, owner, operator, &allowance).unwrap();
        assert_eq!(old_allowance, TokenAmount::zero());
        let returned_allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(returned_allowance, allowance);

        // can set a different positive allowance
        let allowance = TokenAmount::from_atto(120);
        let old_allowance = state.set_allowance(bs, owner, operator, &allowance).unwrap();
        assert_eq!(old_allowance, TokenAmount::from_atto(100));
        let returned_allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(returned_allowance, allowance);

        // can set a zero-allowance
        let allowance = TokenAmount::from_atto(0);
        let old_allowance = state.set_allowance(bs, owner, operator, &allowance).unwrap();
        assert_eq!(old_allowance, TokenAmount::from_atto(120));
        let returned_allowance = state.get_allowance_between(bs, owner, operator).unwrap();
        assert_eq!(returned_allowance, allowance);
        // the map entry is cleaned-up
        let root_map = state.get_allowances_map(bs).unwrap();
        let owner_key = actor_id_key(owner);
        assert!(!root_map.contains_key(&owner_key).unwrap());

        // can't set negative allowance
        let allowance = TokenAmount::from_atto(-50);
        let err = state.set_allowance(bs, owner, operator, &allowance).unwrap_err();
        if let StateError::NegativeAllowance { owner: _, operator: _, amount } = err {
            assert_eq!(amount, allowance);
        }
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

    #[test]
    fn it_allows_variable_bit_width() {
        let bs = &MemoryBlockstore::new();
        let mut state = TokenState::new_with_bit_width(bs, 8).unwrap();
        let amount = TokenAmount::from_whole(5);
        for owner in 0_u64..10_u64 {
            state.set_balance(&bs, owner, &amount).unwrap();
        }
        let cid = state.save(bs).unwrap();

        let loaded_state = TokenState::load(bs, &cid).unwrap();
        assert_eq!(loaded_state.hamt_bit_width, 8);
        for owner in 0_u64..10_u64 {
            // loading the hamts with the wrong bitwidth would result in corrupted data
            let balance = loaded_state.get_balance(&bs, owner).unwrap();
            assert_eq!(balance, amount);
        }
    }
}
