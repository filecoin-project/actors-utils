use frc42_dispatch::method_hash;
use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::{econ::TokenAmount, ActorID};

use super::ReceiverType;

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
