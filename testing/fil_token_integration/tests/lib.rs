use std::env;

use fil_fungible_token::blockstore::SharedMemoryBlockstore;
use fil_fungible_token::token::state::TokenState;
use fvm::executor::{ApplyKind, Executor};
use fvm_integration_tests::tester::{Account, Tester};
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use fvm_shared::state::StateTreeVersion;
use fvm_shared::version::NetworkVersion;

const BASIC_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_token_actor/basic_token_actor.compact.wasm";

#[test]
fn mint_tokens() {
    let blockstore = SharedMemoryBlockstore::default();
    let mut tester =
        Tester::new(NetworkVersion::V15, StateTreeVersion::V4, blockstore.clone()).unwrap();

    let minter: [Account; 1] = tester.create_accounts().unwrap();

    // Get wasm bin
    let wasm_path =
        env::current_dir().unwrap().join(BASIC_TOKEN_ACTOR_WASM).canonicalize().unwrap();
    let wasm_bin = std::fs::read(wasm_path).expect("Unable to read file");

    // Set actor state
    let actor_state = TokenState::new(&blockstore).unwrap(); // TODO: this should probably not be exported from the package
    let state_cid = tester.set_state(&actor_state).unwrap();

    let actor_address = Address::new_id(10000);
    tester.set_actor_from_bin(&wasm_bin, state_cid, actor_address, TokenAmount::zero()).unwrap();

    // Instantiate machine
    tester.instantiate_machine().unwrap();

    let message = Message {
        from: minter[0].1,
        to: actor_address,
        gas_limit: 99999999,
        method_num: 1, // 1 is constructor
        sequence: 0,
        ..Message::default()
    };

    let ret_val = tester
        .executor
        .as_mut()
        .unwrap()
        .execute_message(message, ApplyKind::Explicit, 100)
        .unwrap();

    println!("return data {:?}", &ret_val);

    let message = Message {
        from: minter[0].1,
        to: actor_address,
        gas_limit: 99999999,
        method_num: 12, // 12 is Mint function
        sequence: 1,
        ..Message::default()
    };

    let ret_val = tester
        .executor
        .as_mut()
        .unwrap()
        .execute_message(message, ApplyKind::Explicit, 100)
        .unwrap();

    println!("return data {:?}", &ret_val);

    let params = RawBytes::serialize(minter[0].1).unwrap();

    let message = Message {
        from: minter[0].1,
        to: actor_address,
        gas_limit: 99999999,
        method_num: 5, // 5 is balance of
        sequence: 2,
        params,
        ..Message::default()
    };

    let ret_val = tester
        .executor
        .as_mut()
        .unwrap()
        .execute_message(message, ApplyKind::Explicit, 100)
        .unwrap();

    println!("return data {:?}", &ret_val);

    let return_data = ret_val.msg_receipt.return_data;
    let balance: BigIntDe = return_data.deserialize().unwrap();
    println!("balance: {:?}", balance);
}
