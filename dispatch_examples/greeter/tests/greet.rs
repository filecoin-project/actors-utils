use cid::Cid;
use frc42_dispatch::method_hash;
use fvm::executor::{ApplyKind, Executor};
use fvm_integration_tests::bundle;
use fvm_integration_tests::dummy::DummyExterns;
use fvm_integration_tests::tester::{Account, Tester};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use fvm_shared::state::StateTreeVersion;
use fvm_shared::version::NetworkVersion;

const DISPATCH_EXAMPLE_WASM: &str = "../../target/debug/wbuild/greeter/greeter.compact.wasm";

#[test]
fn test_greeter() {
    let blockstore = MemoryBlockstore::default();
    let bundle_root = bundle::import_bundle(&blockstore, actors_v10::BUNDLE_CAR).unwrap();
    let mut tester =
        Tester::new(NetworkVersion::V18, StateTreeVersion::V5, bundle_root, blockstore).unwrap();

    let wasm_path =
        std::env::current_dir().unwrap().join(DISPATCH_EXAMPLE_WASM).canonicalize().unwrap();
    let wasm_bin = std::fs::read(wasm_path).expect("Unable to read file");

    // set up a test environment with user/actor addresses and no initial state
    let user: [Account; 1] = tester.create_accounts().unwrap();
    let actor_address = Address::new_id(10000);
    tester
        .set_actor_from_bin(&wasm_bin, Cid::default(), actor_address, TokenAmount::zero())
        .unwrap();

    tester.instantiate_machine(DummyExterns).unwrap();

    // call Constructor
    let message = Message {
        from: user[0].1,
        to: actor_address,
        gas_limit: 99999999,
        method_num: method_hash!("Constructor"),
        sequence: 0,
        ..Message::default()
    };

    let _ret_val = tester
        .executor
        .as_mut()
        .unwrap()
        .execute_message(message, ApplyKind::Explicit, 100)
        .unwrap();

    // call Greet for a classic "hello world"
    let params = RawBytes::serialize(String::from("World!")).unwrap();

    let message = Message {
        from: user[0].1,
        to: actor_address,
        gas_limit: 99999999,
        method_num: method_hash!("Greet"),
        sequence: 1,
        params,
        ..Message::default()
    };

    let ret_val = tester
        .executor
        .as_mut()
        .unwrap()
        .execute_message(message, ApplyKind::Explicit, 100)
        .unwrap();

    // get result
    let return_data = ret_val.msg_receipt.return_data;
    let greeting: String = return_data.deserialize().unwrap();
    // display the result - run `cargo test -- --nocapture` to see output
    println!("greeting: {greeting}");

    assert_eq!(greeting, "Hello, World!")
}
