use fil_fungible_token::token::types::{ActorError, Result};
use fvm_ipld_encoding::{de::DeserializeOwned, RawBytes};
use fvm_sdk as sdk;
use fvm_shared::{address::Address, econ::TokenAmount, ActorID, METHOD_SEND};
use num_traits::Zero;
use sdk::sys::ErrorNumber;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("sycall failed: {0}")]
    Syscall(ErrorNumber),
    #[error("address not resolvable")]
    AddrNotFound,
}

pub fn caller_address() -> Address {
    let caller = sdk::message::caller();
    Address::new_id(caller)
}

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().1;
    let params = RawBytes::new(params);
    let params = params.deserialize().unwrap();
    params
}
