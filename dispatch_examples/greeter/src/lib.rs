use frc42_dispatch::match_method;
use fvm_ipld_encoding::{RawBytes, DAG_CBOR};
use fvm_sdk as sdk;
use fvm_shared::error::ExitCode;
use sdk::NO_DATA_BLOCK_ID;

fn greet(name: &str) -> String {
    String::from("Hello, ") + name
}

#[no_mangle]
fn invoke(input: u32) -> u32 {
    let method_num = sdk::message::method_number();
    match_method!(method_num, {
        "Constructor" => {
            // this is a stateless actor so constructor does nothing
            NO_DATA_BLOCK_ID
        },
        "Greet" => {
            // Greet takes a name as a utf8 string
            // returns "Hello, {name}"
            let params = sdk::message::params_raw(input).unwrap().unwrap();
            let params = RawBytes::new(params.data);
            let name = params.deserialize::<String>().unwrap();

            let greeting = greet(&name);

            let bytes = fvm_ipld_encoding::to_vec(&greeting).unwrap();
            sdk::ipld::put_block(DAG_CBOR, bytes.as_slice()).unwrap()
        },
        _ => {
            sdk::vm::abort(
                ExitCode::USR_ILLEGAL_ARGUMENT.value(),
                Some("Unknown method number"),
            );
        }
    })
}
