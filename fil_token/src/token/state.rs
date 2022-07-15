use anyhow::anyhow;
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
use fvm_sdk::sself;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;

const HAMT_BIT_WIDTH: u32 = 5;

/// Token state IPLD structure
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
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
        match bs.get_cbor::<Self>(&cid) {
            Ok(Some(state)) => Ok(state),
            Ok(None) => Err(anyhow!("No state at this cid {:?}", cid)),
            Err(err) => Err(anyhow!("failed to get state: {}", err)),
        }
    }

    /// Saves the current state to the blockstore
    pub fn save<BS: IpldStore + Copy>(&self, bs: &BS) -> Result<Cid> {
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
        if let Err(err) = sself::set_root(&cid) {
            return Err(anyhow!("failed to set root ciid: {:}", err));
        }
        Ok(cid)
    }

    /// Get the balance of an ActorID from the currently stored state
    pub fn get_balance<BS: IpldStore + Copy>(
        &self,
        bs: &BS,
        owner: ActorID,
    ) -> Result<TokenAmount> {
        let balances = self.get_balance_map(bs)?;

        let balance: TokenAmount;
        match balances.get(&owner)? {
            Some(amount) => balance = amount.0.clone(),
            None => balance = TokenAmount::zero(),
        }

        Ok(balance)
    }

    /// Attempts to increase the balance of the specified account by the value
    ///
    /// The requested amount must be non-negative.
    /// Returns an error if the balance overflows, else returns the new balance
    pub fn increase_balance<BS: IpldStore + Copy>(
        &self,
        bs: &BS,
        actor: ActorID,
        value: &TokenAmount,
    ) -> Result<TokenAmount> {
        let mut balance_map = self.get_balance_map(bs)?;
        let balance = balance_map.get(&actor)?;
        match balance {
            Some(existing_amount) => {
                let existing_amount = existing_amount.clone().0;
                let new_amount = existing_amount.checked_add(&value).ok_or_else(|| {
                    anyhow!(
                        "Overflow when adding {} to {}'s balance of {}",
                        value,
                        actor,
                        existing_amount
                    )
                })?;

                balance_map.set(actor, BigIntDe(new_amount.clone()))?;
                Ok(new_amount)
            }
            None => {
                balance_map.set(actor, BigIntDe(value.clone()))?;
                Ok(value.clone())
            }
        }
    }

    /// Retrieve the balance map as a HAMT
    fn get_balance_map<BS: IpldStore + Copy>(
        &self,
        bs: &BS,
    ) -> Result<Hamt<BS, BigIntDe, ActorID>> {
        match Hamt::<BS, BigIntDe, ActorID>::load(&self.balances, *bs) {
            Ok(map) => Ok(map),
            Err(err) => return Err(anyhow!("Failed to load balances hamt: {:?}", err)),
        }
    }

    /// Increase the total supply by the specified value
    ///
    /// The requested amount must be non-negative.
    /// Returns an error if the total supply overflows, else returns the new total supply
    pub fn increase_supply(&mut self, value: &TokenAmount) -> Result<TokenAmount> {
        let new_supply = self.supply.checked_add(&value).ok_or_else(|| {
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
    pub fn get_allowance_between<BS: IpldStore + Copy>(
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
    pub fn increase_allowance<BS: IpldStore + Copy>(
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
                Some(prev_allowance) => prev_allowance.0.checked_add(&value).ok_or_else(|| {
                    anyhow!(
                        "Overflow when adding {} to {}'s allowance of {}",
                        value,
                        spender,
                        prev_allowance.0
                    )
                })?,
                None => value.clone(),
            };

            // TODO: should this be set as a BigIntSer rather than BigIntDe?
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
            Hamt::<BS, BigIntDe, ActorID>::new_with_bit_width(*bs, HAMT_BIT_WIDTH);
        owner_allowances.set(spender, BigIntDe(value.clone()))?;

        {
            // TODO: helper functions for saving hamts?, this can probably be done more efficiently rather than
            // getting the root allowance map again, by abstracting the nested hamt structure
            let mut root_allowance_map = self.get_allowances_map(bs)?;
            root_allowance_map.set(owner, owner_allowances.flush()?)?;
            self.allowances = root_allowance_map.flush()?;
        }

        return Ok((*value).clone());
    }

    /// Decrease the allowance between owner and spender by the specified value. If the resulting
    /// allowance is negative, it is set to zero.
    ///
    /// Caller must ensure that value is non-negative.
    ///
    /// If the allowance is decreased to zero, the entry is removed from the map.
    /// If the map is empty, it is removed from the root map.
    pub fn decrease_allowance<BS: IpldStore + Copy>(
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
                Some(prev_allowance) => prev_allowance.0.checked_sub(&value).ok_or_else(|| {
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

            // TODO: should this be set as a BigIntSer rather than BigIntDe?
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
    pub fn revoke_allowance<BS: IpldStore + Copy>(
        &self,
        bs: &BS,
        owner: ActorID,
        spender: ActorID,
    ) -> Result<()> {
        let allowance_map = self.get_owner_allowance_map(bs, owner)?;
        if let Some(mut map) = allowance_map {
            map.delete(&spender)?;
        }

        Ok(())
    }

    /// Get the allowances map of a specific actor, resolving the CID link to a Hamt
    ///
    /// Ok(Some) if the owner has allocated allowances to other actors
    /// Ok(None) if the owner has no current non-zero allowances to other actors
    /// Err if operations on the underlying Hamt failed
    fn get_owner_allowance_map<BS: IpldStore + Copy>(
        &self,
        bs: &BS,
        owner: ActorID,
    ) -> Result<Option<Hamt<BS, BigIntDe, ActorID>>> {
        let allowances_map = self.get_allowances_map(bs)?;
        let owner_allowances = match allowances_map.get(&owner)? {
            Some(cid) => Some(Hamt::<BS, BigIntDe, ActorID>::load(cid, *bs)?),
            None => None,
        };
        Ok(owner_allowances)
    }

    /// Get the global allowances map
    ///
    /// Gets a HAMT with CIDs linking to other HAMTs
    fn get_allowances_map<BS: IpldStore + Copy>(&self, bs: &BS) -> Result<Hamt<BS, Cid, ActorID>> {
        Hamt::<BS, Cid, ActorID>::load(&self.allowances, *bs)
            .map_err(|e| anyhow!("Failed to load base allowances map {}", e))
    }

    /// TODO: docs
    pub fn attempt_burn<BS: IpldStore>(
        &self,
        _bs: BS,
        _target: u64,
        _value: &TokenAmount,
    ) -> Result<TokenAmount> {
        todo!()
    }

    /// TODO: docs
    pub fn attempt_use_allowance<BS: IpldStore>(
        &self,
        _bs: BS,
        _operator: u64,
        _target: u64,
        _value: &TokenAmount,
    ) -> Result<TokenAmount> {
        todo!()
    }

    // fn enough_allowance(
    //     &self,
    //     bs: &Blockstore,
    //     from: ActorID,
    //     spender: ActorID,
    //     to: ActorID,
    //     amount: &TokenAmount,
    // ) -> std::result::Result<(), TokenAmountDiff> {
    //     if spender == from {
    //         return std::result::Result::Ok(());
    //     }

    //     let allowances = self.get_actor_allowance_map(bs, from);
    //     let allowance = match allowances.get(&to) {
    //         Ok(Some(amount)) => amount.0.clone(),
    //         _ => TokenAmount::zero(),
    //     };

    //     if allowance.lt(&amount) {
    //         Err(TokenAmountDiff {
    //             actual: allowance,
    //             required: amount.clone(),
    //         })
    //     } else {
    //         std::result::Result::Ok(())
    //     }
    // }

    // fn enough_balance(
    //     &self,
    //     bs: &Blockstore,
    //     from: ActorID,
    //     amount: &TokenAmount,
    // ) -> std::result::Result<(), TokenAmountDiff> {
    //     let balances = self.get_balance_map(bs);
    //     let balance = match balances.get(&from) {
    //         Ok(Some(amount)) => amount.0.clone(),
    //         _ => TokenAmount::zero(),
    //     };

    //     if balance.lt(&amount) {
    //         Err(TokenAmountDiff {
    //             actual: balance,
    //             required: amount.clone(),
    //         })
    //     } else {
    //         std::result::Result::Ok(())
    //     }
    // }

    // /// Atomically make a transfer
    // fn make_transfer(
    //     &self,
    //     bs: &Blockstore,
    //     amount: &TokenAmount,
    //     from: ActorID,
    //     spender: ActorID,
    //     to: ActorID,
    // ) -> TransferResult<TokenAmount> {
    //     if let Err(e) = self.enough_allowance(bs, from, spender, to, amount) {
    //         return Err(TransferError::InsufficientAllowance(e));
    //     }
    //     if let Err(e) = self.enough_balance(bs, from, amount) {
    //         return Err(TransferError::InsufficientBalance(e));
    //     }

    //     // Decrease allowance, decrease balance
    //     // From the above checks, we know these exist
    //     // TODO: do this in a transaction to avoid re-entrancy bugs
    //     let mut allowances = self.get_actor_allowance_map(bs, from);
    //     let allowance = allowances.get(&to).unwrap().unwrap();
    //     let new_allowance = allowance.0.clone().sub(amount);
    //     allowances.set(to, BigIntDe(new_allowance)).unwrap();

    //     let mut balances = self.get_balance_map(bs);
    //     let sender_balance = balances.get(&from).unwrap().unwrap();
    //     let new_sender_balance = sender_balance.0.clone().sub(amount);
    //     balances.set(from, BigIntDe(new_sender_balance)).unwrap();

    //     // TODO: call the receive hook

    //     // TODO: if no hook, revert the balance and allowance change

    //     // if successful, mark the balance as having been credited

    //     let receiver_balance = balances.get(&to).unwrap().unwrap();
    //     let new_receiver_balance = receiver_balance.0.clone().add(amount);
    //     balances.set(to, BigIntDe(new_receiver_balance)).unwrap();

    //     Ok(amount.clone())
    // }
}

impl Cbor for TokenState {}
