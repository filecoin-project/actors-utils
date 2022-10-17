//! This acts as the reference library for FRCXX. While remaining complaint with the
//! spec, this library is opinionated in its batching, minting and storage
//! strategies to optimize for common usage patterns.
//!
//! For example, write operations are generally optimised over read operations as
//! on-chain state can be read by direct inspection (rather than via an actor call)
//! in many cases.

use cid::Cid;
use fvm_actor_utils::messaging::{Messaging, MessagingError};
use fvm_ipld_blockstore::Blockstore;
use fvm_shared::{address::Address, ActorID};
use state::StateError;
use thiserror::Error;

use self::state::{NFTState, TokenID};

pub mod state;
pub mod types;
pub mod util;

#[derive(Error, Debug)]
pub enum NFTError {
    #[error("error in underlying state {0}")]
    NFTState(#[from] StateError),
    #[error("error calling other actor: {0}")]
    Messaging(#[from] MessagingError),
}

pub type Result<T> = std::result::Result<T, NFTError>;

/// A helper handle for NFTState that injects services into the state-level operations
pub struct NFT<'st, BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    bs: BS,
    msg: MSG,
    state: &'st mut NFTState,
}

impl<'st, BS, MSG> NFT<'st, BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    /// Wrap an instance of the state-tree in a handle for higher-level operations
    pub fn wrap(bs: BS, msg: MSG, state: &'st mut NFTState) -> Self {
        Self { bs, msg, state }
    }

    /// Flush state and return Cid for root
    pub fn flush(&mut self) -> Result<Cid> {
        Ok(self.state.save(&self.bs)?)
    }
}

impl<'st, BS, MSG> NFT<'st, BS, MSG>
where
    BS: Blockstore,
    MSG: Messaging,
{
    /// Return the total number of NFTs in circulation from this collection
    pub fn total_supply(&self) -> u64 {
        self.state.total_supply
    }

    /// Create a single new NFT belonging to the initial_owner
    ///
    /// Returns the TokenID of the minted which is allocated incrementally
    pub fn mint(&mut self, initial_owner: Address, metadata_id: Cid) -> Result<TokenID> {
        let initial_owner = self.msg.resolve_or_init(&initial_owner)?;
        Ok(self.state.mint_token(&self.bs, initial_owner, metadata_id)?)
    }

    /// Burn a single NFT by TokenID
    ///
    /// A burnt TokenID can never be minted again
    pub fn burn(&mut self, caller: ActorID, token_ids: &[TokenID]) -> Result<()> {
        self.state.burn_tokens(&self.bs, caller, token_ids)?;
        Ok(())
    }
}
