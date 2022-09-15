use frc42_dispatch::method_hash;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;

/// Standard interface for an actor that wishes to receive FRC-0046 tokens or other assets
pub trait UniversalReceiver {
    /// Invoked by a token actor during pending transfer or mint to the receiver's address
    ///
    /// Within this hook, the token actor has optimistically persisted the new balance so
    /// the receiving actor can immediately utilise the received funds. If the receiver wishes to
    /// reject the incoming transfer, this function should abort which will cause the token actor
    /// to rollback the transaction.
    fn receive(params: UniversalReceiverParams);
}

/// Type of asset received - could be tokens (FRC46 or other) or other assets
pub type ReceiverType = u32;
pub const FRC46_TOKEN_TYPE: ReceiverType = method_hash!("FRC46") as u32;

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

impl Cbor for FRC46TokenReceived {}

/// Parameters for universal receiver
///
/// Actual payload varies with asset type
/// eg: FRC46_TOKEN_TYPE will come with a payload of FRC46TokenReceived
#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct UniversalReceiverParams {
    /// Asset type
    pub type_: ReceiverType,
    /// Payload corresponding to asset type
    pub payload: RawBytes,
}
impl Cbor for UniversalReceiverParams {}
