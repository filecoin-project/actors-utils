use frc42_dispatch::method_hash;
use fvm_actor_utils::receiver::{ReceiverHook, ReceiverHookError, ReceiverType, RecipientData};
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, ActorID};
use serde_tuple::{Deserialize_tuple, Serialize_tuple};

use crate::types::TokenID;

pub const FRC53_TOKEN_TYPE: ReceiverType = method_hash!("FRC53") as u32;

pub trait FRC53ReceiverHook<T: RecipientData> {
    fn new_frc53(
        address: Address,
        frc53_params: FRC53TokenReceived,
        result_data: T,
    ) -> std::result::Result<ReceiverHook<T>, ReceiverHookError>;
}

impl<T: RecipientData> FRC53ReceiverHook<T> for ReceiverHook<T> {
    /// Construct a new FRC46 ReceiverHook call
    fn new_frc53(
        address: Address,
        frc53_params: FRC53TokenReceived,
        result_data: T,
    ) -> std::result::Result<ReceiverHook<T>, ReceiverHookError> {
        Ok(ReceiverHook::new(
            address,
            RawBytes::serialize(frc53_params)?,
            FRC53_TOKEN_TYPE,
            result_data,
        ))
    }
}

/// Receive parameters for an FRC53 token
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct FRC53TokenReceived {
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
