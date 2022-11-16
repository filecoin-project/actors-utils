use frc42_dispatch::method_hash;
use fvm_actor_utils::receiver::{ReceiverHook, ReceiverHookError, ReceiverType, RecipientData};
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, econ::TokenAmount, ActorID};
use serde_tuple::{Deserialize_tuple, Serialize_tuple};

pub const FRC46_TOKEN_TYPE: ReceiverType = method_hash!("FRC46") as u32;

pub trait FRC46ReceiverHook<T: RecipientData> {
    fn new_frc46(
        address: Address,
        frc46_params: FRC46TokenReceived,
        result_data: T,
    ) -> std::result::Result<ReceiverHook<T>, ReceiverHookError>;
}

impl<T: RecipientData> FRC46ReceiverHook<T> for ReceiverHook<T> {
    /// Construct a new FRC46 ReceiverHook call
    fn new_frc46(
        address: Address,
        frc46_params: FRC46TokenReceived,
        result_data: T,
    ) -> std::result::Result<ReceiverHook<T>, ReceiverHookError> {
        Ok(ReceiverHook::new(
            address,
            RawBytes::serialize(frc46_params)?,
            FRC46_TOKEN_TYPE,
            result_data,
        ))
    }
}

/// Receive parameters for an FRC46 token
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct FRC46TokenReceived {
    /// The account that the tokens are being pulled from (the token actor address itself for mint)
    pub from: ActorID,
    /// The account that the tokens are being sent to (the receiver address)
    pub to: ActorID,
    /// Address of the operator that initiated the transfer/mint
    pub operator: ActorID,
    /// Amount of tokens being transferred/minted
    pub amount: TokenAmount,
    /// Data specified by the operator during transfer/mint
    pub operator_data: RawBytes,
    /// Additional data specified by the token-actor during transfer/mint
    pub token_data: RawBytes,
}
