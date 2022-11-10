//! Abstraction of the on-chain state related to NFT accounting
use std::collections::HashMap;
use std::mem;
use std::vec;

use cid::multihash::Code;
use cid::Cid;
use fvm_actor_utils::receiver::ReceiverHook;
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
use fvm_shared::address::Address;
use fvm_shared::ActorID;
use integer_encoding::VarInt;
use thiserror::Error;

use crate::receiver::FRCXXReceiverHook;
use crate::receiver::FRCXXTokenReceived;
use crate::types::MintIntermediate;
use crate::types::MintReturn;
use crate::types::TransferFromIntermediate;
use crate::types::TransferFromReturn;
use crate::types::TransferIntermediate;
use crate::types::TransferReturn;
use crate::util::OperatorSet;

pub type TokenID = u64;

/// Each token stores its owner, approved operators etc.
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TokenData {
    pub owner: ActorID,
    // operators on this token
    pub operators: BitField, // or maybe as a Cid to an Amt
    pub metadata: String,
}

/// Each owner stores their own balance and other indexed data
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Clone, Debug)]
pub struct OwnerData {
    pub balance: u64,
    // account-level operators
    pub operators: BitField, // maybe as a Cid to an Amt
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
    #[error("receiver hook error: {0}")]
    ReceiverHook(#[from] ReceiverHookError),
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
}

impl NFTState {
    /// Mint a new token to the specified address
    pub fn mint_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        caller: ActorID,
        owner: ActorID,
        metadatas: Vec<String>,
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<MintIntermediate>> {
        // update token data array
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        let first_token_id = self.next_token;

        for mut metadata in metadatas {
            let token_id = self.next_token;
            token_array.set(
                token_id,
                TokenData {
                    owner,
                    operators: BitField::default(),
                    metadata: mem::take(&mut metadata),
                },
            )?;
            // update owner data map
            let new_owner_data = match owner_map.get(&actor_id_key(owner)) {
                Ok(entry) => {
                    if let Some(existing_data) = entry {
                        //TODO: a move or replace here may avoid the clone (which may be expensive on the vec)
                        OwnerData { balance: existing_data.balance + 1, ..existing_data.clone() }
                    } else {
                        OwnerData { balance: 1, operators: BitField::default() }
                    }
                }
                Err(e) => return Err(e.into()),
            };
            owner_map.set(actor_id_key(owner), new_owner_data)?;

            // update global trackers
            self.next_token += 1;
            self.total_supply += 1;
        }

        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        // params for constructing our return value
        let mint_intermediate = MintIntermediate {
            to: owner,
            recipient_data: RawBytes::default(),
            token_ids: (first_token_id..self.next_token).collect(),
        };

        // params we'll send to the receiver hook
        let params = FRCXXTokenReceived {
            operator: caller,
            to: owner,
            operator_data,
            token_data,
            token_ids: mint_intermediate.token_ids.clone(),
        };

        Ok(ReceiverHook::new_frcxx(Address::new_id(owner), params, mint_intermediate)?)
    }

