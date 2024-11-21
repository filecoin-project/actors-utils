//! Abstraction of the on-chain state related to NFT accounting
use std::collections::HashMap;
use std::mem;
use std::vec;

use cid::Cid;
use fvm_actor_utils::receiver::ReceiverHookError;
use fvm_ipld_amt::Amt;
use fvm_ipld_amt::Error as AmtError;
use fvm_ipld_bitfield::BitField;
use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::RawBytes;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::BytesKey;
use fvm_ipld_hamt::Error as HamtError;
use fvm_ipld_hamt::Hamt;
use fvm_shared::ActorID;
use integer_encoding::VarInt;
use multihash_codetable::Code;
use thiserror::Error;

use crate::types::ActorIDSet;
use crate::types::MintIntermediate;
use crate::types::MintReturn;
use crate::types::TokenID;
use crate::types::TokenSet;
use crate::types::TransferIntermediate;
use crate::types::TransferReturn;
use crate::util::OperatorSet;

/// Opaque cursor to iterate over internal data structures.
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct Cursor {
    pub root: Cid,
    pub index: u64,
}

impl Cursor {
    fn new(cid: Cid, index: u64) -> Self {
        Self { root: cid, index }
    }
}

/// Each token stores its owner, approved operators etc.
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TokenData {
    pub owner: ActorID,
    // operators on this token
    pub operators: BitField, // or maybe as a Cid to an Amt
    pub metadata: String,
}

/// Each owner stores their own balance and other indexed data.
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Clone, Debug)]
pub struct OwnerData {
    pub balance: u64,
    // account-level operators
    pub operators: BitField, // maybe as a Cid to an Amt
}

/// NFT state IPLD structure.
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct NFTState {
    /// [`Amt<TokenId, TokenData>`] encodes information per token - ownership, operators, metadata
    /// etc.
    pub token_data: Cid,
    /// [`Hamt<ActorID, OwnerData>`] index for faster lookup of data often queried by owner.
    pub owner_data: Cid,
    /// The next available token id for minting.
    pub next_token: TokenID,
    /// The number of minted tokens less the number of burned tokens.
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
    #[error("receiver hook error: {0}")]
    ReceiverHook(#[from] ReceiverHookError),
    #[error("invalid cursor")]
    InvalidCursor,
    /// This error is returned for errors that should never happen.
    #[error("invariant failed: {0}")]
    InvariantFailed(String),
}

impl NFTState {
    /// Create a new NFT state-tree, without committing it (the root Cid) to a blockstore.
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

    /// Retrieves the token data amt, asserting that the cursor is valid for the current state. If
    /// the root cid has changed since the cursor was created, the data has mutated and the cursor
    /// is invalid.
    pub fn get_token_amt_for_cursor<'bs, BS: Blockstore>(
        &self,
        store: &'bs BS,
        cursor: &Option<Cursor>,
    ) -> Result<Amt<TokenData, &'bs BS>> {
        if let Some(cursor) = cursor {
            if cursor.root != self.token_data {
                return Err(StateError::InvalidCursor);
            }
        }
        self.get_token_data_amt(store)
    }
}

