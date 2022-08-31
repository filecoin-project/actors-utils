use fvm_ipld_blockstore::Blockstore;
use fvm_shared::ActorID;

use self::state::{NFTSetState, TokenID};

pub mod state;

pub struct NFT<'st, BS>
where
    BS: Blockstore,
{
    bs: BS,
    state: &'st mut NFTSetState,
}

impl<'st, BS> NFT<'st, BS>
where
    BS: Blockstore,
{
    pub fn wrap(bs: BS, state: &'st mut NFTSetState) -> Self {
        Self { bs, state }
    }
}

impl<'st, BS> NFT<'st, BS>
where
    BS: Blockstore,
{
    pub fn mint(&mut self, initial_owner: ActorID) -> TokenID {
        self.state.mint_token(&self.bs, initial_owner).unwrap()
    }
}