    /// Get the number of tokens owned by a particular address
    pub fn get_balance<BS: Blockstore>(&self, bs: &BS, owner: ActorID) -> Result<u64> {
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
    /// valid (i.e. non-existent or not-owned by the caller), the entire batch approval is aborted.. If any of the token_ids is not
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
            token_data.operators.add_operator(operator);
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
        caller: ActorID,
        operator: ActorID,
        token_ids: &[TokenID],
    ) -> Result<()> {
        let mut token_array = self.get_token_data_amt(bs)?;

        for token_id in token_ids {
            let mut token_data = Self::owns_token(&token_array, caller, *token_id)?;
            token_data.operators.remove_operator(&operator);
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
                operators.add_operator(operator);
                OwnerData { operators, balance: data.balance }
            }
            None => OwnerData { balance: 0, operators: BitField::default() },
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

    /// Burns a set of token, removing them from circulation and deleting associated metadata
    ///
    /// If any of the token_ids is not valid (i.e. non-existent/already burned or not owned by the
    /// caller), the entire batch of burns fails
    pub fn burn_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        caller: ActorID,
        token_ids: &[TokenID],
    ) -> Result<u64> {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for token_id in token_ids {
            Self::owns_token(&token_array, caller, *token_id)?;

            let _token_data = token_array
                .delete(*token_id)?
                .ok_or_else(|| StateError::TokenNotFound(*token_id))?;
        }

        // we only reach here if all tokens were burned successfully so assume the caller is valid
        let owner_key = actor_id_key(caller);
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

    /// Burns a set of token, removing them from circulation and deleting associated metadata.
    /// The caller must be an approved operator at the token or account level.
    ///
    /// If any of the token_ids is not valid (i.e. non-existent/already burned or not authorized for
    /// the caller), the entire batch of burns fails
    pub fn operator_burn_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        caller: u64,
        token_ids: &[u64],
    ) -> Result<()> {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for token_id in token_ids {
            if !Self::approved_for_token(&token_array, &owner_map, caller, *token_id)? {
                return Err(StateError::NotAuthorized { actor: caller, token_id: *token_id });
            }

            let token_data = token_array
                .delete(*token_id)?
                .ok_or_else(|| StateError::TokenNotFound(*token_id))?;

            let owner_key = actor_id_key(token_data.owner);
            let owner_data = owner_map.get(&owner_key)?.ok_or_else(|| {
                StateError::InvariantFailed(format!("owner of token {token_id} not found"))
            })?;

            if owner_data.balance == 1 && owner_data.operators.is_empty() {
                owner_map.delete(&owner_key)?;
            } else {
                owner_map.set(
                    owner_key,
                    OwnerData {
                        balance: owner_data.balance - 1,
                        operators: owner_data.operators.clone(),
                    },
                )?;
            }
        }

        self.total_supply -= token_ids.len() as u64;
        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;
        Ok(())
    }

    /// Transfers a set of token, initiated by the owner
    pub fn transfer_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        caller: ActorID,
        to: ActorID,
        token_ids: &[TokenID],
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferIntermediate>> {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for token_id in token_ids {
            let _token_data = Self::owns_token(&token_array, caller, *token_id)?;
            // update the token_data to reflect the new owner and clear approved operators
            self.make_transfer(&mut token_array, &mut owner_map, *token_id, to)?;
        }

        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        let params = FRCXXTokenReceived {
            to,
            operator: caller,
            token_ids: token_ids.into(),
            operator_data,
            token_data,
        };

        let res = TransferIntermediate {
            to,
            from: caller,
            token_ids: token_ids.into(),
            recipient_data: RawBytes::default(),
        };

        Ok(ReceiverHook::new_frcxx(Address::new_id(to), params, res)?)
    }

    /// Transfers a token, initiated by an operator
    ///
    /// An operator is allowed to transfer a token that it has been explicitly approved for or a token
    /// owned by an account that it has been approved for.
    pub fn operator_transfer_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        caller: ActorID,
        to: ActorID,
        token_ids: &[TokenID],
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferFromIntermediate>> {
        let mut token_array = self.get_token_data_amt(bs)?;
        let mut owner_map = self.get_owner_data_hamt(bs)?;

        for token_id in token_ids {
            if !Self::approved_for_token(&token_array, &owner_map, caller, *token_id)? {
                return Err(StateError::NotAuthorized { actor: caller, token_id: *token_id });
            }

            // update the token_data to reflect the new owner and clear approved operators
            self.make_transfer(&mut token_array, &mut owner_map, *token_id, to)?;
        }

        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;

        let params = FRCXXTokenReceived {
            to,
            operator: caller,
            token_ids: token_ids.into(),
            operator_data,
            token_data,
        };

        let res = TransferFromIntermediate {
            to,
            token_ids: token_ids.into(),
            recipient_data: RawBytes::default(),
        };

        Ok(ReceiverHook::new_frcxx(Address::new_id(to), params, res)?)
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
            .ok_or_else(|| StateError::InvariantFailed(format!("token {token_id} not found")))?;

        // operator is approved at token-level
        if token_data.operators.contains_actor(&operator) {
            return Ok(true);
        }

        // operator is approved at account-level
        let owner_account = owner_map.get(&actor_id_key(token_data.owner))?.ok_or_else(|| {
            StateError::InvariantFailed(format!("owner of token {token_id} not found"))
        })?;
        if owner_account.operators.contains_actor(&operator) {
            return Ok(true);
        }

        Ok(false)
    }

    /// Converts a MintIntermediate to a MintReturn
    ///
    /// This function should be called on a freshly loaded or known-up-to-date state
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

    /// Converts a TransferIntermediate to a TransferReturn
    ///
    /// This function should be called on a freshly loaded or known-up-to-date state
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

