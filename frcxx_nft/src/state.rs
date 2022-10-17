//! Abstraction of the on-chain state related to NFT accounting
use std::collections::HashSet;

use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_amt::Amt;
use fvm_ipld_amt::Error as AmtError;
use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::BytesKey;
use fvm_ipld_hamt::Error as HamtError;
use fvm_ipld_hamt::Hamt;
use fvm_shared::ActorID;
use integer_encoding::VarInt;
use thiserror::Error;

pub type TokenID = u64;

/// Each token stores its owner, approved operators etc.
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TokenData {
    pub owner: ActorID,
    // operators on this token
    pub operators: HashSet<ActorID>, // or maybe as a Cid to an Amt
    pub metadata_id: Cid,
}

/// Each owner stores their own balance and other indexed data
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct OwnerData {
    pub balance: u64,
    // account-level operators
    pub operators: HashSet<ActorID>, // maybe as a Cid to an Amt
}

/// NFT state IPLD structure
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct NFTState {
    /// Amt<TokenId, TokenData> encodes information per token - ownership, operators, metadata etc.
    pub token_data: Cid,
    /// Hamt<ActorID, OwnerData> index for faster lookup of data often queried by owner
    pub owner_data: Cid,
    /// The next available token id for minting
    pub next_token: TokenID,
    /// The number of minted tokens less the number of burned tokens
    pub total_supply: u64,
}

// TODO: benchmark and tune these values
const AMT_BIT_WIDTH: u32 = 5;
const HAMT_BIT_WIDTH: u32 = 3;

type Result<T> = std::result::Result<T, StateError>;

