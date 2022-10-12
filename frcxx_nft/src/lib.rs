//! This acts as the reference library for FRCXX. While remaining complaint with the
//! spec, this library is opinionated in its batching, minting and storage
//! strategies to optimize for common usage patterns.
//!
//! For example, write operations are generally optimised over read operations as
//! on-chain state can be read by direct inspection (rather than via an actor call)
//! in many cases.

use cid::Cid;
use fvm_actor_utils::{
    messaging::{Messaging, MessagingError},
    receiver::ReceiverHook,
};
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, ActorID};
use state::StateError;
use thiserror::Error;
use types::{MintIntermediate, TransferIntermediate};

use self::state::{NFTState, TokenID};

pub mod receiver;
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

    /// Create a single new NFT belonging to the initial_owner. The mint method is not standardised
    /// as part of the actor's interface but this is a usefuly method at the library level to
    /// generate new tokens that will maintain the necessary state invariants.
    ///
    /// Returns a MintIntermediate that can be used to construct return data
    pub fn mint(
        &mut self,
        caller: Address,
        initial_owner: Address,
        metadata_ids: &[Cid],
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<MintIntermediate>> {
        let caller = self.msg.resolve_or_init(&caller)?;
        let initial_owner = self.msg.resolve_or_init(&initial_owner)?;
        Ok(self.state.mint_tokens(
            &self.bs,
            caller,
            initial_owner,
            metadata_ids,
            operator_data,
            token_data,
        )?)
    }

    /// Burn a single NFT by TokenID
    ///
    /// A burnt TokenID can never be minted again
    pub fn burn(&mut self, caller: ActorID, token_ids: &[TokenID]) -> Result<()> {
        self.state.burn_tokens(&self.bs, caller, token_ids)?;
        Ok(())
    }

    /// Approve an operator to transfer or burn a single NFT
    pub fn approve(
        &mut self,
        caller: &Address,
        operator: &Address,
        token_ids: &[TokenID],
    ) -> Result<()> {
        // Attempt to instantiate the accounts if they don't exist
        let caller = self.msg.resolve_or_init(caller)?;
        let operator = self.msg.resolve_or_init(operator)?;

        self.state.approve_for_tokens(&self.bs, caller, operator, token_ids)?;
        Ok(())
    }

    /// Revoke the approval of an operator to transfer a particular NFT
    pub fn revoke(
        &mut self,
        caller: &Address,
        operator: &Address,
        token_ids: &[TokenID],
    ) -> Result<()> {
        // Attempt to instantiate the accounts if they don't exist
        let caller = self.msg.resolve_or_init(caller)?;
        let operator = match self.msg.resolve_id(operator) {
            Ok(id) => id,
            Err(_) => return Ok(()), // if operator didn't exist this is a no-op
        };

        self.state.revoke_for_tokens(&self.bs, caller, operator, token_ids)?;
        Ok(())
    }

    /// Approve an operator to transfer or burn on behalf of the account
    pub fn approve_for_owner(&mut self, caller: &Address, operator: &Address) -> Result<()> {
        // Attempt to instantiate the accounts if they don't exist
        let caller = self.msg.resolve_or_init(caller)?;
        let operator = self.msg.resolve_or_init(operator)?;
        self.state.approve_for_owner(&self.bs, caller, operator)?;
        Ok(())
    }

    /// Revoke the approval of an operator to transfer on behalf of the caller
    pub fn revoke_for_all(&mut self, caller: &Address, operator: &Address) -> Result<()> {
        // Attempt to instantiate the accounts if they don't exist
        let caller = self.msg.resolve_or_init(caller)?;
        let operator = match self.msg.resolve_id(operator) {
            Ok(id) => id,
            Err(_) => return Ok(()), // if operator didn't exist this is a no-op
        };

        self.state.revoke_for_all(&self.bs, caller, operator)?;
        Ok(())
    }

    /// Transfers a token owned by the caller
    pub fn transfer(
        &mut self,
        caller: &Address,
        recipient: &Address,
        token_ids: &[TokenID],
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferIntermediate>> {
        // Attempt to instantiate the accounts if they don't exist
        let caller = self.msg.resolve_or_init(caller)?;
        let recipient = self.msg.resolve_or_init(recipient)?;

        let hook = self.state.transfer_tokens(
            &self.bs,
            caller,
            recipient,
            token_ids,
            operator_data,
            token_data,
        )?;

        Ok(hook)
    }

    /// Transfers a token that the caller is an operator for
    pub fn transfer_from(
        &mut self,
        caller: &Address,
        recipient: &Address,
        token_ids: &[TokenID],
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferIntermediate>> {
        // Attempt to instantiate the accounts if they don't exist
        let caller = self.msg.resolve_or_init(caller)?;
        let recipient = self.msg.resolve_or_init(recipient)?;

        let hook = self.state.operator_transfer_tokens(
            &self.bs,
            caller,
            recipient,
            token_ids,
            operator_data,
            token_data,
        )?;

        Ok(hook)
    }
}
