//! Abstraction of the on-chain state related to NFT accounting
use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_amt::Amt;
use fvm_ipld_amt::Error as AmtError;
use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_ipld_hamt::Error as HamtError;
use fvm_ipld_hamt::Hamt;
use fvm_shared::ActorID;
use thiserror::Error;

pub type TokenID = u64;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("ipld amt error: {0}")]
    IpldAmt(#[from] AmtError),
    #[error("ipld hamt error: {0}")]
    IpldHamt(#[from] HamtError),
    #[error("other error: {0}")]
    Other(String),
}

/// Each token stores its owner, approved operators etc.
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TokenData {
    pub owner: ActorID,
    // operators on this token
    pub approved: Vec<ActorID>, // or maybe as a Cid to an Amt
    pub metadata_uri: String,
}

/// Each owner stores their own balance and other indexed data
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct OwnerData {
    pub balance: u64,
    // account-level operators
    pub approved: Vec<ActorID>, // maybe as a Cid to an Amt
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
            _ => panic!(""),
        }
    }

    pub fn save<BS: Blockstore>(&self, store: &BS) -> Result<Cid> {
        let serialized = match fvm_ipld_encoding::to_vec(self) {
            Ok(s) => s,
            Err(err) => return Err(StateError::Other(err.to_string())),
        };
        let block = Block { codec: DAG_CBOR, data: serialized };
        let cid = match store.put(Code::Blake2b256, &block) {
            Ok(cid) => cid,
            Err(err) => return Err(StateError::Other(err.to_string())),
        };
        Ok(cid)
    }

    fn get_token_data_amt<'bs, BS: Blockstore>(
        &self,
        store: &'bs BS,
    ) -> Result<Amt<TokenData, &'bs BS>> {
        let res = Amt::load(&self.token_data, store)?;
        Ok(res)
    }

    fn get_owner_data_hamt<'bs, BS: Blockstore>(
        &self,
        store: &'bs BS,
    ) -> Result<Hamt<&'bs BS, OwnerData, ActorID>> {
        let res = Hamt::load_with_bit_width(&self.owner_data, store, HAMT_BIT_WIDTH)?;
        Ok(res)
    }

    /// Mint a new token to the specified address
    pub fn mint_token<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        metadata_uri: String,
    ) -> Result<TokenID> {
        // update token data array
        let mut token_array = self.get_token_data_amt(bs)?;
        let token_id = self.next_token;
        token_array.set(token_id, TokenData { owner, approved: vec![], metadata_uri })?;

        // update owner data map
        let mut owner_map = self.get_owner_data_hamt(bs)?;
        let new_owner_data = match owner_map.delete(&owner) {
            Ok(entry) => {
                if let Some((_, existing_data)) = entry {
                    OwnerData { balance: existing_data.balance + 1, ..existing_data }
                } else {
                    OwnerData { balance: 1, approved: vec![] }
                }
            }
            Err(e) => return Err(e.into()),
        };
        owner_map.set(owner, new_owner_data)?;

        // update global trackers
        self.next_token += 1;
        self.total_supply += 1;

        self.token_data = token_array.flush()?;
        self.owner_data = owner_map.flush()?;
        Ok(token_id)
    }

    /// Get the number of tokens owned by a particular address
    pub fn get_balance<BS: Blockstore>(&mut self, bs: &BS, owner: ActorID) -> Result<u64> {
        let owner_data = self.get_owner_data_hamt(bs)?;
        let balance = match owner_data.get(&owner)? {
            Some(data) => data.balance,
            None => 0,
        };

        Ok(balance)
    }
}

#[cfg(test)]
mod test {
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_shared::ActorID;

    use crate::NFTState;

    const ALICE_ID: ActorID = 1;
    const BOB_ID: ActorID = 2;

    #[test]
    fn it_mints_tokens_incrementally() {
        let bs = &MemoryBlockstore::new();
        let mut state = NFTState::new(bs).unwrap();

        // mint first token
        let token_id = state.mint_token(bs, ALICE_ID, "".into()).unwrap();
        let balance = state.get_balance(bs, ALICE_ID).unwrap();
        // expect balance increase, token id increment
        assert_eq!(token_id, 0);
        assert_eq!(balance, 1);

        // mint another token
        let token_id = state.mint_token(bs, ALICE_ID, "".into()).unwrap();
        let balance = state.get_balance(bs, ALICE_ID).unwrap();
        // expect balance increase, token id increment
        assert_eq!(token_id, 1);
        assert_eq!(balance, 2);

        // expect another actor to have zero balance by default
        let balance = state.get_balance(bs, BOB_ID).unwrap();
        assert_eq!(balance, 0);

        // mint another token to a different actor
        let token_id = state.mint_token(bs, BOB_ID, "".into()).unwrap();
        let alice_balance = state.get_balance(bs, ALICE_ID).unwrap();
        let bob_balance = state.get_balance(bs, BOB_ID).unwrap();
        // expect balance increase globally, token id increment
        assert_eq!(token_id, 2);
        assert_eq!(bob_balance, 1);
        assert_eq!(alice_balance, 2);
    }
}
