use frc42_dispatch::match_method;
use frc46_token::receiver::{FRC46TokenReceived, FRC46_TOKEN_TYPE};
use fvm_actor_utils::receiver::UniversalReceiverParams;
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
fn invoke(input: u32) -> u32 {
    std::panic::set_hook(Box::new(|info| {
        sdk::vm::abort(ExitCode::USR_ASSERTION_FAILED.value(), Some(&format!("{}", info)))
    }));

    let method_num = sdk::message::method_number();
    match_method!(method_num, {
        "Constructor" => {
            // this is a stateless actor so constructor does nothing
            NO_DATA_BLOCK_ID
        },
        "Receive" => {
            // Receive is passed a UniversalReceiverParams
            let params: UniversalReceiverParams = deserialize_params(input);

            // reject if not an FRC46 token
            // we don't know how to inspect other payloads in this example
            if params.type_ != FRC46_TOKEN_TYPE {
                panic!("invalid token type, rejecting transfer");
            }

            // get token transfer data
            let _token_params: FRC46TokenReceived = params.payload.deserialize().unwrap();
            // TODO: inspect token_params and decide if we'll accept the transfer
            // to reject it, we just abort (or panic, which does the same thing)

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
