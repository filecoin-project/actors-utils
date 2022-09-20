use frc42_dispatch::method_hash;
use frc46_token::token::state::TokenState;
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, econ::TokenAmount};

mod common;
use common::{construct_tester, TestHelpers, TokenHelpers};
use test_actor::{action, ActionParams, TestAction};

const BASIC_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_token_actor/basic_token_actor.compact.wasm";
const TEST_ACTOR_WASM: &str = "../../target/debug/wbuild/test_actor/test_actor.compact.wasm";

fn action_params(token_address: Address, action: TestAction) -> RawBytes {
    RawBytes::serialize(ActionParams { token_address, action }).unwrap()
}

#[test]
fn frc46_multi_actor_tests() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let operator: [Account; 1] = tester.create_accounts().unwrap();

    let initial_token_state = TokenState::new(&blockstore).unwrap();

    let token_actor =
        tester.install_actor_with_state(BASIC_TOKEN_ACTOR_WASM, 10000, initial_token_state);
    // we'll use up to four actors for some of these tests, though most use only two
    let alice = tester.install_actor_stateless(TEST_ACTOR_WASM, 10010);
    let bob = tester.install_actor_stateless(TEST_ACTOR_WASM, 10011);
    let carol = tester.install_actor_stateless(TEST_ACTOR_WASM, 10012);
    let dave = tester.install_actor_stateless(TEST_ACTOR_WASM, 10013);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct actors
    for actor in [token_actor, alice, bob, carol, dave] {
        let ret_val = tester.call_method(operator[0].1, actor, method_hash!("Constructor"), None);
        assert!(ret_val.msg_receipt.exit_code.is_success());
    }

    // TEST: alice sends bob a transfer of zero amount (rejecting first time and then accepting)
    // first, tell bob to reject it
    let params = action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Reject)));
    let ret_val = tester.call_method(operator[0].1, alice, method_hash!("Action"), Some(params));
    // we told bob to reject, so transfer should fail
    assert!(!ret_val.msg_receipt.exit_code.is_success());

    // this time tell bob to accept it
    let params = action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Accept)));
    let ret_val = tester.call_method(operator[0].1, alice, method_hash!("Action"), Some(params));
    // we told bob to accept this time, so transfer should succeed
    assert!(ret_val.msg_receipt.exit_code.is_success());

    // balance should remain zero
    let balance = tester.get_balance(operator[0].1, token_actor, alice);
    assert_eq!(balance, TokenAmount::from_atto(0));
    let balance = tester.get_balance(operator[0].1, token_actor, bob);
    assert_eq!(balance, TokenAmount::from_atto(0));

    // TEST: alice sends bob a transfer of a non-zero amounnt. As before, we'll reject it the first time then accept
    // mint some tokens to alice first
    let ret_val = tester.mint_tokens(
        operator[0].1,
        token_actor,
        alice,
        TokenAmount::from_atto(100),
        action(TestAction::Accept),
    );
    assert!(ret_val.msg_receipt.exit_code.is_success());
    let balance = tester.get_balance(operator[0].1, token_actor, alice);
    assert_eq!(balance, TokenAmount::from_atto(100));
    // now send to bob, who will reject them
    let params = action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Reject)));
    let ret_val = tester.call_method(operator[0].1, alice, method_hash!("Action"), Some(params));
    assert!(!ret_val.msg_receipt.exit_code.is_success());

    // transfer to bob who will accept it this time
    let params = action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Accept)));
    let ret_val = tester.call_method(operator[0].1, alice, method_hash!("Action"), Some(params));
    assert!(ret_val.msg_receipt.exit_code.is_success());
    // check balances
    let balance = tester.get_balance(operator[0].1, token_actor, alice);
    assert_eq!(balance, TokenAmount::from_atto(0));
    let balance = tester.get_balance(operator[0].1, token_actor, bob);
    assert_eq!(balance, TokenAmount::from_atto(100));
}
