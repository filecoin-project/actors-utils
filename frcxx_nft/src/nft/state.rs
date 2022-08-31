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

pub type TokenID = u64;

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct BatchMintReturn {
    pub tokens: Vec<TokenID>,
}

#[derive(Error, Debug)]
pub enum StateError {
    #[error("ipld hamt error: {0}")]
    IpldAmt(#[from] AmtError),
    #[error("other error: {0}")]
    Other(String),
}

/// NFT state IPLD structure
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct NFTSetState {
    /// Amt<ActorID> of balances as a Hamt where index is TokenID
    pub tokens: Cid,
}

const AMT_BIT_WIDTH: u32 = 5;

type Result<T> = std::result::Result<T, StateError>;

impl NFTSetState {
    /// Create a new NFT state-tree, without committing it (the root Cid) to a blockstore
    pub fn new<BS: Blockstore>(store: &BS) -> Result<Self> {
        // Blockstore is still needed to create valid Cids for the Hamts
        let empty_token_array =
            Amt::<ActorID, _>::new_with_bit_width(store, AMT_BIT_WIDTH).flush()?;

        Ok(Self { tokens: empty_token_array })
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
        let res = Amt::load(&self.tokens, store)?;
        Ok(res)
    }

    pub fn mint_token<BS: Blockstore>(&mut self, bs: &BS, owner: ActorID) -> Result<TokenID> {
        let mut token_map = self.get_token_amt(&bs)?;
        let new_index = token_map.count();
        token_map.set(new_index, owner)?;
        self.tokens = token_map.flush()?;
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
        self.tokens = token_map.flush()?;
        Ok(BatchMintReturn { tokens })
    }
}
