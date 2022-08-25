//use fil_fungible_token::receiver::types::TokenReceivedParams;
use frc42_dispatch::match_method;
use fvm_ipld_encoding::{de::DeserializeOwned, RawBytes};
use fvm_sdk as sdk;
use fvm_shared::error::ExitCode;
use sdk::NO_DATA_BLOCK_ID;

/// Grab the incoming parameters and convert from RawBytes to deserialized struct
pub fn deserialize_params<O: DeserializeOwned>(params: u32) -> O {
    let params = sdk::message::params_raw(params).unwrap().1;
    let params = RawBytes::new(params);
    params.deserialize().unwrap()
}

#[no_mangle]
fn invoke(_input: u32) -> u32 {
    let method_num = sdk::message::method_number();
    match_method!(method_num, {
        "Constructor" => {
            // this is a stateless actor so constructor does nothing
            NO_DATA_BLOCK_ID
        },
        "TokensReceived" => {
            // TokensReceived is passed a TokenReceivedParams
            //let _params: TokenReceivedParams = deserialize_params(input);

            // decide if we care about incoming tokens or not
            // if we don't want them, abort

            NO_DATA_BLOCK_ID
        },
        _ => {
            sdk::vm::abort(
                ExitCode::USR_UNHANDLED_MESSAGE.value(),
                Some("Unknown method number"),
            );
        }
    })
}
