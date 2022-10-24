//! This acts as the reference library for FRCXX. While remaining complaint with the
//! spec, this library is opinionated in its batching, minting and storage
//! strategies to optimize for common usage patterns.
//!
//! For example, write operations are generally optimised over read operations as
//! on-chain state can be read by direct inspection (rather than via an actor call)
//! in many cases.

use cid::Cid;
use fvm_actor_utils::{
    actor::{Actor, ActorError},
    messaging::{Messaging, MessagingError},
    receiver::ReceiverHook,
};
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, ActorID};
use state::StateError;
use thiserror::Error;
use types::{
    MintIntermediate, MintReturn, TransferFromIntermediate, TransferFromReturn,
    TransferIntermediate, TransferReturn,
};

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
    #[error("error in runtime: {0}")]
    Actor(#[from] ActorError),
}

pub type Result<T> = std::result::Result<T, NFTError>;

/// A helper handle for NFTState that injects services into the state-level operations
pub struct NFT<'st, BS, MSG, A>
where
    BS: Blockstore,
    MSG: Messaging,
    A: Actor,
{
    bs: BS,
    msg: MSG,
    state: &'st mut NFTState,
    actor: A,
}

impl<'st, BS, MSG, A> NFT<'st, BS, MSG, A>
where
    BS: Blockstore,
    MSG: Messaging,
    A: Actor,
{
    /// Wrap an instance of the state-tree in a handle for higher-level operations
    pub fn wrap(bs: BS, msg: MSG, actor: A, state: &'st mut NFTState) -> Self {
        Self { bs, msg, actor, state }
    }

    /// Flush state and return Cid for root
    pub fn flush(&mut self) -> Result<Cid> {
        Ok(self.state.save(&self.bs)?)
    }

    /// Loads a fresh copy of the state from a blockstore from a given cid, replacing existing state
    /// The old state is returned for convenience but can be safely dropped
    pub fn load_replace(&mut self, cid: &Cid) -> Result<NFTState> {
        let new_state = NFTState::load(&self.bs, cid)?;
        Ok(std::mem::replace(self.state, new_state))
    }
}

impl<'st, BS, MSG, A> NFT<'st, BS, MSG, A>
where
    BS: Blockstore,
    MSG: Messaging,
    A: Actor,
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

    /// Constructs MintReturn data from a MintIntermediate handle
    ///
    /// Creates an up-to-date view of the actor state where necessary to generate the values
    pub fn mint_return(&mut self, intermediate: MintIntermediate, cid: Cid) -> Result<MintReturn> {
        self.reload_if_changed(cid)?;
        Ok(self.state.mint_return(&self.bs, intermediate)?)
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

    /// Constructs TransferReturn data from a TransferIntermediate
    ///
    /// Creates an up-to-date view of the actor state where necessary to generate the values
    pub fn transfer_return(
        &mut self,
        intermediate: TransferIntermediate,
        cid: Cid,
    ) -> Result<TransferReturn> {
        self.reload_if_changed(cid)?;
        Ok(self.state.transfer_return(&self.bs, intermediate)?)
    }

    /// Transfers a token that the caller is an operator for
    pub fn transfer_from(
        &mut self,
        caller: &Address,
        recipient: &Address,
        token_ids: &[TokenID],
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferFromIntermediate>> {
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

    /// Constructs TransferReturn data from a TransferIntermediate
    ///
    /// Creates an up-to-date view of the actor state where necessary to generate the values
    pub fn transfer_from_return(
        &mut self,
        intermediate: TransferFromIntermediate,
        cid: Cid,
    ) -> Result<TransferFromReturn> {
        self.reload_if_changed(cid)?;
        Ok(self.state.transfer_from_return(&self.bs, intermediate)?)
    }

    /// Reloads the state if the root cid has diverged (i.e. during re-entrant receiver hooks)
    /// from the passed in cid
    ///
    /// Returns the current in-memory state if the root cid has changed else None
    pub fn reload_if_changed(&mut self, cid: Cid) -> Result<Option<NFTState>> {
        let new_cid = self.actor.root_cid()?;
        if new_cid != cid {
            let old_state = self.load_replace(&new_cid)?;
            Ok(Some(old_state))
        } else {
            Ok(None)
        }
    }
}
