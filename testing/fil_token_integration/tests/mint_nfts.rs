use std::env;

use cid::Cid;
use fil_fungible_token::token::state::TokenState;
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

const BASIC_NFT_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_nft_actor/basic_nft_actor.compact.wasm";
const BASIC_RECEIVER_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_receiving_actor/basic_receiving_actor.compact.wasm";

#[test]
fn mint_tokens() {
    let blockstore = MemoryBlockstore::default();
    let bundle_root = bundle::import_bundle(&blockstore, actors_v10::BUNDLE_CAR).unwrap();
    let mut tester =
        Tester::new(NetworkVersion::V15, StateTreeVersion::V4, bundle_root, blockstore.clone())
            .unwrap();

    let minter: [Account; 1] = tester.create_accounts().unwrap();

    // Get wasm bin
    let wasm_path = env::current_dir().unwrap().join(BASIC_NFT_ACTOR_WASM).canonicalize().unwrap();
    let wasm_bin = std::fs::read(wasm_path).expect("Unable to read token actor file");
    let rcvr_path =
        env::current_dir().unwrap().join(BASIC_RECEIVER_ACTOR_WASM).canonicalize().unwrap();
    let rcvr_bin = std::fs::read(rcvr_path).expect("Unable to read receiver actor file");

    // Set actor state
    let actor_state = TokenState::new(&blockstore).unwrap(); // TODO: this should probably not be exported from the package
    let state_cid = tester.set_state(&actor_state).unwrap();

    let actor_address = Address::new_id(10000);
    let receive_address = Address::new_id(10010);
    tester.set_actor_from_bin(&wasm_bin, state_cid, actor_address, TokenAmount::zero()).unwrap();
    tester
        .set_actor_from_bin(&rcvr_bin, Cid::default(), receive_address, TokenAmount::zero())
        .unwrap();

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // Helper to simplify sending messages
    let mut sequence = 0u64;
    let mut call_method = |from, to, method_num, params| {
        let message = Message {
            from,
            to,
            gas_limit: 99999999,
            method_num,
            sequence,
            params: if let Some(params) = params { params } else { RawBytes::default() },
            ..Message::default()
        };
        sequence += 1;
        tester
            .executor
            .as_mut()
            .unwrap()
            .execute_message(message, ApplyKind::Explicit, 100)
            .unwrap()
    };

    // Construct the token actor
    call_method(minter[0].1, actor_address, method_hash!("Constructor"), None);

    // TODO: assert that minting calls out to hook

    // Mint some tokens
    let ret_val = call_method(minter[0].1, actor_address, 2, None);
    assert!(ret_val.msg_receipt.exit_code.is_success());
    println!("mint single gas cost {:#?}", &ret_val.gas_burned);

    // Mint 10 tokens batched message
    let ret_val = call_method(minter[0].1, actor_address, 3, None);
    assert!(ret_val.msg_receipt.exit_code.is_success());
    println!("mint 10 (batched messaged) gas cost {:#?}", &ret_val.gas_burned);

    // Mint 10 tokens batched state operations
    let ret_val = call_method(minter[0].1, actor_address, 4, None);
    assert!(ret_val.msg_receipt.exit_code.is_success());
    println!("mint 10 (batched state op) gas cost {:#?}", &ret_val.gas_burned);
}
