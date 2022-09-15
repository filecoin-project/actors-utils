//! This acts as the reference library for FRCXX. While remaining complaint with the
//! spec, this library is opinionated in its batching, minting and storage
//! strategies to optimize for common usage patterns.
//!
//! For example, write operations are generally optimised over read operations as
//! on-chain state can be read by direct inspection (rather than via an actor call)
//! in many cases.

use fvm_ipld_blockstore::Blockstore;
use fvm_shared::ActorID;

use self::state::{NFTState, TokenID};

pub mod state;
pub mod types;

/// A helper handle for NFTState that injects services into the state-level operations
pub struct NFT<'st, BS>
where
    BS: Blockstore,
{
    bs: BS,
    state: &'st mut NFTState,
}

impl<'st, BS> NFT<'st, BS>
where
    BS: Blockstore,
{
    /// Wrap an instance of the state-tree in a handle for higher-level operations
    pub fn wrap(bs: BS, state: &'st mut NFTState) -> Self {
        Self { bs, state }
    }
}

impl<'st, BS> NFT<'st, BS>
where
    BS: Blockstore,
{
    /// Create a single new NFT belonging to the initial_owner
    ///
    /// Returns the TokenID of the minted which is allocated incrementally
    pub fn mint(&mut self, initial_owner: ActorID, metadata_uri: String) -> TokenID {
        self.state.mint_token(&self.bs, initial_owner, metadata_uri).unwrap()
    }
}