impl NFTState {
    /// Mint a new token to the specified address.
    pub fn mint_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        initial_owner: ActorID,
        metadatas: Vec<String>,
    ) -> Result<MintIntermediate> {
        let first_token_id = self.next_token;
        let num_to_mint = metadatas.len();

        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        // update owner data map
        let new_owner_data = match owner_map.get(&actor_id_key(initial_owner)) {
            Ok(entry) => {
                if let Some(existing_data) = entry {
                    //TODO: a move or replace here may avoid the clone (which may be expensive on the vec)
                    OwnerData {
                        balance: existing_data.balance + metadatas.len() as u64,
                        ..existing_data.clone()
                    }
                } else {
                    OwnerData { balance: metadatas.len() as u64, operators: BitField::default() }
                }
            }
            Err(e) => return Err(e.into()),
        };
        owner_map.set(actor_id_key(initial_owner), new_owner_data)?;

        // update token data array
        for mut metadata in metadatas {
            let token_id = self.next_token;
            token_array.set(
                token_id,
                TokenData {
                    owner: initial_owner,
                    operators: BitField::default(),
                    metadata: mem::take(&mut metadata),
                },
            )?;
            self.next_token += 1;
        }

        // update global trackers
        self.total_supply += num_to_mint as u64;
        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        // params for constructing our return value
        Ok(MintIntermediate {
            to: initial_owner,
            recipient_data: RawBytes::default(),
            token_ids: (first_token_id..self.next_token).collect(),
        })
    }

    /// Get the number of tokens owned by a particular address.
    pub fn get_balance<BS: Blockstore>(&self, bs: &BS, owner: ActorID) -> Result<u64> {
        let owner_data = self.get_owner_data_hamt(bs)?;
        let balance = match owner_data.get(&actor_id_key(owner))? {
            Some(data) => data.balance,
            None => 0,
        };

        Ok(balance)
    }

    /// Approves an operator to transfer a set of specified tokens.
    ///
    /// The caller should own the tokens or an account-level operator on the owner of the tokens.
    pub fn approve_for_tokens<F, BS: Blockstore>(
        &mut self,
        bs: &BS,
        operator: ActorID,
        token_ids: &[TokenID],
        approve_predicate: F,
    ) -> Result<()>
    where
        F: Fn(&TokenData, TokenID) -> Result<()>,
    {
        let mut token_array = self.get_token_data_amt(bs)?;

        for &token_id in token_ids {
            let mut token_data =
                token_array.get(token_id)?.ok_or(StateError::TokenNotFound(token_id))?.clone();
            approve_predicate(&token_data, token_id)?;
            token_data.operators.add_operator(operator);
            token_array.set(token_id, token_data)?;
        }

        self.token_data = token_array.flush()?;

        Ok(())
    }

    /// Revokes an operator's permission to transfer the specified tokens.
    ///
    /// The caller should own the tokens or be an account-level operator on the owner of the tokens.
    pub fn revoke_for_tokens<F, BS: Blockstore>(
        &mut self,
        bs: &BS,
        operator: ActorID,
        token_ids: &[TokenID],
        revoke_predicate: F,
    ) -> Result<()>
    where
        F: Fn(&TokenData, TokenID) -> Result<()>,
    {
        let mut token_array = self.get_token_data_amt(bs)?;
        for &token_id in token_ids {
            let mut token_data =
                token_array.get(token_id)?.ok_or(StateError::TokenNotFound(token_id))?.clone();
            revoke_predicate(&token_data, token_id)?;
            token_data.operators.remove_operator(&operator);
            token_array.set(token_id, token_data)?;
        }

        self.token_data = token_array.flush()?;

        Ok(())
    }

    /// Approves an operator to transfer tokens on behalf of the owner.
    ///
    /// The operator becomes authorized at account level, meaning all tokens owned by the account
    /// can be transferred, approved or burned by the operator, including future tokens owned by the
    /// account.
    ///
    /// The caller should be the owning account.
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
                operators.add_operator(operator);
                OwnerData { operators, balance: data.balance }
            }
            None => OwnerData { balance: 0, operators: BitField::default() },
        };
        owner_map.set(actor_id_key(owner), new_owner_data)?;

        self.owner_data = owner_map.flush()?;

        Ok(())
    }

    /// Revokes an operator's authorization to transfer tokens on behalf of the owner account.
    ///
    /// The caller should be the owner of the account.
    pub fn revoke_for_all<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        operator: ActorID,
    ) -> Result<()> {
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        let new_owner_data = owner_map.get(&actor_id_key(owner))?.map(|existing_data| {
            let mut operators = existing_data.operators.clone();
            operators.remove_operator(&operator);
            OwnerData { balance: existing_data.balance, operators }
        });

        if let Some(data) = new_owner_data {
            let actor_key = actor_id_key(owner);
            if data.balance == 0 && data.operators.is_empty() {
                owner_map.delete(&actor_key)?;
            } else {
                owner_map.set(actor_key, data)?;
            }
        }

        self.owner_data = owner_map.flush()?;

        Ok(())
    }

    /// Burns a set of tokens, removing them from circulation and deleting associated metadata.
    ///
    /// If any of the token_ids cannot be burned (e.g. non-existent, already burned), the entire
    /// transaction should fail atomically.
    ///
    /// The tokens must all be owned by the same owner.
    ///
    /// Returns the new balance of the owner if all tokens were burned successfully.
    pub fn burn_tokens<F, BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        token_ids: &[TokenID],
        burn_predicate: F,
    ) -> Result<u64>
    where
        F: Fn(&TokenData, TokenID) -> Result<()>,
    {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for &token_id in token_ids {
            let token_data =
                token_array.delete(token_id)?.ok_or(StateError::TokenNotFound(token_id))?;
            burn_predicate(&token_data, token_id)?;
        }

        // we only reach here if all tokens were burned successfully so assume the caller is valid
        let owner_key = actor_id_key(owner);
        let mut new_owner_data = owner_map
            .get(&owner_key)?
            .ok_or_else(|| StateError::InvariantFailed("owner of tokens not found".into()))?
            .clone();
        let new_balance = new_owner_data.balance - token_ids.len() as u64;

        // update the owner's balance
        new_owner_data.balance = new_balance;
        if new_owner_data.balance == 0 && new_owner_data.operators.is_empty() {
            owner_map.delete(&owner_key)?;
        } else {
            owner_map.set(owner_key, new_owner_data)?;
        }

        self.total_supply -= token_ids.len() as u64;
        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        Ok(new_balance)
    }

    /// Transfers a batch of tokens between the owner and receiver.
    ///
    /// The predicate is checked for each token to be transferred, and the entire transfer is
    /// aborted if the predicate fails. It is the caller's responsibility to check that the
    /// actor using this method is permitted to do so.
    pub fn transfer<F, BS: Blockstore>(
        &mut self,
        bs: &BS,
        token_ids: &[TokenID],
        owner: ActorID,
        receiver: ActorID,
        transfer_predicate: &F,
    ) -> Result<TransferIntermediate>
    where
        F: Fn(&TokenData, TokenID) -> Result<()>,
    {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for &token_id in token_ids {
            // update the token_data to reflect the new owner and clear approved operators
            self.make_transfer(
                &mut token_array,
                &mut owner_map,
                token_id,
                receiver,
                transfer_predicate,
            )?;
        }

        Ok(TransferIntermediate {
            token_ids: token_ids.into(),
            from: owner,
            to: receiver,
            recipient_data: RawBytes::default(),
        })
    }

    /// Makes a transfer of a token from one address to another. The caller must verify that such a
    /// transfer is allowed.
    fn make_transfer<F, BS: Blockstore>(
        &mut self,
        token_array: &mut Amt<TokenData, &BS>,
        owner_map: &mut Hamt<&BS, OwnerData>,
        token_id: TokenID,
        receiver: ActorID,
        transfer_predicate: &F,
    ) -> Result<()>
    where
        F: Fn(&TokenData, TokenID) -> Result<()>,
    {
        let old_token_data =
            token_array.get(token_id)?.ok_or(StateError::TokenNotFound(token_id))?.clone();
        // check the transfer against business rules
        transfer_predicate(&old_token_data, token_id)?;

        let new_token_data =
            TokenData { owner: receiver, operators: BitField::default(), ..old_token_data };
        token_array.set(token_id, new_token_data)?;

        let previous_owner_key = actor_id_key(old_token_data.owner);
        let previous_owner_data = owner_map
            .get(&previous_owner_key)?
            .ok_or_else(|| {
                StateError::InvariantFailed(format!("owner of token {token_id} not found"))
            })?
            .clone();
        let previous_owner_data =
            OwnerData { balance: previous_owner_data.balance - 1, ..previous_owner_data };

        if previous_owner_data.balance == 0 && previous_owner_data.operators.is_empty() {
            owner_map.delete(&previous_owner_key)?;
        } else {
            owner_map.set(previous_owner_key, previous_owner_data)?;
        }

        let new_owner_key = actor_id_key(receiver);
        let new_owner_data = match owner_map.get(&new_owner_key)? {
            Some(data) => OwnerData { balance: data.balance + 1, ..data.clone() },
            None => OwnerData { balance: 1, operators: BitField::default() },
        };
        owner_map.set(new_owner_key, new_owner_data)?;

        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        Ok(())
    }

    /// Checks for account-level approval between owner and operator.
    pub fn is_account_operator<BS: Blockstore>(
        owner_map: &Hamt<&BS, OwnerData>,
        owner: ActorID,
        operator: ActorID,
    ) -> Result<bool> {
        let owner_data = owner_map
            .get(&actor_id_key(owner))?
            .ok_or_else(|| StateError::InvariantFailed(format!("owner {owner} not found")))?;
        Ok(owner_data.operators.contains_actor(&operator))
    }

    /// Asserts that the actor either owns the token or is an account level operator on the owner of
    /// the token.
    pub fn assert_can_approve_token(
        token_data: &TokenData,
        actor: ActorID,
        token_id: TokenID,
    ) -> Result<()> {
        if token_data.owner != actor {
            return Err(StateError::NotOwner { actor, token_id });
        }

        Ok(())
    }

    /// Asserts that the given operator is permitted authorised for the specified token.
    pub fn assert_token_level_approval(
        token_data: &TokenData,
        token_id: TokenID,
        operator: ActorID,
    ) -> Result<()> {
        // operator is approved at token-level
        if !token_data.operators.contains_actor(&operator) {
            return Err(StateError::NotAuthorized { actor: operator, token_id });
        }

        Ok(())
    }

    /// Asserts that a given account owns the specified token.
    pub fn assert_owns_token(
        token_data: &TokenData,
        token_id: TokenID,
        actor: ActorID,
    ) -> Result<()> {
        if token_data.owner != actor {
            return Err(StateError::NotOwner { actor, token_id });
        }

        Ok(())
    }

    /// Converts a [`MintIntermediate`] to a [`MintReturn`].
    ///
    /// This function should be called on a freshly loaded or known-up-to-date state.
    pub fn mint_return<BS: Blockstore>(
        &self,
        bs: &BS,
        intermediate: MintIntermediate,
    ) -> Result<MintReturn> {
        let balance = self.get_balance(bs, intermediate.to)?;

        Ok(MintReturn {
            balance,
            supply: self.total_supply,
            token_ids: intermediate.token_ids,
            recipient_data: intermediate.recipient_data,
        })
    }

    /// Converts a [`TransferIntermediate`] to a [`TransferReturn`].
    ///
    /// This function should be called on a freshly loaded or known-up-to-date state.
    pub fn transfer_return<BS: Blockstore>(
        &self,
        bs: &BS,
        intermediate: TransferIntermediate,
    ) -> Result<TransferReturn> {
        // TODO: optimise a pattern to avoid reading the owner data hamt twice
        let to_balance = self.get_balance(bs, intermediate.to)?;
        let from_balance = self.get_balance(bs, intermediate.from)?;
        Ok(TransferReturn { from_balance, to_balance, token_ids: intermediate.token_ids })
    }

    /// Get the metadata for a token.
    pub fn get_metadata<BS: Blockstore>(&self, bs: &BS, token_id: TokenID) -> Result<String> {
        let token_data_array = self.get_token_data_amt(bs)?;
        let token = token_data_array.get(token_id)?.ok_or(StateError::TokenNotFound(token_id))?;
        Ok(token.metadata.clone())
    }

    /// Get the owner of a token.
    pub fn get_owner<BS: Blockstore>(&self, bs: &BS, token_id: TokenID) -> Result<ActorID> {
        let token_data_array = self.get_token_data_amt(bs)?;
        let token = token_data_array.get(token_id)?.ok_or(StateError::TokenNotFound(token_id))?;
        Ok(token.owner)
    }

    /// List all the minted tokens.
    pub fn list_tokens<BS: Blockstore>(
        &self,
        bs: &BS,
        cursor: Option<Cursor>,
        limit: u64,
    ) -> Result<(TokenSet, Option<Cursor>)> {
        let token_data_array = self.get_token_amt_for_cursor(bs, &cursor)?;
        // Build the TokenSet
        let mut token_ids = TokenSet::new();
        let (_, next_key) =
            token_data_array.for_each_ranged(cursor.map(|r| r.index), Some(limit), |i, _| {
                token_ids.set(i);
                Ok(())
            })?;

        let next_cursor = next_key.map(|key| Cursor::new(self.token_data, key));
        Ok((token_ids, next_cursor))
    }

    /// List the tokens owned by an actor. In the reference implementation this is may be a
    /// prohibitievly expensive operation as it involves iterating over the entire token set.
    /// Returns a bitfield of the tokens owned by the actor and a cursor to the next page of data.
    pub fn list_owned_tokens<BS: Blockstore>(
        &self,
        bs: &BS,
        owner: ActorID,
        cursor: Option<Cursor>,
        limit: u64,
    ) -> Result<(TokenSet, Option<Cursor>)> {
        let token_data_array = self.get_token_amt_for_cursor(bs, &cursor)?;

        // Build the TokenSet
        let mut token_ids = TokenSet::new();
        let (_, next_key) =
            token_data_array.for_each_ranged(cursor.map(|r| r.index), Some(limit), |i, data| {
                if data.owner == owner {
                    token_ids.set(i);
                }
                Ok(())
            })?;

        let next_cursor = next_key.map(|key| Cursor::new(self.token_data, key));
        Ok((token_ids, next_cursor))
    }

    /// List all the token operators for a given `token_id`.
    pub fn list_token_operators<BS: Blockstore>(
        &self,
        bs: &BS,
        token_id: TokenID,
        cursor: Option<Cursor>,
        limit: u64,
    ) -> Result<(ActorIDSet, Option<Cursor>)> {
        let token_data_array = self.get_token_amt_for_cursor(bs, &cursor)?;
        let token_data =
            token_data_array.get(token_id)?.ok_or(StateError::TokenNotFound(token_id))?;

        let range_start = cursor.map(|c| c.index).unwrap_or(0);
        let mut actor_set = ActorIDSet::new();
        token_data.operators.iter().skip(range_start as usize).take(limit as usize).for_each(
            |operator| {
                actor_set.set(operator);
            },
        );

        let next_cursor = match token_data.operators.len() > range_start + limit {
            true => Some(Cursor::new(self.token_data, range_start + limit)),
            false => None,
        };

        Ok((actor_set, next_cursor))
    }

    /// Enumerates tokens for which an account is an operator.
    pub fn list_operator_tokens<BS: Blockstore>(
        &self,
        bs: &BS,
        operator: ActorID,
        cursor: Option<Cursor>,
        limit: u64,
    ) -> Result<(TokenSet, Option<Cursor>)> {
        let token_data_array = self.get_token_amt_for_cursor(bs, &cursor)?;

        // Build the TokenSet
        let mut operatable_tokens = TokenSet::new();
        let (_, next_key) =
            token_data_array.for_each_ranged(cursor.map(|r| r.index), Some(limit), |i, data| {
                if data.operators.get(operator) {
                    operatable_tokens.set(i);
                }
                Ok(())
            })?;

        let next_cursor = next_key.map(|key| Cursor::new(self.token_data, key));
        Ok((operatable_tokens, next_cursor))
    }

    /// List all the token operators for a given account.
    pub fn list_account_operators<BS: Blockstore>(
        &self,
        bs: &BS,
        actor_id: ActorID,
        cursor: Option<Cursor>,
        limit: u64,
    ) -> Result<(ActorIDSet, Option<Cursor>)> {
        let owner_data_map = self.get_owner_data_hamt(bs)?;
        let account = owner_data_map.get(&actor_id_key(actor_id))?;
        match account {
            Some(account) => {
                let mut operator_set = ActorIDSet::new();
                let range_start = cursor.map(|c| c.index).unwrap_or(0);

                account.operators.iter().skip(range_start as usize).take(limit as usize).for_each(
                    |operator| {
                        operator_set.set(operator);
                    },
                );

                let next_cursor = match account.operators.len() > range_start + limit {
                    true => Some(Cursor::new(self.token_data, range_start + limit)),
                    false => None,
                };

                Ok((operator_set, next_cursor))
            }
            None => Ok((ActorIDSet::new(), None)),
        }
    }
}