type Map<'bs, BS, K, V> = Hamt<&'bs BS, V, K>;
type OwnerMap<'bs, BS> = Map<'bs, BS, BytesKey, OwnerData>;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("ipld amt error: {0}")]
    IpldAmt(#[from] AmtError),
    #[error("ipld hamt error: {0}")]
    IpldHamt(#[from] HamtError),
    #[error("token id not found: {0}")]
    TokenNotFound(TokenID),
    #[error("actor {actor:?} is not the owner of the token {token_id:?}")]
    NotOwner { actor: ActorID, token_id: TokenID },
    #[error("actor {actor:?} is not authorized for token {token_id:?}")]
    NotAuthorized { actor: ActorID, token_id: TokenID },
    /// This error is returned for errors that should never happen
    #[error("invariant failed: {0}")]
    InvariantFailed(String),
}

impl NFTState {
    /// Create a new NFT state-tree, without committing it (the root Cid) to a blockstore
    pub fn new<BS: Blockstore>(store: &BS) -> Result<Self> {
        // Blockstore is still needed to create valid Cids for the Hamts
        let empty_token_array =
            Amt::<TokenData, &BS>::new_with_bit_width(store, AMT_BIT_WIDTH).flush()?;
        // Blockstore is still needed to create valid Cids for the Hamts
        let empty_owner_map =
            Hamt::<&BS, OwnerData, ActorID>::new_with_bit_width(store, HAMT_BIT_WIDTH).flush()?;

        Ok(Self {
            token_data: empty_token_array,
            owner_data: empty_owner_map,
            next_token: 0,
            total_supply: 0,
        })
    }

    pub fn load<BS: Blockstore>(store: &BS, root: &Cid) -> Result<Self> {
        match store.get_cbor::<Self>(root) {
            Ok(Some(state)) => Ok(state),
            Ok(None) => Err(StateError::InvariantFailed("State root not found".into())),
            Err(e) => Err(StateError::InvariantFailed(e.to_string())),
        }
    }

    pub fn save<BS: Blockstore>(&self, store: &BS) -> Result<Cid> {
        let serialized = match fvm_ipld_encoding::to_vec(self) {
            Ok(s) => s,
            Err(err) => return Err(StateError::InvariantFailed(err.to_string())),
        };
        let block = Block { codec: DAG_CBOR, data: serialized };
        let cid = match store.put(Code::Blake2b256, &block) {
            Ok(cid) => cid,
            Err(err) => return Err(StateError::InvariantFailed(err.to_string())),
        };
        Ok(cid)
    }

    pub fn get_token_data_amt<'bs, BS: Blockstore>(
        &self,
        store: &'bs BS,
    ) -> Result<Amt<TokenData, &'bs BS>> {
        let res = Amt::load(&self.token_data, store)?;
        Ok(res)
    }

    pub fn get_owner_data_hamt<'bs, BS: Blockstore>(
        &self,
        store: &'bs BS,
    ) -> Result<OwnerMap<'bs, BS>> {
        let res = OwnerMap::load_with_bit_width(&self.owner_data, store, HAMT_BIT_WIDTH)?;
        Ok(res)
    }

    /// Mint a new token to the specified address
    pub fn mint_token<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        metadata_id: Cid,
    ) -> Result<TokenID> {
        // update token data array
        let mut token_array = self.get_token_data_amt(bs)?;
        let token_id = self.next_token;
        token_array.set(token_id, TokenData { owner, operators: HashSet::new(), metadata_id })?;

        // update owner data map
        let mut owner_map = self.get_owner_data_hamt(bs)?;
        let new_owner_data = match owner_map.get(&actor_id_key(owner)) {
            Ok(entry) => {
                if let Some(existing_data) = entry {
                    //TODO: a move or replace here may avoid the clone (which may be expensive on the vec)
                    OwnerData { balance: existing_data.balance + 1, ..existing_data.clone() }
                } else {
                    OwnerData { balance: 1, operators: HashSet::new() }
                }
            }
            Err(e) => return Err(e.into()),
        };
        owner_map.set(actor_id_key(owner), new_owner_data)?;

        // update global trackers
        self.next_token += 1;
        self.total_supply += 1;

        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        // TODO: call receiver hook

        Ok(token_id)
    }

    /// Get the number of tokens owned by a particular address
    pub fn get_balance<BS: Blockstore>(&mut self, bs: &BS, owner: ActorID) -> Result<u64> {
        let owner_data = self.get_owner_data_hamt(bs)?;
        let balance = match owner_data.get(&actor_id_key(owner))? {
            Some(data) => data.balance,
            None => 0,
        };

        Ok(balance)
    }

    /// Approves an operator to transfer a set of specified tokens
    ///
    /// Checks that the caller is the owner of the specified token. If any of the token_ids is not
    /// valid (i.e. non-existent or not-owned by the caller), the entire batch approval is aborted.
    pub fn approve_for_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        caller: ActorID,
        operator: ActorID,
        token_ids: &[TokenID],
    ) -> Result<()> {
        let mut token_array = self.get_token_data_amt(bs)?;

        for token_id in token_ids {
            let mut token_data = Self::owns_token(&token_array, caller, *token_id)?;
            token_data.operators.insert(operator);
            token_array.set(*token_id, token_data)?;
        }

        self.token_data = token_array.flush()?;

        Ok(())
    }

    /// Revokes an operator to transfer a specific token
    ///
    /// Checks that the caller is the owner of the specified token. If any of the token_ids is not
    /// valid (i.e. non-existent or not-owned by the caller), the entire batch revoke is aborted.
    pub fn revoke_for_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        token_ids: &[TokenID],
        caller: ActorID,
        operator: ActorID,
    ) -> Result<()> {
        let mut token_array = self.get_token_data_amt(bs)?;

        for token_id in token_ids {
            let mut token_data = Self::owns_token(&token_array, caller, *token_id)?;
            token_data.operators.remove(&operator);

            token_array.set(*token_id, token_data)?;
        }

        self.token_data = token_array.flush()?;

        Ok(())
    }

    /// Approves an operator to transfer tokens on behalf of the owner
    ///
    /// The operator is authorized at the account level, meaning that all tokens owned by the owner
    /// can be transferred or burned by the operator including future tokens held by the account
    pub fn approve_for_owner<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        operator: ActorID,
    ) -> Result<()> {
        let mut owner_map = self.get_owner_data_hamt(bs)?;
        let new_owner_data = match owner_map.get(&actor_id_key(owner))? {
            Some(data) => {
                let mut operators = data.operators.clone();
                operators.insert(operator);
                OwnerData { operators, balance: data.balance }
            }
            None => OwnerData { balance: 0, operators: HashSet::new() },
        };
        owner_map.set(actor_id_key(owner), new_owner_data)?;
        self.owner_data = owner_map.flush()?;

        Ok(())
    }

    /// Revokes an operator's authorization to transfer tokens on behalf of the owner account
    pub fn revoke_for_all<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        operator: ActorID,
    ) -> Result<()> {
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        let new_owner_data = owner_map.get(&actor_id_key(owner))?.map(|existing_data| {
            let mut operators = existing_data.operators.clone();
            operators.remove(&operator);
            OwnerData { balance: existing_data.balance, operators }
        });

        if let Some(data) = new_owner_data {
            owner_map.set(actor_id_key(owner), data)?;
        }

        self.owner_data = owner_map.flush()?;

        Ok(())
    }

    /// Burns a set of token, removing them from circulation and deleting associated metadata
    ///
    /// If any of the token_ids is not valid (i.e. non-existent/already burned or not owned by the
    /// caller), the entire batch of burns fails
    pub fn burn_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        caller: ActorID,
        token_ids: &[TokenID],
    ) -> Result<()> {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for token_id in token_ids {
            Self::owns_token(&token_array, caller, *token_id)?;

            let token_data = token_array
                .delete(*token_id)?
                .ok_or_else(|| StateError::TokenNotFound(*token_id))?;

            let owner_key = actor_id_key(token_data.owner);
            let owner_data = owner_map.get(&owner_key)?.ok_or_else(|| {
                StateError::InvariantFailed(format!("owner of token {} not found", token_id))
            })?;

            // TODO: if balance goes to zero AND approved array is empty, delete the owner entry
            owner_map.set(
                owner_key,
                OwnerData {
                    balance: owner_data.balance - 1,
                    operators: owner_data.operators.clone(),
                },
            )?;
        }

        self.total_supply -= token_ids.len() as u64;
        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;
        Ok(())
    }

    /// Transfers a token, initiated by the owner
    pub fn transfer_token<BS: Blockstore>(
        &mut self,
        bs: &BS,
        caller: ActorID,
        to: ActorID,
        token_ids: &[TokenID],
    ) -> Result<()> {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for token_id in token_ids {
            let _token_data = Self::owns_token(&token_array, caller, *token_id)?;
            // update the token_data to reflect the new owner and clear approved operators
            self.make_transfer(&mut token_array, &mut owner_map, *token_id, to)?;
        }

        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        Ok(())
    }

    /// Transfers a token, initiated by an operator
    ///
    /// An operator is allowed to transfer a token that it has been explicitly approved for or a token
    /// owned by an account that it has been approved for.
    pub fn operator_transfer_token<BS: Blockstore>(
        &mut self,
        bs: &BS,
        operator: ActorID,
        to: ActorID,
        token_ids: &[TokenID],
    ) -> Result<()> {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for token_id in token_ids {
            if !Self::approved_for_token(&token_array, &owner_map, operator, *token_id)? {
                return Err(StateError::NotAuthorized { actor: operator, token_id: *token_id });
            }

            // update the token_data to reflect the new owner and clear approved operators
            self.make_transfer(&mut token_array, &mut owner_map, *token_id, to)?;
        }

        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        Ok(())
    }

    /// Makes a transfer of a token from one address to another. The caller must verify that such a
    /// transfer is allowed. This function does not flush the token AMT or the owner HAMT, it is the
    /// caller's responsibility to do so at the end of the batch.
    pub fn make_transfer<BS: Blockstore>(
        &mut self,
        token_array: &mut Amt<TokenData, &BS>,
        owner_map: &mut Hamt<&BS, OwnerData>,
        token_id: TokenID,
        receiver: ActorID,
    ) -> Result<()> {
        let old_token_data =
            token_array.get(token_id)?.ok_or_else(|| StateError::TokenNotFound(token_id))?.clone();
        let new_token_data =
            TokenData { owner: receiver, operators: HashSet::new(), ..old_token_data };
        token_array.set(token_id, new_token_data)?;

        let previous_owner_key = actor_id_key(old_token_data.owner);
        let previous_owner_data = owner_map
            .get(&previous_owner_key)?
            .ok_or_else(|| {
                StateError::InvariantFailed(format!("owner of token {} not found", token_id))
            })?
            .clone();
        let previous_owner_data =
            OwnerData { balance: previous_owner_data.balance - 1, ..previous_owner_data };
        owner_map.set(previous_owner_key, previous_owner_data)?;
        let new_owner_key = actor_id_key(receiver);
        let new_owner_data = match owner_map.get(&new_owner_key)? {
            Some(data) => OwnerData { balance: data.balance + 1, ..data.clone() },
            None => OwnerData { balance: 1, operators: HashSet::new() },
        };
        owner_map.set(new_owner_key, new_owner_data)?;

        Ok(())
    }

    /// Asserts that the actor owns the token and returns a copy of the TokenData
    ///
    /// Returns TokenNotFound if the token_id is invalid or NotOwner if the actor does not own
    /// own the token.
    pub fn owns_token<BS: Blockstore>(
        token_array: &Amt<TokenData, &BS>,
        actor: ActorID,
        token_id: TokenID,
    ) -> Result<TokenData> {
        let token_data =
            token_array.get(token_id)?.ok_or_else(|| StateError::TokenNotFound(token_id))?;
        match token_data.owner == actor {
            true => Ok(token_data.clone()),
            false => Err(StateError::NotOwner { actor, token_id }),
        }
    }

    /// Checks whether an operator is approved to transfer/burn a token
    pub fn approved_for_token<BS: Blockstore>(
        token_array: &Amt<TokenData, &BS>,
        owner_map: &Hamt<&BS, OwnerData>,
        operator: ActorID,
        token_id: TokenID,
    ) -> Result<bool> {
        let token_data = token_array
            .get(token_id)?
            .ok_or_else(|| StateError::InvariantFailed(format!("token {} not found", token_id)))?;

        // operator is approved at token-level
        if token_data.operators.contains(&operator) {
            return Ok(true);
        }

        // operator is approved at account-level
        let owner_account = owner_map.get(&actor_id_key(token_data.owner))?.ok_or_else(|| {
            StateError::InvariantFailed(format!("owner of token {} not found", token_id))
        })?;
        if owner_account.operators.contains(&operator) {
            return Ok(true);
        }

        Ok(false)
    }
}

