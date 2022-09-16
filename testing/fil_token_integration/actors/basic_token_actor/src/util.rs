use frc46_token::token::TokenError;
use fvm_ipld_encoding::{de::DeserializeOwned, RawBytes};
use fvm_sdk as sdk;
use fvm_shared::address::Address;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("error in token: {0}")]
    Token(#[from] TokenError),
}

pub fn caller_address() -> Address {
    let caller = sdk::message::caller();
    Address::new_id(caller)
}

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().1;
    let params = RawBytes::new(params);
    params.deserialize().unwrap()
}