pub struct StateSummary {
    pub total_supply: u64,
    pub owner_data: Option<HashMap<ActorID, OwnerData>>,
    pub token_data: Option<HashMap<TokenID, TokenData>>,
}

#[derive(Error, Debug)]
pub enum StateInvariantError {
    #[error(
        "the total supply {total_supply:?} does not match the number of toknens recorded{token_count:?}"
    )]
    TotalSupplyMismatch { total_supply: u64, token_count: u64 },
    #[error(
        "the token array recorded {token_count:?} tokens but the owner map recorded {owner_count:?} owners"
    )]
    TokenBalanceMismatch { token_count: u64, owner_count: u64 },
    #[error("invalid serialized owner key {0:?}")]
    InvalidBytesKey(BytesKey),
    #[error("actorids stored in operator array were not strictly increasing {0:?}")]
    InvalidOperatorArray(Vec<ActorID>),
    #[error("underlying state error {0}")]
    State(#[from] StateError),
    #[error("entry for {0:?} in owner map had no tokens and no operators")]
    ExplicitEmptyOwner(u64),
}

impl NFTState {
    /**
     * Checks that the state is internally consistent and obeys the specified invariants
     *
     * Checks that balances in the TokenArray and OwnerMap are consistent. Checks that the total supply
     * is consistent with the number of tokens in the TokenArray. Checks that the OwnerHamt is clear of
     * semantically empty entries. Checks that all bytes keys are valid actor ids.
     *
     * Returns a state summary that can be used to check application specific invariants and a list
     * of errors that were found.
     */
    pub fn check_invariants<BS: Blockstore>(
        &self,
        bs: &BS,
    ) -> (StateSummary, Vec<StateInvariantError>) {
        // accumulate errors encountered in the state
        let mut errors: Vec<StateInvariantError> = vec![];

        // get token data
        let token_data = match self.get_token_data_amt(bs) {
            Ok(token_amt) => Some(token_amt),
            Err(e) => {
                errors.push(e.into());
                None
            }
        };

        // get owner data
        let owner_data = match self.get_owner_data_hamt(bs) {
            Ok(owner_hamt) => Some(owner_hamt),
            Err(e) => {
                errors.push(e.into());
                None
            }
        };

        // there's no point continuing if either are missing as something serious is wrong
        // we can't do meaningful state checks without the underlying data being loadable
        if owner_data.is_none() || token_data.is_none() {
            return (
                StateSummary {
                    owner_data: None,
                    token_data: None,
                    total_supply: self.total_supply,
                },
                errors,
            );
        }

        let owner_data = owner_data.unwrap();
        let token_data = token_data.unwrap();

        // check the total supply matches the number of NFTs stored
        if self.total_supply != token_data.count() {
            errors.push(StateInvariantError::TotalSupplyMismatch {
                total_supply: self.total_supply,
                token_count: token_data.count(),
            });
        }

        // tally the ownership of each token to check for consistency against owner_data
        let mut counted_balances = HashMap::<ActorID, u64>::new();

        let mut token_map = HashMap::<TokenID, TokenData>::new();
        token_data
            .for_each(|id, data| {
                // tally owner of token
                let owner = data.owner;
                let count = counted_balances.entry(owner).or_insert(0);
                *count += 1;

                token_map.insert(id, data.clone());
                Ok(())
            })
            .unwrap();

        let mut owner_map = HashMap::<ActorID, OwnerData>::new();
        // check owner data is consistent with token data
        owner_data
            .for_each(|owner_key, data| {
                if let Some(actor_id) = Self::decode_key_addr(owner_key, &mut errors) {
                    // assert balance matches the balance derived from the token array
                    let expected_balance = counted_balances.get(&actor_id).unwrap_or(&0);
                    if *expected_balance != data.balance {
                        errors.push(StateInvariantError::TokenBalanceMismatch {
                            token_count: *expected_balance,
                            owner_count: data.balance,
                        });
                    }

                    // if balance is zero and there are no operators, there should be no entry in the owner map
                    if data.balance == 0 && data.operators.is_empty() {
                        errors.push(StateInvariantError::ExplicitEmptyOwner(actor_id));
                    }

                    owner_map.insert(actor_id, data.clone());
                } else {
                    errors.push(StateInvariantError::InvalidBytesKey(owner_key.clone()));
                }

                Ok(())
            })
            .unwrap();

        (
            StateSummary {
                owner_data: Some(owner_map),
                token_data: Some(token_map),
                total_supply: self.total_supply,
            },
            errors,
        )
    }

    /// Helper to decode keys from bytes, recording errors if they fail.
    fn decode_key_addr(key: &BytesKey, errors: &mut Vec<StateInvariantError>) -> Option<ActorID> {
        match decode_actor_id(key) {
            Some(actor_id) => Some(actor_id),
            None => {
                errors.push(StateInvariantError::InvalidBytesKey(key.clone()));
                None
            }
        }
    }
}

pub fn actor_id_key(a: ActorID) -> BytesKey {
    a.encode_var_vec().into()
}

pub fn decode_actor_id(key: &BytesKey) -> Option<ActorID> {
    u64::decode_var(key.0.as_slice()).map(|a| a.0)
}