pub fn actor_id_key(a: ActorID) -> BytesKey {
    a.encode_var_vec().into()
}

#[cfg(test)]
mod test {
    use cid::Cid;
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_shared::ActorID;

    use crate::{state::StateError, NFTState};

    const ALICE_ID: ActorID = 1;
    const BOB_ID: ActorID = 2;
    const CHARLIE_ID: ActorID = 3;

    #[test]
    fn it_mints_tokens_incrementally() {
        let bs = &MemoryBlockstore::new();
        let mut state = NFTState::new(bs).unwrap();

        // mint first token
        let token_id = state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        let balance = state.get_balance(bs, ALICE_ID).unwrap();
        // expect balance increase, token id increment
        assert_eq!(token_id, 0);
        assert_eq!(balance, 1);
        assert_eq!(state.total_supply, 1);

        // mint another token
        let token_id = state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        let balance = state.get_balance(bs, ALICE_ID).unwrap();
        // expect balance increase, token id increment
        assert_eq!(token_id, 1);
        assert_eq!(balance, 2);
        assert_eq!(state.total_supply, 2);

        // expect another actor to have zero balance by default
        let balance = state.get_balance(bs, BOB_ID).unwrap();
        assert_eq!(balance, 0);

        // mint another token to a different actor
        let token_id = state.mint_token(bs, BOB_ID, Cid::default()).unwrap();
        let alice_balance = state.get_balance(bs, ALICE_ID).unwrap();
        let bob_balance = state.get_balance(bs, BOB_ID).unwrap();
        // expect balance increase globally, token id increment
        assert_eq!(token_id, 2);
        assert_eq!(bob_balance, 1);
        assert_eq!(alice_balance, 2);
        assert_eq!(state.total_supply, 3);
    }

