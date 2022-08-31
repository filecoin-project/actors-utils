use cid::Cid;
use fvm_ipld_amt::Amt;
use fvm_ipld_amt::Error as AmtError;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::tuple::*;
use fvm_shared::address::Address;
use fvm_shared::ActorID;
use thiserror::Error;

pub type TokenID = u64;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("ipld hamt error: {0}")]
    IpldAmt(#[from] AmtError),
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

    fn get_token_amt<'bs, BS: Blockstore>(&self, store: &'bs BS) -> Result<Amt<ActorID, &'bs BS>> {
        let res = Amt::load(&self.tokens, store)?;
        Ok(res)
    }

    pub fn mint_token<BS: Blockstore>(&self, bs: &BS, owner: ActorID) -> Result<TokenID> {
        let mut token_map = self.get_token_amt(&bs)?;
        let new_index = token_map.count();
        token_map.set(new_index, owner)?;
        Ok(new_index)
    }
}
