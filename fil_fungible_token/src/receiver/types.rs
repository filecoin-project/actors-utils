use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::bigint::bigint_ser;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;

/// Standard interface for an actor that wishes to receive FRC-XXX tokens
pub trait FrcXXXTokenReceiver {
    /// Invoked by a token actor during pending transfer or mint to the receiver's address
    ///
    /// Within this hook, the token actor has optimistically persisted the new balance so
    /// the receiving actor can immediately utilise the received funds. If the receiver wishes to
    /// reject the incoming transfer, this function should abort which will cause the token actor
    /// to rollback the transaction.
    fn token_received(params: TokenReceivedParams);
}

#[derive(Serialize_tuple, Deserialize_tuple, PartialEq, Eq, Clone, Debug)]
pub struct TokenReceivedParams {
    /// Address of the operator that initiated the transfer/mint
    pub operator: ActorID,
    /// The account that the tokens are being pulled from (the token actor address itself for mint)
    pub from: ActorID,
    /// The account that the tokens are being sent to (the receiver address)
    pub to: ActorID,
    #[serde(with = "bigint_ser")]
    pub amount: TokenAmount,
    /// Data specified by the operator during transfer/mint
    pub data: RawBytes,
}

impl Cbor for TokenReceivedParams {}
