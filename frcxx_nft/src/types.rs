use cid::Cid;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::address::Address;
use fvm_shared::ActorID;

type TokenID = u64;

pub trait FRCXXXNFT {
    /// A descriptive name for the collection of NFTs in this actor
    fn name(&self) -> String;

    /// An abbreviated name for NFTs in this contract
    fn symbol(&self) -> String;

    /// Gets a link to associated metadata for a given NFT
    fn metadata(&self, params: TokenID) -> Cid;

    /// Gets the total number of NFTs in this actor
    fn total_supply(&self) -> u64;

    /// Burns a given NFT, removing it from the total supply and preventing new NFTs from being
    /// minted with the same ID
    fn burn(&self, params: TokenID);

    /// Gets a list of all the tokens in the collection
    /// FIXME: make this paginated
    fn list_tokens(&self) -> Vec<TokenID>;

    /// Gets the number of tokens held by a particular address (if it exists)
    fn balance_of(&self, params: Address) -> u64;

    /// Returns the owner of the NFT specified by `token_id`
    fn owner_of(&self, params: TokenID) -> ActorID;

    /// Transfers specific NFTs from the caller to another account
    fn transfer(&self, params: TransferParams);

    /// Transfers specific NFTs between the `from` and `to` addresses
    fn transfer_from(&self, params: TransferFromParams);

    /// Change or reaffirm the approved address for a set of NFTs, setting to zero means there is no approved address
    fn approve(&self, params: ApproveParams);

    /// Set approval for all, allowing an operator to control all of the caller's tokens (including future tokens)
    /// until approval is revoked
    fn set_approval_for_all(&self, params: ApproveForAllParams);

    /// Get the approved address for a single NFT
    fn get_approved(&self, params: TokenID) -> ActorID;

    /// Query if the address is the approved operator for another address
    fn is_approved_for_all(&self, params: IsApprovedForAllParams) -> bool;
}

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct TransferParams {
    pub from: Address,
    pub to: Address,
    pub token_ids: Vec<TokenID>,
    pub operator_data: RawBytes,
}

impl Cbor for TransferParams {}

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct TransferFromParams {
    pub from: Address,
    pub to: Address,
    pub operator: Address,
    pub token_ids: Vec<TokenID>,
    pub operator_data: RawBytes,
}

impl Cbor for TransferFromParams {}

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct ApproveParams {
    pub operator: Address,
    pub token_ids: Vec<TokenID>,
}

impl Cbor for ApproveParams {}

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct ApproveForAllParams {
    pub operator: Address,
}

impl Cbor for ApproveForAllParams {}

#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct IsApprovedForAllParams {
    pub owner: Address,
    pub operator: Address,
}

impl Cbor for IsApprovedForAllParams {}
