use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore as IpldStore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::Cbor;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::Error as HamtError;
use fvm_ipld_hamt::Hamt;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;
use num_traits::Signed;
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
        "negative balance caused by subtracting {delta:?} from {owner:?}'s balance of {balance:?}"
    )]
    NegativeBalance {
        owner: ActorID,
        balance: TokenAmount,
        delta: TokenAmount,
    },
    #[error(
        "{spender:?} attempted to utilise {delta:?} of allowance {allowance:?} set by {owner:?}"
    )]
    InsufficentAllowance {
        owner: ActorID,
        spender: ActorID,
        allowance: TokenAmount,
        delta: TokenAmount,
    },
}

type Result<T> = std::result::Result<T, StateError>;

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
/// caller to handle. However, some invariants such as non-negative balances, allowances and total
/// supply are enforced.
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
            Ok(None) => Err(StateError::MissingState(*cid)),
            Err(err) => Err(StateError::Serialization(err.to_string())),
        }
    }

    /// Saves the current state to the blockstore, returning the cid
    pub fn save<BS: IpldStore>(&self, bs: &BS) -> Result<Cid> {
        let serialized = match fvm_ipld_encoding::to_vec(self) {
            Ok(s) => s,
            Err(err) => return Err(StateError::Serialization(err.to_string())),
        };
        let block = Block {
            codec: DAG_CBOR,
            data: serialized,
        };
        let cid = match bs.put(Code::Blake2b256, &block) {
            Ok(cid) => cid,
            Err(err) => return Err(StateError::Serialization(err.to_string())),
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

    /// Changes the balance of the specified account by the delta
    ///
    /// Caller must ensure that the sign of of the delta is consistent with token rules (i.e.
    /// negative transfers, burns etc. are not allowed)
    pub fn change_balance_by<BS: IpldStore + Clone>(
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

        let new_balance = match balance {
            Some(existing_amount) => existing_amount.0.clone() + delta,
            None => (*delta).clone(),
        };

        // if the new_balance is negative, return an error
        if new_balance.is_negative() {
            return Err(StateError::NegativeBalance {
                balance: new_balance,
                delta: delta.clone(),
                owner,
            });
        }

        balance_map.set(owner, BigIntDe(new_balance.clone()))?;
        self.balances = balance_map.flush()?;

        Ok(new_balance)
    }

    /// Retrieve the balance map as a HAMT
    fn get_balance_map<BS: IpldStore + Clone>(
        &self,
        bs: &BS,
    ) -> Result<Hamt<BS, BigIntDe, ActorID>> {
        Ok(Hamt::<BS, BigIntDe, ActorID>::load_with_bit_width(
            &self.balances,
            (*bs).clone(),
            HAMT_BIT_WIDTH,
        )?)
    }

    /// Increase the total supply by the specified value
    ///
    /// The requested amount must be non-negative. Returns the new total supply
    pub fn increase_supply(&mut self, value: &TokenAmount) -> Result<&TokenAmount> {
        self.supply += value;
        Ok(&self.supply)
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

    /// Change the allowance between owner and spender by the specified delta
    pub fn change_allowance_by<BS: IpldStore + Clone>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        spender: ActorID,
        delta: &TokenAmount,
    ) -> Result<TokenAmount> {
        if delta.is_zero() {
            // This is a no-op as far as mutating state
            return self.get_allowance_between(bs, owner, spender);
        }

        let mut global_allowances_map = self.get_allowances_map(bs)?;

        // get or create the owner's allowance map
        let mut allowance_map = match global_allowances_map.get(&owner)? {
            Some(hamt) => Hamt::<BS, BigIntDe, ActorID>::load_with_bit_width(
                hamt,
                (*bs).clone(),
                HAMT_BIT_WIDTH,
            )?,
            None => {
                // the owner doesn't have any allowances, and the delta is negative, this is a no-op
                if delta.is_negative() {
                    return Ok(TokenAmount::zero());
                }

                // else create a new map for the owner
                Hamt::<BS, BigIntDe, ActorID>::new_with_bit_width((*bs).clone(), HAMT_BIT_WIDTH)
            }
        };

        // calculate new allowance (max with zero)
        let new_allowance = match allowance_map.get(&spender)? {
            Some(existing_allowance) => existing_allowance.0.clone() + delta,
            None => (*delta).clone(),
        }
        .max(TokenAmount::zero());

        // if the new allowance is zero, we can remove the entry from the state tree
        if new_allowance.is_zero() {
            allowance_map.delete(&spender)?;
        } else {
            allowance_map.set(spender, BigIntDe(new_allowance.clone()))?;
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

        if current_allowance.lt(value) {
            return Err(StateError::InsufficentAllowance {
                owner,
                spender,
                allowance: current_allowance,
                delta: value.clone(),
            });
        }

        let new_allowance = current_allowance - value;

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
            Some(cid) => Some(Hamt::<BS, BigIntDe, ActorID>::load_with_bit_width(
                cid,
                (*bs).clone(),
                HAMT_BIT_WIDTH,
            )?),
            None => None,
        };
        Ok(owner_allowances)
    }

    /// Get the global allowances map
    ///
    /// Gets a HAMT with CIDs linking to other HAMTs
    fn get_allowances_map<BS: IpldStore + Clone>(&self, bs: &BS) -> Result<Hamt<BS, Cid, ActorID>> {
        Ok(Hamt::<BS, Cid, ActorID>::load_with_bit_width(
            &self.allowances,
            (*bs).clone(),
            HAMT_BIT_WIDTH,
        )?)
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
    use crate::blockstore::SharedMemoryBlockstore;

    #[test]
    fn it_instantiates() {
        let bs = &SharedMemoryBlockstore::new();
        let state = TokenState::new(bs).unwrap();
        let cid = state.save(bs).unwrap();
        let saved_state = TokenState::load(bs, &cid).unwrap();
        assert_eq!(state, saved_state);
    }

    #[test]
    fn it_increases_balance_from_zero() {
        let bs = &SharedMemoryBlockstore::new();
        let mut state = TokenState::new(bs).unwrap();
        let actor: ActorID = 1;

        // Initially any actor has an implicit balance of 0
        assert_eq!(state.get_balance(bs, actor).unwrap(), BigInt::zero());

        let amount = BigInt::from(100);
        state.change_balance_by(bs, actor, &amount).unwrap();

        assert_eq!(state.get_balance(bs, actor).unwrap(), amount);
    }

    #[test]
    fn it_fails_to_decrease_balance_below_zero() {
        let bs = &SharedMemoryBlockstore::new();
        let mut state = TokenState::new(bs).unwrap();
        let actor: ActorID = 1;

        // can't decrease from zero
        state
            .change_balance_by(bs, actor, &BigInt::from(-1))
            .unwrap_err();
        let balance = state.get_balance(bs, actor).unwrap();
        assert_eq!(balance, BigInt::zero());

        // can't become negative from a positive balance
        state
            .change_balance_by(bs, actor, &BigInt::from(50))
            .unwrap();
        state
            .change_balance_by(bs, actor, &BigInt::from(-100))
            .unwrap_err();
    }

    #[test]
    fn it_sets_allowances_between_actors() {
        let bs = &SharedMemoryBlockstore::new();
        let mut state = TokenState::new(&bs).unwrap();
        let owner: ActorID = 1;
        let spender: ActorID = 2;

        // initial allowance is zero
        let initial_allowance = state.get_allowance_between(bs, owner, spender).unwrap();
        assert_eq!(initial_allowance, BigInt::zero());

        // can set a positive allowance
        let delta = BigInt::from(100);
        let ret = state
            .change_allowance_by(bs, owner, spender, &delta)
            .unwrap();
        assert_eq!(ret, delta);
        let allowance_1 = state.get_allowance_between(bs, owner, spender).unwrap();
        assert_eq!(allowance_1, delta);

        // vice-versa allowance was unaffected
        let reverse_allowance = state.get_allowance_between(bs, spender, owner).unwrap();
        assert_eq!(reverse_allowance, BigInt::zero());

        // can subtract an allowance
        let delta = BigInt::from(-50);
        let ret = state
            .change_allowance_by(bs, owner, spender, &delta)
            .unwrap();
        assert_eq!(ret, BigInt::from(50));
        let allowance_2 = state.get_allowance_between(bs, owner, spender).unwrap();
        assert_eq!(allowance_2, allowance_1 + delta);
        assert_eq!(allowance_2, BigInt::from(50));

        // allowance won't go negative
        let delta = BigInt::from(-100);
        let ret = state
            .change_allowance_by(bs, owner, spender, &delta)
            .unwrap();
        assert_eq!(ret, BigInt::zero());
        let allowance_3 = state.get_allowance_between(bs, owner, spender).unwrap();
        assert_eq!(allowance_3, BigInt::zero());
    }
}