    /// Converts a TransferFromIntermediate to a TransferFromReturn
    ///
    /// This function should be called on a freshly loaded or known-up-to-date state
    pub fn transfer_from_return<BS: Blockstore>(
        &self,
        bs: &BS,
        intermediate: TransferFromIntermediate,
    ) -> Result<TransferFromReturn> {
        let to_balance = self.get_balance(bs, intermediate.to)?;
        Ok(TransferFromReturn { to_balance, token_ids: intermediate.token_ids })
    }

    /// Get the metadata for a token
    pub fn get_metadata<BS: Blockstore>(&self, bs: &BS, token_id: u64) -> Result<String> {
        let token_data_array = self.get_token_data_amt(bs)?;
        let token =
            token_data_array.get(token_id)?.ok_or_else(|| StateError::TokenNotFound(token_id))?;
        Ok(token.metadata.clone())
    }

    /// Get the owner of a token
    pub fn get_owner<BS: Blockstore>(&self, bs: &BS, token_id: u64) -> Result<ActorID> {
        let token_data_array = self.get_token_data_amt(bs)?;
        let token =
            token_data_array.get(token_id)?.ok_or_else(|| StateError::TokenNotFound(token_id))?;
        Ok(token.owner)
    }

    pub fn list_tokens<BS: Blockstore>(&self, bs: &BS) -> Result<Vec<TokenID>> {
        let token_amt = self.get_token_data_amt(bs)?;
        let mut vec = vec![];
        token_amt
            .for_each(|id, _| {
                vec.push(id);
                Ok(())
            })
            .unwrap();
        Ok(vec)
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

                // BitField maintains operator invariants, re-enable these checks if operators are stored in a vec
                // assert operator array has no duplicates and is ordered
                // let res = Self::assert_operator_array(&data.operators);
                // if res.is_err() {
                //     errors.push(res.err().unwrap());
                // }

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

                // BitField maintains operator invariants, re-enable these checks if operators are stored in a vec
                // let res = Self::assert_operator_array(&data.operators);
                // if res.is_err() {
                //     errors.push(res.err().unwrap());
                // }

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

    #[allow(dead_code)]
    fn assert_operator_array(
        operators: &[ActorID],
    ) -> std::result::Result<(), StateInvariantError> {
        for pair in operators.windows(2) {
            if pair[0] >= pair[1] {
                // pairs need to be unique and strictly increasing
                return Err(StateInvariantError::InvalidOperatorArray(operators.to_vec()));
            }
        }
        Ok(())
    }

    /// Helper to decode keys from bytes, recording errors if they fail
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

#[cfg(test)]
mod test {
    use std::vec;

    use fvm_actor_utils::messaging::FakeMessenger;
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_ipld_encoding::RawBytes;
    use fvm_shared::ActorID;

    use crate::{
        state::{StateError, TokenID},
        types::{MintIntermediate, TransferFromIntermediate, TransferIntermediate},
        NFTState,
    };

    const ALICE_ID: ActorID = 1;
    const BOB_ID: ActorID = 2;
    const CHARLIE_ID: ActorID = 3;

    /// A convenience wrapper to help with testing
    ///
    /// Assigns default values to optional fields and calls receiver hooks for minting/transfer
    struct StateTester {
        pub state: NFTState,
        pub bs: MemoryBlockstore,
        pub msg: FakeMessenger,
    }

    impl StateTester {
        fn new() -> Self {
            let bs = MemoryBlockstore::default();
            let state = NFTState::new(&bs).unwrap();
            let msg = FakeMessenger::new(0, 99);
            Self { state, bs, msg }
        }

        /// Mint an amount of NFTs and return the token_ids
        fn mint_amount(&mut self, to: ActorID, num_tokens: u64) -> MintIntermediate {
            let mut hook = self
                .state
                .mint_tokens(
                    &self.bs,
                    0,
                    to,
                    vec![String::default(); num_tokens as usize],
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap();
            hook.call(&self.msg).unwrap()
        }

        /// Transfer tokens to an address, expecting the transaction to succeed
        fn transfer(
            &mut self,
            operator: ActorID,
            to: ActorID,
            token_ids: &[TokenID],
        ) -> TransferIntermediate {
            let mut hook = self
                .state
                .transfer_tokens(
                    &self.bs,
                    operator,
                    to,
                    token_ids,
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap();
            hook.call(&self.msg).unwrap()
        }

        /// Transfer tokens to an address, expecting the transaction to succeed
        fn operator_transfer(
            &mut self,
            operator: ActorID,
            to: ActorID,
            token_ids: &[TokenID],
        ) -> TransferFromIntermediate {
            let mut hook = self
                .state
                .operator_transfer_tokens(
                    &self.bs,
                    operator,
                    to,
                    token_ids,
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap();
            hook.call(&self.msg).unwrap()
        }

        fn assert_balance(&self, owner: ActorID, expected: u64) {
            let balance = self.state.get_balance(&self.bs, owner).unwrap();
            assert_eq!(balance, expected);
        }

        fn assert_invariants(&self) {
            let (_, vec) = self.state.check_invariants(&self.bs);
            assert!(vec.is_empty(), "invariants failed: {vec:?}");
        }
    }

    #[test]
    fn it_mints_tokens_incrementally() {
        let mut tester = StateTester::new();

        // mint first token
        let res = tester.mint_amount(ALICE_ID, 1);
        // expect balance increase, token id increment
        tester.assert_balance(ALICE_ID, 1);
        assert_eq!(res.token_ids, vec![0]);
        assert_eq!(tester.state.total_supply, 1);

        // mint another token
        let res = tester.mint_amount(ALICE_ID, 1);
        // expect balance increase, token id increment
        tester.assert_balance(ALICE_ID, 2);
        assert_eq!(res.token_ids, vec![1]);
        assert_eq!(tester.state.total_supply, 2);

        // expect another actor to have zero balance by default
        tester.assert_balance(BOB_ID, 0);

        // mint another token to a different actor
        let res = tester.mint_amount(BOB_ID, 1);
        // expect balance increase globally, token id increment
        tester.assert_balance(ALICE_ID, 2);
        tester.assert_balance(BOB_ID, 1);
        assert_eq!(res.token_ids, vec![2]);
        assert_eq!(tester.state.total_supply, 3);

        // mint 0 tokens (manual empty array should succeed)
        let mut hook = tester
            .state
            .mint_tokens(&tester.bs, 0, ALICE_ID, vec![], RawBytes::default(), RawBytes::default())
            .unwrap();
        let res = hook.call(&tester.msg).unwrap();
        assert_eq!(res.token_ids, Vec::<TokenID>::default());
        // assert no state change
        tester.assert_balance(ALICE_ID, 2);
        tester.assert_balance(BOB_ID, 1);
        assert_eq!(res.token_ids, Vec::<TokenID>::default());
        assert_eq!(tester.state.total_supply, 3);

        tester.assert_invariants();
    }

    #[test]
    fn it_burns_tokens() {
        let mut tester = StateTester::new();
        tester.mint_amount(ALICE_ID, 4);
        tester.assert_balance(ALICE_ID, 4);
        assert_eq!(tester.state.total_supply, 4);

        // burn a non-existent token
        let err = tester.state.burn_tokens(&tester.bs, ALICE_ID, &[99]).unwrap_err();
        if let StateError::TokenNotFound(token_id) = err {
            assert_eq!(token_id, 99);
        } else {
            panic!("unexpected error: {err:?}");
        }
        // no state change
        assert_eq!(tester.state.total_supply, 4);
        tester.assert_balance(ALICE_ID, 4);

        // burn a token owned by alice
        tester.state.burn_tokens(&tester.bs, ALICE_ID, &[0]).unwrap();
        // total supply and balance should decrease
        tester.assert_balance(ALICE_ID, 3);
        assert_eq!(tester.state.total_supply, 3);

        // attempt to burn the same token again
        // burn a token owned by alice
        let err = tester.state.burn_tokens(&tester.bs, ALICE_ID, &[0]).unwrap_err();
        if let StateError::TokenNotFound(token_id) = err {
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {err:?}");
        }
        // total supply and balance should remain the same
        tester.assert_balance(ALICE_ID, 3);
        assert_eq!(tester.state.total_supply, 3);

        // attempt to burn multiple tokens owned by alice with one invalid token
        tester.state.burn_tokens(&tester.bs, ALICE_ID, &[0, 1, 2]).unwrap_err();
        // total supply and balance should not change
        tester.assert_balance(ALICE_ID, 3);
        assert_eq!(tester.state.total_supply, 3);

        // attempt to burn multiple tokens owned by alice with one invalid token (invalid token at end)
        tester.state.burn_tokens(&tester.bs, ALICE_ID, &[1, 2, 0]).unwrap_err();
        // total supply and balance should not change
        tester.assert_balance(ALICE_ID, 3);
        assert_eq!(tester.state.total_supply, 3);

        // attempt to burn multiple tokens owned by alice
        tester.state.burn_tokens(&tester.bs, ALICE_ID, &[1, 2]).unwrap();
        // total supply and balance should not change
        tester.assert_balance(ALICE_ID, 1);
        assert_eq!(tester.state.total_supply, 1);

        tester.assert_invariants();
    }

    #[test]
    fn it_transfers_tokens() {
        let mut tester = StateTester::new();

        // mint two tokens
        tester.mint_amount(ALICE_ID, 3);

        // bob cannot transfer from alice to himself
        let res = tester
            .state
            .transfer_tokens(
                &tester.bs,
                BOB_ID,
                BOB_ID,
                &[0],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {res:?}");
        }

        // alice can transfer to bob
        let res = tester.transfer(ALICE_ID, BOB_ID, &[0]);
        tester.assert_balance(ALICE_ID, 2);
        tester.assert_balance(BOB_ID, 1);
        assert_eq!(res.token_ids, vec![0]);

        // alice is unauthorized to transfer that token now
        let res = tester
            .state
            .transfer_tokens(
                &tester.bs,
                ALICE_ID,
                ALICE_ID,
                &[0],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, ALICE_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {res:?}");
        }
        // no state change
        tester.assert_balance(ALICE_ID, 2);
        tester.assert_balance(BOB_ID, 1);

        // but bob can transfer it back
        let res = tester.transfer(BOB_ID, ALICE_ID, &[0]);
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 0);
        assert_eq!(res.token_ids, vec![0]);
        assert_eq!(res.to, ALICE_ID);

        // transferring a batch fails if any tokens is not valid
        tester
            .state
            .transfer_tokens(
                &tester.bs,
                ALICE_ID,
                BOB_ID,
                &[1, 99],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        tester
            .state
            .transfer_tokens(
                &tester.bs,
                ALICE_ID,
                BOB_ID,
                &[99, 1],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        // or there are duplicates
        let err = tester
            .state
            .transfer_tokens(
                &tester.bs,
                ALICE_ID,
                BOB_ID,
                &[1, 1, 2],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = err {
            assert_eq!(operator, ALICE_ID);
            assert_eq!(token_id, 1);
        } else {
            panic!("unexpected error: {res:?}");
        }
        // state unchanged
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 0);

        // alice can transfer other two in a batch
        let res = tester.transfer(ALICE_ID, BOB_ID, &[1, 2]);
        tester.assert_balance(ALICE_ID, 1);
        tester.assert_balance(BOB_ID, 2);
        assert_eq!(res.token_ids, vec![1, 2]);
        tester.assert_invariants();
    }

    #[test]
    fn it_allows_account_level_delegation() {
        let mut tester = StateTester::new();

        // mint a few tokens
        tester.mint_amount(ALICE_ID, 4);

        // bob cannot transfer from alice to himself
        let res = tester
            .state
            .operator_transfer_tokens(
                &tester.bs,
                BOB_ID,
                BOB_ID,
                &[0],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        if let StateError::NotAuthorized { actor: operator, token_id } = res {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {res:?}");
        }

        // approve bob to transfer on behalf of alice
        tester.state.approve_for_owner(&tester.bs, ALICE_ID, BOB_ID).unwrap();

        // bob can now transfer from alice to himself
        // but cannot use the incorrect method
        let res = tester
            .state
            .transfer_tokens(
                &tester.bs,
                BOB_ID,
                ALICE_ID,
                &[0],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {res:?}");
        }

        // using correct method succeeds
        let res = tester.operator_transfer(BOB_ID, BOB_ID, &[0]);
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 1);
        assert_eq!(tester.state.total_supply, 4);
        assert_eq!(res.to, BOB_ID);
        assert_eq!(res.token_ids, vec![0]);

        // alice is unauthorised to transfer that token now
        let res = tester
            .state
            .transfer_tokens(
                &tester.bs,
                ALICE_ID,
                ALICE_ID,
                &[0],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        // because she is no longer the owner
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, ALICE_ID);
            assert_eq!(token_id, 0);
        } else {
            panic!("unexpected error: {res:?}");
        }
        // state was unchanged
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 1);

        // but bob can transfer it back
        tester.transfer(BOB_ID, ALICE_ID, &[0]);
        tester.assert_balance(ALICE_ID, 4);
        tester.assert_balance(BOB_ID, 0);

        // bob can burn a token for alice
        // but not with the wrong method
        let res = tester.state.burn_tokens(&tester.bs, BOB_ID, &[1]).unwrap_err();
        if let StateError::NotOwner { actor: operator, token_id } = res {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, 1);
        } else {
            panic!("unexpected error: {res:?}");
        }
        // state was unchanged
        tester.assert_balance(ALICE_ID, 4);
        tester.assert_balance(BOB_ID, 0);
        assert_eq!(tester.state.total_supply, 4);

        // using correct method succeeds
        tester.state.operator_burn_tokens(&tester.bs, BOB_ID, &[0]).unwrap();
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 0);
        assert_eq!(tester.state.total_supply, 3);

        // a newly minted token after approval can be transferred by bob
        let res = tester.mint_amount(ALICE_ID, 1);
        tester.operator_transfer(BOB_ID, BOB_ID, &res.token_ids);
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 1);

        // a newly minted token after approval can be burnt by bob
        let res = tester.mint_amount(ALICE_ID, 1);
        tester.state.operator_burn_tokens(&tester.bs, BOB_ID, &res.token_ids).unwrap();
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 1);

        // bob cannot transfer a batch if any of the tokens is invalid or duplicated
        tester
            .state
            .operator_transfer_tokens(
                &tester.bs,
                BOB_ID,
                BOB_ID,
                &res.token_ids, // already transferred
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        tester
            .state
            .operator_transfer_tokens(
                &tester.bs,
                BOB_ID,
                BOB_ID,
                &[0, 99], // 99 doesn't exist
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        tester
            .state
            .operator_transfer_tokens(
                &tester.bs,
                BOB_ID,
                BOB_ID,
                &[0, 0], // duplicaated
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        // no state change
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 1);

        // bob can batch transfer tokens
        let res = tester.mint_amount(ALICE_ID, 3);
        tester.assert_balance(ALICE_ID, 6);
        tester.assert_balance(BOB_ID, 1);
        let res = tester.operator_transfer(BOB_ID, BOB_ID, &res.token_ids); // transfer the newly minted tokens
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 4);

        // bob's authorization can be revoked
        tester.state.revoke_for_all(&tester.bs, ALICE_ID, BOB_ID).unwrap();
        // cannot transfer
        let err = tester
            .state
            .operator_transfer_tokens(
                &tester.bs,
                BOB_ID,
                BOB_ID,
                &[res.token_ids[1]],
                RawBytes::default(),
                RawBytes::default(),
            )
            .unwrap_err();
        if let StateError::NotAuthorized { actor: operator, token_id } = err {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, res.token_ids[1]);
        } else {
            panic!("unexpected error: {err:?}");
        }
        // cannot burn
        tester.state.operator_burn_tokens(&tester.bs, BOB_ID, &[res.token_ids[1]]).unwrap_err();
        if let StateError::NotAuthorized { actor: operator, token_id } = err {
            assert_eq!(operator, BOB_ID);
            assert_eq!(token_id, res.token_ids[1]);
        } else {
            panic!("unexpected error: {err:?}");
        }
        // state didn't change
        tester.assert_balance(ALICE_ID, 3);
        tester.assert_balance(BOB_ID, 4);

        tester.assert_invariants();
    }

