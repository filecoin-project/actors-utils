use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::{Cbor, RawBytes};
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser;
use fvm_shared::econ::TokenAmount;
use fvm_shared::ActorID;

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct ConstructorParams {
    pub mint_params: MintParams,
    pub name: String,
    pub symbol: String,
}

/// Called during construction of the token actor to set a supply
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct MintParams {
    pub initial_holder: ActorID,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct MintReturn {
    pub successful: bool,
    #[serde(with = "bigint_ser")]
    pub newly_minted: TokenAmount,
    #[serde(with = "bigint_ser")]
    pub total_supply: TokenAmount,
}

impl Cbor for MintParams {}
impl Cbor for MintReturn {}

/// An amount to increase or decrease an allowance by
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct ChangeAllowanceParams {
    pub owner: Address,
    pub spender: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

/// Params to get allowance between to addresses
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct GetAllowanceParams {
    pub owner: Address,
    pub spender: Address,
}

/// Instruction to revoke (set to 0) an allowance
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct RevokeAllowanceParams {
    pub owner: Address,
    pub spender: Address,
}

/// The updated value after allowance is increased or decreased
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct AllowanceReturn {
    pub owner: Address,
    pub spender: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

impl Cbor for ChangeAllowanceParams {}
impl Cbor for GetAllowanceParams {}
impl Cbor for RevokeAllowanceParams {}
impl Cbor for AllowanceReturn {}

/// Burns an amount of token from an address
#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct BurnParams {
    pub owner: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
    pub data: RawBytes,
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct BurnReturn {
    pub owner: Address,
    #[serde(with = "bigint_ser")]
    pub burnt: TokenAmount,
    #[serde(with = "bigint_ser")]
    pub remaining_balance: TokenAmount,
}

impl Cbor for BurnParams {}
impl Cbor for BurnReturn {}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct TransferParams {
    pub from: Address,
    pub to: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

#[derive(Serialize_tuple, Deserialize_tuple)]
pub struct TransferReturn {
    pub from: Address,
    pub to: Address,
    #[serde(with = "bigint_ser")]
    pub value: TokenAmount,
}

impl Cbor for TransferParams {}
impl Cbor for TransferReturn {}
