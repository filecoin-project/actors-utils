use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_amt::Amt;
use fvm_ipld_amt::Error as AmtError;
use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::CborStore;
use fvm_ipld_encoding::DAG_CBOR;
use fvm_shared::ActorID;
use thiserror::Error;

pub use super::types::BatchMintReturn;

pub type TokenID = u64;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("ipld hamt error: {0}")]
    IpldAmt(#[from] AmtError),
    #[error("other error: {0}")]
    Other(String),
}

/// Each token stores its owner, approved operators etc.
pub struct TokenData {
    pub owner: ActorID,
    pub approved: Vec<ActorID>, // or maybe as a Cid to an Amt
}

/// Each owner stores their own balance and other indexed data
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
    /// Amt<ActorID, OwnerData> index for faster lookup of data often queried by owner
    pub owner_data: Cid,
    /// The next available token id for minting
    pub next_token: TokenID,
    /// The number of minted tokens less the number of burned tokens
    pub total_supply: u64,
}

const AMT_BIT_WIDTH: u32 = 5;

type Result<T> = std::result::Result<T, StateError>;

impl NFTState {
    /// Create a new NFT state-tree, without committing it (the root Cid) to a blockstore
    pub fn new<BS: Blockstore>(store: &BS) -> Result<Self> {
        // Blockstore is still needed to create valid Cids for the Hamts
        let empty_token_array =
            Amt::<ActorID, _>::new_with_bit_width(store, AMT_BIT_WIDTH).flush()?;
        // Blockstore is still needed to create valid Cids for the Hamts
        let empty_owner_arrays =
            Amt::<ActorID, _>::new_with_bit_width(store, AMT_BIT_WIDTH).flush()?;

        Ok(Self {
            token_data: empty_token_array,
            owner_data: empty_owner_arrays,
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

    fn get_token_amt<'bs, BS: Blockstore>(&self, store: &'bs BS) -> Result<Amt<ActorID, &'bs BS>> {
        let res = Amt::load(&self.owner_data, store)?;
        Ok(res)
    }

    pub fn mint_token<BS: Blockstore>(&mut self, bs: &BS, owner: ActorID) -> Result<TokenID> {
        let mut token_map = self.get_token_amt(&bs)?;
        let new_index = token_map.count();
        token_map.set(new_index, owner)?;
        self.token_data = token_map.flush()?;
        Ok(new_index)
    }

    pub fn batch_mint_tokens<BS: Blockstore>(
        &mut self,
        bs: &BS,
        owner: ActorID,
        count: u64,
    ) -> Result<BatchMintReturn> {
        let mut token_map = self.get_token_amt(&bs)?;
        let mut tokens = Vec::new();
        for _ in 0..count {
            let new_index = token_map.count();
            token_map.set(new_index, owner)?;
            tokens.push(new_index);
        }
        self.token_data = token_map.flush()?;
        Ok(BatchMintReturn { tokens })
    }
}

#[cfg(test)]
mod test {}
