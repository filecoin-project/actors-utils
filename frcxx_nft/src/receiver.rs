use frc42_dispatch::method_hash;
use fvm_actor_utils::receiver::{ReceiverHook, ReceiverHookError, ReceiverType, RecipientData};
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::{address::Address, ActorID};
use serde_tuple::{Deserialize_tuple, Serialize_tuple};

use crate::state::TokenID;

pub const FRCXX_TOKEN_TYPE: ReceiverType = method_hash!("FRCXX") as u32;

pub trait FRCXXReceiverHook<T: RecipientData> {
    fn new_frcxx(
        address: Address,
        frcxx_params: FRCXXTokenReceived,
        result_data: T,
    ) -> std::result::Result<ReceiverHook<T>, ReceiverHookError>;
}

impl<T: RecipientData> FRCXXReceiverHook<T> for ReceiverHook<T> {
    /// Construct a new FRC46 ReceiverHook call
    fn new_frcxx(
        address: Address,
        frcxx_params: FRCXXTokenReceived,
        result_data: T,
    ) -> std::result::Result<ReceiverHook<T>, ReceiverHookError> {
        Ok(ReceiverHook::new(
            address,
            RawBytes::serialize(&frcxx_params)?,
            FRCXX_TOKEN_TYPE,
            result_data,
        ))
    }
}

/// Receive parameters for an FRCXX token
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct FRCXXTokenReceived {
    /// The account that the tokens are being sent to (the receiver address)
    pub to: ActorID,
    /// Address of the operator that initiated the transfer/mint
    pub operator: ActorID,
    /// Amount of tokens being transferred/minted
    pub token_ids: Vec<TokenID>,
    /// Data specified by the operator during transfer/mint
    pub operator_data: RawBytes,
    /// Additional data specified by the token-actor during transfer/mint
    pub token_data: RawBytes,
}

impl Cbor for FRCXXTokenReceived {}
