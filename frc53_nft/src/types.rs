//! Interfaces and types for the frc53 NFT standard
use cid::Cid;
use fvm_actor_utils::receiver::RecipientData;
use fvm_ipld_bitfield::BitField;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::ActorID;

#[cfg(doc)]
use super::state::Cursor;

pub type TokenID = u64;

/// Multiple token IDs are represented as a BitField encoded with RLE+ the index of each set bit
/// corresponds to a TokenID.
pub type TokenSet = BitField;

/// Multiple actor IDs are represented as a BitField encoded with RLE+ the index of each set bit
/// corresponds to a ActorID.
pub type ActorIDSet = BitField;

/// A trait to be implemented by FRC-0053 compliant actors.
pub trait FRC53NFT {
    /// A descriptive name for the collection of NFTs in this actor.
    fn name(&self) -> String;

    /// An abbreviated name for NFTs in this contract.
    fn symbol(&self) -> String;

    /// Gets a link to associated metadata for a given NFT.
    fn metadata(&self, params: TokenID) -> Cid;

    /// Gets the total number of NFTs in this actor.
    fn total_supply(&self) -> u64;

    /// Burns a given NFT, removing it from the total supply and preventing new NFTs from being
    /// minted with the same ID.
    fn burn(&self, token_id: TokenID);

    /// Gets a list of all the tokens in the collection.
    // FIXME: make this paginated
    fn list_tokens(&self) -> Vec<TokenID>;

    /// Gets the number of tokens held by a particular address (if it exists).
    fn balance_of(&self, owner: Address) -> u64;

    /// Returns the owner of the NFT specified by `token_id`.
    fn owner_of(&self, token_id: TokenID) -> ActorID;

    /// Transfers specific NFTs from the caller to another account.
    fn transfer(&self, params: TransferParams);

    /// Transfers specific NFTs between the [`from`][`TransferFromParams::from`] and
    /// [`to`][`TransferFromParams::to`] addresses.
    fn transfer_from(&self, params: TransferFromParams);

    /// Change or reaffirm the approved address for a set of NFTs, setting to zero means there is no
    /// approved address.
    fn approve(&self, params: ApproveParams);

    /// Set approval for all, allowing an operator to control all of the caller's tokens (including
    /// future tokens) until approval is revoked.
    fn set_approval_for_all(&self, params: ApproveForAllParams);

    /// Get the approved address for a single NFT.
    fn get_approved(&self, params: TokenID) -> ActorID;

    /// Query if the address is the approved operator for another address.
    fn is_approved_for_all(&self, params: IsApprovedForAllParams) -> bool;
}

/// Return value after a successful mint.
///
/// The mint method is not standardised, so this is merely a useful library-level type, and
/// recommendation for token implementations.
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct MintReturn {
    /// The new balance of the owner address.
    pub balance: u64,
    /// The new total supply.
    pub supply: u64,
    /// List of the tokens that were minted successfully (some may have been burned during hook
    /// execution).
    pub token_ids: Vec<TokenID>,
    /// (Optional) data returned from the receiver hook.
    pub recipient_data: RawBytes,
}

/// Intermediate data used by mint_return to construct the return data.
#[derive(Clone, Debug)]
pub struct MintIntermediate {
    /// Receiving address used for querying balance.
    pub to: ActorID,
    /// List of the newly minted tokens.
    pub token_ids: Vec<TokenID>,
    /// (Optional) data returned from the receiver hook.
    pub recipient_data: RawBytes,
}

impl RecipientData for MintIntermediate {
    fn set_recipient_data(&mut self, data: RawBytes) {
        self.recipient_data = data;
    }
}

/// Intermediate data used by [`NFT::transfer_return`][`super::NFT::transfer_return`] to construct
/// the return data.
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TransferIntermediate {
    pub token_ids: Vec<TokenID>,
    pub from: ActorID,
    pub to: ActorID,
    /// (Optional) data returned from the receiver hook.
    pub recipient_data: RawBytes,
}

impl RecipientData for TransferIntermediate {
    fn set_recipient_data(&mut self, data: RawBytes) {
        self.recipient_data = data;
    }
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TransferParams {
    pub to: Address,
    pub token_ids: Vec<TokenID>,
    pub operator_data: RawBytes,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TransferReturn {
    pub from_balance: u64,
    pub to_balance: u64,
    pub token_ids: Vec<TokenID>,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct TransferFromParams {
    pub from: Address,
    pub to: Address,
    pub token_ids: Vec<TokenID>,
    pub operator_data: RawBytes,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct BurnFromParams {
    pub from: Address,
    pub token_ids: Vec<TokenID>,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ApproveParams {
    pub operator: Address,
    pub token_ids: Vec<TokenID>,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ApproveForAllParams {
    pub operator: Address,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct IsApprovedForAllParams {
    pub owner: Address,
    pub operator: Address,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct RevokeParams {
    pub operator: Address,
    pub token_ids: Vec<TokenID>,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct RevokeForAllParams {
    pub operator: Address,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListTokensParams {
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub cursor: RawBytes,
    pub limit: u64,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListTokensReturn {
    pub tokens: BitField,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub next_cursor: Option<RawBytes>,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListOwnedTokensParams {
    pub owner: Address,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub cursor: RawBytes,
    pub limit: u64,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListOwnedTokensReturn {
    pub tokens: TokenSet,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub next_cursor: Option<RawBytes>,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListTokenOperatorsParams {
    pub token_id: TokenID,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub cursor: RawBytes,
    pub limit: u64,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListTokenOperatorsReturn {
    pub operators: ActorIDSet,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub next_cursor: Option<RawBytes>,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListOperatorTokensParams {
    pub operator: Address,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub cursor: RawBytes,
    pub limit: u64,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListOperatorTokensReturn {
    pub tokens: TokenSet,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub next_cursor: Option<RawBytes>,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListAccountOperatorsParams {
    pub owner: Address,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub cursor: RawBytes,
    pub limit: u64,
}

#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct ListAccountOperatorsReturn {
    pub operators: ActorIDSet,
    /// Opaque serialisation of [`Cursor`], with empty cursor meaning start of list.
    pub next_cursor: Option<RawBytes>,
}