    #[test]
    fn it_burns_tokens() {
        let bs = &MemoryBlockstore::new();
        let mut state = NFTState::new(bs).unwrap();

        // mint a few tokens
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        assert_eq!(state.total_supply, 4);
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 4);

        // burn a non-existent token
        let err = state.burn_tokens(bs, ALICE_ID, &[99]).unwrap_err();
        if let StateError::TokenNotFound(token_id) = err {
            assert_eq!(token_id, 99);
        } else {
            panic!("unexpected error: {:?}", err);
        }
        assert_eq!(state.total_supply, 4);
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 4);

        // burn a token owned by alice
        state.burn_tokens(bs, ALICE_ID, &[0]).unwrap();
        // total supply and balance should decrease
        assert_eq!(state.total_supply, 3);
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);

        // attempt to burn multiple tokens owned by alice with one invalid token
        state.burn_tokens(bs, ALICE_ID, &[0, 1, 2]).unwrap_err();
        // total supply and balance should not change
        assert_eq!(state.total_supply, 3);
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);

        // attempt to burn multiple tokens owned by alice with one invalid token (invalid token at end)
        state.burn_tokens(bs, ALICE_ID, &[1, 2, 0]).unwrap_err();
        // total supply and balance should not change
        assert_eq!(state.total_supply, 3);
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);

        // attempt to burn multiple tokens owned by alice with duplicate
        state.burn_tokens(bs, ALICE_ID, &[1, 2, 1]).unwrap_err();
        // total supply and balance should not change
        assert_eq!(state.total_supply, 3);
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);

        // burn multiple tokens owned by alice
        state.burn_tokens(bs, ALICE_ID, &[1, 2]).unwrap();
        // total supply and balance should not change
        assert_eq!(state.total_supply, 1);
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 1);
    }

    #[test]
    fn it_transfers_tokens() {
        let bs = &MemoryBlockstore::new();
        let mut state = NFTState::new(bs).unwrap();

        // mint a few tokens
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();

        // bob cannot transfer from alice to himself
        let res = state.transfer_token(bs, BOB_ID, BOB_ID, &[0]).unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {:?}", res);
        }

        // alice can transfer to bob
        state.transfer_token(bs, ALICE_ID, BOB_ID, &[0]).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 2);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);

        // alice is unauthorized to transfer that token now
        let res = state.transfer_token(bs, ALICE_ID, ALICE_ID, &[0]).unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, ALICE_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {:?}", res);
        }
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 2);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);

        // but bob can transfer it back
        state.transfer_token(bs, BOB_ID, ALICE_ID, &[0]).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 0);

        // transferring a batch fails if any tokens is not valid
        state.transfer_token(bs, ALICE_ID, BOB_ID, &[1, 99]).unwrap_err();
        state.transfer_token(bs, ALICE_ID, BOB_ID, &[99, 1]).unwrap_err();
        // or there are duplicates
        let err = state.transfer_token(bs, ALICE_ID, BOB_ID, &[1, 1, 2]).unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = err {
            assert_eq!(operator, ALICE_ID);
            assert_eq!(token_id, 1);
        } else {
            panic!("unexpected error: {:?}", res);
        }
        // state unchanged
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 0);

        // alice can transfer other two in a batch
        state.transfer_token(bs, ALICE_ID, BOB_ID, &[1, 2]).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 1);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 2);
    }

    #[test]
    fn it_allows_account_level_delegation() {
        let bs = &MemoryBlockstore::new();
        let mut state = NFTState::new(bs).unwrap();

        // mint a few tokens
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();

        // bob cannot transfer from alice to himself
        let res = state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[0]).unwrap_err();
        if let StateError::NotAuthorized { actor: operator, token_id } = res {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {:?}", res);
        }

        // approve bob to transfer on behalf of alice
        state.approve_for_owner(bs, ALICE_ID, BOB_ID).unwrap();

        // bob can now transfer from alice to himself
        // but cannot use the incorrect method
        let res = state.transfer_token(bs, BOB_ID, ALICE_ID, &[0]).unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {:?}", res);
        }

        // using correct method succeeds
        state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[0]).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 2);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);
        assert_eq!(state.total_supply, 3);

        // alice is unauthorized to transfer that token now
        let res = state.transfer_token(bs, ALICE_ID, ALICE_ID, &[0]).unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, ALICE_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {:?}", res);
        }
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 2);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);

        // but bob can transfer it back
        state.transfer_token(bs, BOB_ID, ALICE_ID, &[0]).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 0);

        // a newly minted token after approval can be transferred by bob
        let new_token_id = state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[new_token_id]).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);

        // bob cannot transfer a batch if any of the tokens is invalid or duplicated
        state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[new_token_id]).unwrap_err();
        state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[0, 99]).unwrap_err();
        state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[0, 0]).unwrap_err();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);

        // bob can batch transfer tokens
        let new_token_a = state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        let new_token_b = state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        let new_token_c = state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 6);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);
        state
            .operator_transfer_token(bs, BOB_ID, BOB_ID, &[new_token_a, new_token_b, new_token_c])
            .unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 4);

        // bob's authorization can be revoked
        state.revoke_for_all(bs, ALICE_ID, BOB_ID).unwrap();
        let res = state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[new_token_id]).unwrap_err();
        if let StateError::NotAuthorized { actor: operator, token_id } = res {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, new_token_id);
        } else {
            panic!("unexpected error: {:?}", res);
        }

        // state didn't change
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 3);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 4);
    }

    #[test]
    fn it_allows_token_level_delegation() {
        let bs = &MemoryBlockstore::new();
        let mut state = NFTState::new(bs).unwrap();

        // mint a few tokens
        let token_0 = state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();
        let token_1 = state.mint_token(bs, ALICE_ID, Cid::default()).unwrap();

        // neither bob nor charlie can transfer either token
        state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[token_0]).unwrap_err();
        state.operator_transfer_token(bs, CHARLIE_ID, BOB_ID, &[token_0]).unwrap_err();
        state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[token_1]).unwrap_err();
        state.operator_transfer_token(bs, CHARLIE_ID, BOB_ID, &[token_1]).unwrap_err();
        // state didn't change
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 2);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 0);
        assert_eq!(state.get_balance(bs, CHARLIE_ID).unwrap(), 0);

        // charlie cannot not approve bob or charlie to a token owned by alice
        state.approve_for_tokens(bs, CHARLIE_ID, BOB_ID, &[token_0]).unwrap_err();
        let res = state.approve_for_tokens(bs, CHARLIE_ID, CHARLIE_ID, &[token_0]).unwrap_err();
        if let StateError::NotOwner { actor, token_id } = res {
            assert_eq!(actor, CHARLIE_ID);
            assert_eq!(token_id, token_0);
        } else {
            panic!("unexpected error: {:?}", res);
        }

        // alice approves bob and charlie as operators
        state.approve_for_tokens(bs, ALICE_ID, BOB_ID, &[token_0]).unwrap();
        state.approve_for_tokens(bs, ALICE_ID, BOB_ID, &[token_1]).unwrap();
        state.approve_for_tokens(bs, ALICE_ID, CHARLIE_ID, &[token_1]).unwrap();

        // charlie still can't transfer token_0
        let res =
            state.operator_transfer_token(bs, CHARLIE_ID, CHARLIE_ID, &[token_0]).unwrap_err();
        if let StateError::NotAuthorized { actor, token_id } = res {
            assert_eq!(actor, CHARLIE_ID);
            assert_eq!(token_id, token_0);
        } else {
            panic!("unexpected error: {:?}", res);
        }

        // but bob can transfer token_0
        state.operator_transfer_token(bs, BOB_ID, BOB_ID, &[token_0]).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 1);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);
        assert_eq!(state.get_balance(bs, CHARLIE_ID).unwrap(), 0);

        // charlie can transfer token_1
        state.operator_transfer_token(bs, CHARLIE_ID, CHARLIE_ID, &[token_1]).unwrap();
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 0);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);
        assert_eq!(state.get_balance(bs, CHARLIE_ID).unwrap(), 1);

        // but after that, bob can no longer transfer it (approvals were reset)
        state.operator_transfer_token(bs, BOB_ID, CHARLIE_ID, &[token_1]).unwrap_err();
        // state was unchanged
        assert_eq!(state.get_balance(bs, ALICE_ID).unwrap(), 0);
        assert_eq!(state.get_balance(bs, BOB_ID).unwrap(), 1);
        assert_eq!(state.get_balance(bs, CHARLIE_ID).unwrap(), 1);
    }
}
