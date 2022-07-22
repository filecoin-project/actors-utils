use fvm_ipld_encoding::tuple::{Deserialize_tuple, Serialize_tuple};
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::bigint::bigint_ser;
use fvm_shared::{address::Address, econ::TokenAmount};

type Result<T> = std::result::Result<T, ()>;

/// Standard interface for an actor that wishes to receive FRC-XXX tokens
pub trait FrcXXXTokenReceiver {
    /// Invoked by a token actor during pending transfer to the receiver's address
    ///
    /// Within this hook, the token actor has optimistically persisted the new balance so
    /// the receiving actor can immediately utilise the received funds. If the receiver wishes to
    /// reject the incoming transfer, this function should abort which will cause the token actor
    /// to rollback the transaction.
    fn token_received(params: TokenReceivedParams) -> Result<()>;
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct TokenReceivedParams {
    pub sender: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
    pub data: RawBytes,
}

impl Cbor for TokenReceivedParams {}
