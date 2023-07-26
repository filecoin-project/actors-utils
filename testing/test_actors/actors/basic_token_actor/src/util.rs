use frc46_token::token::TokenError;
use fvm_actor_utils::receiver::ReceiverHookError;
use fvm_ipld_encoding::{de::DeserializeOwned, RawBytes};
use fvm_sdk as sdk;
use fvm_shared::address::Address;
use thiserror::Error;

/// Errors that can occur during the execution of this actor
#[derive(Error, Debug)]
pub enum RuntimeError {
    /// Error from the underlying token library
    #[error("error in token: {0}")]
    Token(#[from] TokenError),
    /// Error from the underlying universal receiver hook library
    #[error("error calling receiver hook: {0}")]
    Receiver(#[from] ReceiverHookError),
}

pub fn caller_address() -> Address {
    let caller = sdk::message::caller();
    Address::new_id(caller)
}

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().unwrap();
    let params = RawBytes::new(params.data);
    params.deserialize().unwrap()
}
