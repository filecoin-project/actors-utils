use frc42_dispatch::method_hash;
use frcxx_nft::state::NFTState;
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, receipt::Receipt};

mod common;
// use common::frcxx_nft::NFTHelpers;
use common::{construct_tester, TestHelpers};
use frc46_test_actor::{action, ActionParams, TestAction};

const BASIC_NFT_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_nft_actor/basic_nft_actor.compact.wasm";
const TEST_ACTOR_WASM: &str =
    "../../target/debug/wbuild/frc46_test_actor/frc46_test_actor.compact.wasm";

fn action_params(token_address: Address, action: TestAction) -> RawBytes {
    RawBytes::serialize(ActionParams { token_address, action }).unwrap()
}

#[test]
fn frcxx_multi_actor_tests() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let operator: [Account; 1] = tester.create_accounts().unwrap();

    let initial_nft_state = NFTState::new(&blockstore).unwrap();

    let token_actor =
        tester.install_actor_with_state(BASIC_NFT_ACTOR_WASM, 10000, initial_nft_state);
    // we'll use up to four actors for some of these tests, though most use only two
    let alice = tester.install_actor_stateless(TEST_ACTOR_WASM, 10010);
    let bob = tester.install_actor_stateless(TEST_ACTOR_WASM, 10011);
    let carol = tester.install_actor_stateless(TEST_ACTOR_WASM, 10012);
    let dave = tester.install_actor_stateless(TEST_ACTOR_WASM, 10013);

    // instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct actors
    for actor in [token_actor, alice, bob, carol, dave] {
        let ret_val = tester.call_method(operator[0].1, actor, method_hash!("Constructor"), None);
        assert!(ret_val.msg_receipt.exit_code.is_success());
    }

    // TEST: alice sends bob a transfer of zero amount (rejecting first time and then accepting)
    {
        // first, tell bob to reject it
        let params =
            action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Reject)));
        let ret_val =
            tester.call_method_ok(operator[0].1, alice, method_hash!("Action"), Some(params));
        // we told bob to reject, so the action call should return success but give us the error result as return data
        // check the receipt we got in return data
        let receipt = ret_val.msg_receipt.return_data.deserialize::<Receipt>().unwrap();
        assert!(!receipt.exit_code.is_success());
    }
}
