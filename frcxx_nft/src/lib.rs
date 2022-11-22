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
use receiver::{FRCXXReceiverHook, FRCXXTokenReceived};
use state::StateError;
use thiserror::Error;
use types::{MintIntermediate, MintReturn, TransferIntermediate, TransferReturn};

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

    /// Opens an atomic transaction on TokenState which allows a closure to make multiple
    /// modifications to the state tree.
    ///
    /// If errors are returned by any intermediate state method, it is recommended to abort the
    /// entire transaction by propagating the error. If state-level errors are explicitly handled,
    /// it is necessary to reload from the blockstore any passed-in owner HAMT or token AMT to ensure
    /// partial writes are dropped.
    ///
    /// If the closure returns an error, the transaction is dropped atomically and no change is
    /// observed on token state.
    pub fn transaction<F, Res>(&mut self, f: F) -> Result<Res>
    where
        F: FnOnce(&mut NFTState, &BS) -> Result<Res>,
    {
        let mut mutable_state = self.state.clone();
        let res = f(&mut mutable_state, &self.bs)?;
        // if closure didn't error save state
        *self.state = mutable_state;
        Ok(res)
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

    /// Return the number of NFTs held by a particular address
    pub fn balance_of(&self, address: &Address) -> Result<u64> {
        let balance = match self.msg.resolve_id(address) {
            Ok(owner) => self.state.get_balance(&self.bs, owner)?,
            Err(MessagingError::AddressNotResolved(_)) => 0,
            Err(e) => return Err(e.into()),
        };
        Ok(balance)
    }

    /// Return the owner of an NFT
    pub fn owner_of(&self, token_id: TokenID) -> Result<ActorID> {
        Ok(self.state.get_owner(&self.bs, token_id)?)
    }

    /// Return the metadata for an NFT
    pub fn metadata(&self, token_id: TokenID) -> Result<String> {
        Ok(self.state.get_metadata(&self.bs, token_id)?)
    }

    /// Create new NFTs belonging to the initial_owner. The mint method is not standardised
    /// as part of the actor's interface but this is a usefuly method at the library level to
    /// generate new tokens that will maintain the necessary state invariants.
    ///
    /// For each string in metadata_array, a new NFT will be minted with the given metadata.
    ///
    /// Returns a MintIntermediate that can be used to construct return data
    pub fn mint(
        &mut self,
        operator: &Address,
        initial_owner: &Address,
        metadata_array: Vec<String>,
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<MintIntermediate>> {
        let operator = self.msg.resolve_id(operator)?;
        let initial_owner = self.msg.resolve_or_init(initial_owner)?;
        Ok(self.state.mint_tokens(
            &self.bs,
            operator,
            initial_owner,
            metadata_array,
            operator_data,
            token_data,
        )?)
    }

    /// Constructs MintReturn data from a MintIntermediate handle
    ///
    /// Creates an up-to-date view of the actor state where necessary to generate the values
    /// `prior_state_cid` is the CID of the state prior to hook call
    pub fn mint_return(
        &mut self,
        intermediate: MintIntermediate,
        prior_state_cid: Cid,
    ) -> Result<MintReturn> {
        self.reload_if_changed(prior_state_cid)?;
        Ok(self.state.mint_return(&self.bs, intermediate)?)
    }

    /// Burn a set of NFTs as the owner
    ///
    /// A burnt TokenID can never be minted again
    pub fn burn(&mut self, owner: &Address, token_ids: &[TokenID]) -> Result<u64> {
        let owner = self.msg.resolve_id(owner)?;

        let balance = self.transaction(|state, bs| {
            let mut token_array = state.get_token_data_amt(bs)?;
            let mut owner_map = state.get_owner_data_hamt(bs)?;
            NFTState::assert_owns_tokens(&token_array, owner, token_ids)?;

            let res = state.burn_tokens(&mut token_array, &mut owner_map, owner, token_ids)?;

            state.token_data = token_array.flush().map_err(StateError::from)?;
            state.owner_data = owner_map.flush().map_err(StateError::from)?;
            Ok(res)
        })?;

        Ok(balance)
    }

    /// Burn a set of NFTs as an operator
    ///
    /// A burnt TokenID can never be minted again
    pub fn burn_from(
        &mut self,
        owner: &Address,
        operator: &Address,
        token_ids: &[TokenID],
    ) -> Result<u64> {
        let operator = self.msg.resolve_id(operator)?;
        let owner = self.msg.resolve_or_init(owner)?;

        let balance = self.transaction(|state, bs| {
            let mut token_array = state.get_token_data_amt(bs)?;
            let mut owner_map = state.get_owner_data_hamt(bs)?;
            // check the tokens are all owned by the same expected account
            NFTState::assert_owns_tokens(&token_array, owner, token_ids)?;
            // check that the operator has permission to burn the tokens
            NFTState::assert_approved_for_tokens(&token_array, &owner_map, operator, token_ids)?;

            let res = state.burn_tokens(&mut token_array, &mut owner_map, owner, token_ids)?;

            state.token_data = token_array.flush().map_err(StateError::from)?;
            state.owner_data = owner_map.flush().map_err(StateError::from)?;
            Ok(res)
        })?;

        Ok(balance)
    }

    /// Approve an operator to transfer or burn a single NFT
    ///
    /// `caller` may be an account-level operator or owner of the NFT
    /// `operator` is the new address to become an approved operator
    pub fn approve(
        &mut self,
        caller: &Address,
        operator: &Address,
        token_ids: &[TokenID],
    ) -> Result<()> {
        // Attempt to instantiate the accounts if they don't exist
        let caller = self.msg.resolve_id(caller)?;
        let operator = self.msg.resolve_or_init(operator)?;

        self.transaction(|state, bs| {
            let mut token_array = state.get_token_data_amt(bs)?;
            NFTState::assert_owns_tokens(&token_array, caller, token_ids)?;

            state.approve_for_tokens(&mut token_array, operator, token_ids)?;

            state.token_data = token_array.flush().map_err(StateError::from)?;
            Ok(())
        })?;

        Ok(())
    }

    /// Revoke the approval of an operator to transfer a particular NFT
    ///
    /// `caller` may be an account-level operator or owner of the NFT
    /// `operator` is the address whose approval is being revoked
    pub fn revoke(
        &mut self,
        caller: &Address,
        operator: &Address,
        token_ids: &[TokenID],
    ) -> Result<()> {
        // Attempt to instantiate the accounts if they don't exist
        let caller = self.msg.resolve_id(caller)?;
        let operator = match self.msg.resolve_id(operator) {
            Ok(id) => id,
            Err(_) => return Ok(()), // if operator didn't exist this is a no-op
        };

        self.transaction(|state, bs| {
            let mut token_array = state.get_token_data_amt(bs)?;
            NFTState::assert_owns_tokens(&token_array, caller, token_ids)?;

            state.revoke_for_tokens(&mut token_array, operator, token_ids)?;

            state.token_data = token_array.flush().map_err(StateError::from)?;
            Ok(())
        })
    }

    /// Approve an operator to transfer or burn on behalf of the account
    ///
    /// `owner` must be the address that called this method
    /// `operator` is the new address to become an approved operator
    pub fn approve_for_owner(&mut self, owner: &Address, operator: &Address) -> Result<()> {
        let owner = self.msg.resolve_id(owner)?;
        // Attempt to instantiate the accounts if they don't exist
        let operator = self.msg.resolve_or_init(operator)?;

        self.transaction(|state, bs| {
            let mut owner_map = state.get_owner_data_hamt(bs)?;

            state.approve_for_owner(&mut owner_map, owner, operator)?;

            state.owner_data = owner_map.flush().map_err(StateError::from)?;
            Ok(())
        })
    }

    /// Revoke the approval of an operator to transfer on behalf of the caller
    ///
    /// `owner` must be the address that called this method
    /// `operator` is the address whose approval is being revoked
    pub fn revoke_for_all(&mut self, owner: &Address, operator: &Address) -> Result<()> {
        let owner = self.msg.resolve_id(owner)?;
        let operator = match self.msg.resolve_id(operator) {
            Ok(id) => id,
            Err(_) => return Ok(()), // if operator didn't exist this is a no-op
        };

        self.transaction(|state, bs| {
            let mut owner_map = state.get_owner_data_hamt(bs)?;

            state.revoke_for_all(&mut owner_map, owner, operator)?;

            state.owner_data = owner_map.flush().map_err(StateError::from)?;
            Ok(())
        })
    }

    /// Transfers a token owned by the caller
    pub fn transfer(
        &mut self,
        owner: &Address,
        recipient: &Address,
        token_ids: &[TokenID],
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferIntermediate>> {
        // Attempt to instantiate the accounts if they don't exist
        let owner_id = self.msg.resolve_or_init(owner)?;
        let recipient_id = self.msg.resolve_or_init(recipient)?;

        self.transaction(|state, store| {
            let mut token_array = state.get_token_data_amt(store)?;
            let mut owner_map = state.get_owner_data_hamt(store)?;
            NFTState::assert_owns_tokens(&token_array, owner_id, token_ids)?;

            for &token_id in token_ids {
                // update the token_data to reflect the new owner and clear approved operators
                state.make_transfer(&mut token_array, &mut owner_map, token_id, recipient_id)?;
            }

            state.token_data = token_array.flush().map_err(StateError::from)?;
            state.owner_data = owner_map.flush().map_err(StateError::from)?;
            Ok(())
        })?;

        let params = FRCXXTokenReceived {
            to: recipient_id,
            operator: owner_id,
            token_ids: token_ids.into(),
            operator_data,
            token_data,
        };

        let intermediate = TransferIntermediate {
            to: recipient_id,
            from: owner_id,
            token_ids: token_ids.into(),
            recipient_data: RawBytes::default(),
        };

        Ok(ReceiverHook::new_frcxx(*recipient, params, intermediate).map_err(StateError::from)?)
    }

    /// Constructs TransferReturn data from a TransferIntermediate
    ///
    /// Creates an up-to-date view of the actor state where necessary to generate the values
    /// `prior_state_cid` is the CID of the state prior to hook call
    pub fn transfer_return(
        &mut self,
        intermediate: TransferIntermediate,
        prior_state_cid: Cid,
    ) -> Result<TransferReturn> {
        self.reload_if_changed(prior_state_cid)?;
        Ok(self.state.transfer_return(&self.bs, intermediate)?)
    }

    /// Transfers a token that the caller is an operator for
    pub fn transfer_from(
        &mut self,
        owner: &Address,
        operator: &Address,
        recipient: &Address,
        token_ids: &[TokenID],
        operator_data: RawBytes,
        token_data: RawBytes,
    ) -> Result<ReceiverHook<TransferIntermediate>> {
        // Attempt to instantiate the accounts if they don't exist
        let owner_id = self.msg.resolve_id(owner)?;
        let operator_id = self.msg.resolve_id(operator)?;
        let recipient_id = self.msg.resolve_or_init(recipient)?;

        self.transaction(|state, store| {
            let mut token_array = state.get_token_data_amt(store)?;
            let mut owner_map = state.get_owner_data_hamt(store)?;
            NFTState::assert_owns_tokens(&token_array, owner_id, token_ids)?;
            NFTState::assert_approved_for_tokens(&token_array, &owner_map, operator_id, token_ids)?;

            for &token_id in token_ids {
                // update the token_data to reflect the new owner and clear approved operators
                state.make_transfer(&mut token_array, &mut owner_map, token_id, recipient_id)?;
            }

            state.token_data = token_array.flush().map_err(StateError::from)?;
            state.owner_data = owner_map.flush().map_err(StateError::from)?;
            Ok(())
        })?;

        let params = FRCXXTokenReceived {
            to: recipient_id,
            operator: owner_id,
            token_ids: token_ids.into(),
            operator_data,
            token_data,
        };

        let intermediate = TransferIntermediate {
            to: recipient_id,
            from: owner_id,
            token_ids: token_ids.into(),
            recipient_data: RawBytes::default(),
        };

        Ok(ReceiverHook::new_frcxx(*recipient, params, intermediate).map_err(StateError::from)?)
    }

    /// Constructs TransferReturn data from a TransferIntermediate
    ///
    /// Creates an up-to-date view of the actor state where necessary to generate the values
    /// `prior_state_cid` is the CID of the state prior to hook call
    pub fn transfer_from_return(
        &mut self,
        intermediate: TransferIntermediate,
        prior_state_cid: Cid,
    ) -> Result<TransferReturn> {
        self.reload_if_changed(prior_state_cid)?;
        Ok(self.state.transfer_return(&self.bs, intermediate)?)
    }

    /// Reloads the state if the current root cid has diverged (i.e. during re-entrant receiver hooks)
    /// from the last known expected cid
    ///
    /// Returns the current in-blockstore state if the root cid has changed else None
    pub fn reload_if_changed(&mut self, expected_cid: Cid) -> Result<Option<NFTState>> {
        let current_cid = self.actor.root_cid()?;
        if current_cid != expected_cid {
            let old_state = self.load_replace(&current_cid)?;
            Ok(Some(old_state))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod test {

    use fvm_actor_utils::{actor::FakeActor, messaging::FakeMessenger};
    use fvm_ipld_blockstore::MemoryBlockstore;
    use fvm_ipld_encoding::RawBytes;
    use fvm_shared::{address::Address, ActorID};

    use crate::{state::StateError, NFTError, NFTState, NFT};

    const ALICE_ID: ActorID = 1;
    const ALICE: Address = Address::new_id(ALICE_ID);
    const BOB_ID: ActorID = 2;
    const BOB: Address = Address::new_id(BOB_ID);

    #[test]
    fn it_transfers_tokens() {
        let bs = MemoryBlockstore::default();
        let mut state = NFTState::new(&bs).unwrap();
        let msg = FakeMessenger::new(4, 5);
        let mut nft =
            NFT::wrap(bs.clone(), msg, FakeActor { root: state.save(&bs).unwrap() }, &mut state);

        {
            // mint tokens to alice
            let mut hook = nft
                .mint(
                    &ALICE,
                    &ALICE,
                    vec![String::new(); 3],
                    RawBytes::default(),
                    RawBytes::default(),
                )
                .unwrap();
            hook.call(&nft.msg).unwrap();
            // alice: [0, 1, 2]
            // bob: []
        }

        {
            // transfer tokens from alice to bob
            let mut hook = nft
                .transfer(&ALICE, &BOB, &[0, 1, 2], RawBytes::default(), RawBytes::default())
                .unwrap();
            hook.call(&nft.msg).unwrap();
            // alice: []
            // bob: [0, 1, 2]
        }

        {
            // alice's tokens go to bob
            let alice_balance = nft.balance_of(&ALICE).unwrap();
            assert_eq!(alice_balance, 0);
            let bob_balance = nft.balance_of(&BOB).unwrap();
            assert_eq!(bob_balance, 3);
        }

        {
            // alice is denied permission to transfer them back
            // transfer tokens from alice to bob
            let err = nft
                .transfer(&ALICE, &ALICE, &[0, 1, 2], RawBytes::default(), RawBytes::default())
                .unwrap_err();
            if let NFTError::NFTState(StateError::NotOwner { actor, token_id }) = err {
                assert_eq!(actor, ALICE_ID);
                assert_eq!(token_id, 0);
            } else {
                panic!("Unexpected error: {:?}", err);
            }
            // alice: []
            // bob: [0, 1, 2]
        }

        {
            // state didn't change
            let alice_balance = nft.balance_of(&ALICE).unwrap();
            assert_eq!(alice_balance, 0);
            let bob_balance = nft.balance_of(&BOB).unwrap();
            assert_eq!(bob_balance, 3);
        }

        {
            // doesn't transfer if any of the token ids are invalid
            let err = nft
                .transfer(&BOB, &ALICE, &[0, 1, 2, 3], RawBytes::default(), RawBytes::default())
                .unwrap_err();
            if let NFTError::NFTState(StateError::TokenNotFound(token_id)) = err {
                assert_eq!(token_id, 3);
            } else {
                panic!("Unexpected error: {:?}", err);
            }
            // alice: []
            // bob: [0, 1, 2]
        }

        {
            // state didn't change
            let alice_balance = nft.balance_of(&ALICE).unwrap();
            assert_eq!(alice_balance, 0);
            let bob_balance = nft.balance_of(&BOB).unwrap();
            assert_eq!(bob_balance, 3);
        }

        /*
        TODO: either explicitly check for duplicates or use a bitfield representation to prevent duplicates being specified
        {
            // doesn't transfer if there are duplicates
            let err = nft
                .transfer(&BOB, &ALICE, &[0, 1, 0], RawBytes::default(), RawBytes::default())
                .unwrap_err();
            if let NFTError::NFTState(StateError::NotOwner { actor, token_id }) = err {
                assert_eq!(actor, BOB_ID);
                assert_eq!(token_id, 0);
            } else {
                panic!("Unexpected error: {:?}", err);
            }
            // alice: []
            // bob: [0, 1, 2]
        }

        {
            // state didn't change
            let alice_balance = nft.balance_of(&ALICE).unwrap();
            assert_eq!(alice_balance, 0);
            let bob_balance = nft.balance_of(&BOB).unwrap();
            assert_eq!(bob_balance, 3);
        }
         */
    }
}