    #[test]
    fn it_allows_token_level_delegation() {
        let mut tester = StateTester::new();

        if let [token_0, token_1] = tester.mint_amount(ALICE_ID, 2).token_ids[..] {
            // neither bob nor charlie can transfer either token
            tester
                .state
                .operator_transfer_tokens(
                    &tester.bs,
                    BOB_ID,
                    BOB_ID,
                    &[token_0],
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap_err();
            tester
                .state
                .operator_transfer_tokens(
                    &tester.bs,
                    CHARLIE_ID,
                    BOB_ID,
                    &[token_0],
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap_err();
            tester
                .state
                .operator_transfer_tokens(
                    &tester.bs,
                    BOB_ID,
                    BOB_ID,
                    &[token_1],
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap_err();
            tester
                .state
                .operator_transfer_tokens(
                    &tester.bs,
                    CHARLIE_ID,
                    BOB_ID,
                    &[token_1],
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap_err();

            // neither bob nor charlie can burn either token
            tester.state.operator_burn_tokens(&tester.bs, BOB_ID, &[token_0]).unwrap_err();
            tester.state.operator_burn_tokens(&tester.bs, CHARLIE_ID, &[token_0]).unwrap_err();
            tester.state.operator_burn_tokens(&tester.bs, BOB_ID, &[token_1]).unwrap_err();
            tester.state.operator_burn_tokens(&tester.bs, CHARLIE_ID, &[token_1]).unwrap_err();

            // state didn't change
            tester.assert_balance(ALICE_ID, 2);
            tester.assert_balance(BOB_ID, 0);
            tester.assert_balance(CHARLIE_ID, 0);

            // charlie cannot not approve bob or charlie for a token owned by alice
            tester
                .state
                .approve_for_tokens(&tester.bs, CHARLIE_ID, BOB_ID, &[token_0])
                .unwrap_err();
            let res = tester
                .state
                .approve_for_tokens(&tester.bs, CHARLIE_ID, CHARLIE_ID, &[token_0])
                .unwrap_err();
            if let StateError::NotOwner { actor, token_id } = res {
                assert_eq!(actor, CHARLIE_ID);
                assert_eq!(token_id, token_0);
            } else {
                panic!("unexpected error: {res:?}");
            }

            // alice approves bob and charlie as operators
            tester.state.approve_for_tokens(&tester.bs, ALICE_ID, BOB_ID, &[token_0]).unwrap();
            tester.state.approve_for_tokens(&tester.bs, ALICE_ID, BOB_ID, &[token_1]).unwrap();
            tester.state.approve_for_tokens(&tester.bs, ALICE_ID, CHARLIE_ID, &[token_1]).unwrap();

            // charlie still can't transfer token_0
            let res = tester
                .state
                .operator_transfer_tokens(
                    &tester.bs,
                    CHARLIE_ID,
                    CHARLIE_ID,
                    &[token_0],
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap_err();
            if let StateError::NotAuthorized { actor, token_id } = res {
                assert_eq!(actor, CHARLIE_ID);
                assert_eq!(token_id, token_0);
            } else {
                panic!("unexpected error: {res:?}");
            }
            // charlie still can't burn token_0
            tester.state.operator_burn_tokens(&tester.bs, CHARLIE_ID, &[token_0]).unwrap_err();

            // but bob can transfer token_0
            tester.operator_transfer(BOB_ID, BOB_ID, &[token_0]);
            tester.assert_balance(ALICE_ID, 1);
            tester.assert_balance(BOB_ID, 1);
            tester.assert_balance(CHARLIE_ID, 0);

            // charlie can transfer token_1
            tester.operator_transfer(CHARLIE_ID, CHARLIE_ID, &[token_1]);
            tester.assert_balance(ALICE_ID, 0);
            tester.assert_balance(BOB_ID, 1);
            tester.assert_balance(CHARLIE_ID, 1);

            // but after that, bob can no longer transfer it (approvals were reset)
            tester
                .state
                .operator_transfer_tokens(
                    &tester.bs,
                    BOB_ID,
                    CHARLIE_ID,
                    &[token_1],
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap_err();
            // state was unchanged
            tester.assert_balance(ALICE_ID, 0);
            tester.assert_balance(BOB_ID, 1);
            tester.assert_balance(CHARLIE_ID, 1);

            // charlie can approve bob to for token_1
            tester.state.approve_for_tokens(&tester.bs, CHARLIE_ID, BOB_ID, &[token_1]).unwrap();
            // now bob can burn token_1
            // but not with the wrong method
            tester.state.burn_tokens(&tester.bs, BOB_ID, &[token_1]).unwrap_err();
            // state was unchanged
            tester.assert_balance(ALICE_ID, 0);
            tester.assert_balance(BOB_ID, 1);
            tester.assert_balance(CHARLIE_ID, 1);
            // using the correct method succeeds
            tester.state.operator_burn_tokens(&tester.bs, BOB_ID, &[token_1]).unwrap();
            // the token disappears
            tester.assert_balance(ALICE_ID, 0);
            tester.assert_balance(BOB_ID, 1);
            tester.assert_balance(CHARLIE_ID, 0);

            // total supply is updated
            assert_eq!(tester.state.total_supply, 1);
        }
        tester.assert_invariants();
    }
}
