use frc42_dispatch::method_hash;
use frc46_token::token::{state::TokenState, types::MintReturn};
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::econ::TokenAmount;

mod common;
use common::{construct_tester, TestHelpers, TokenHelpers};
use test_actor::{action, ActionParams, TestAction};

const BASIC_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_token_actor/basic_token_actor.compact.wasm";
const TEST_ACTOR_WASM: &str = "../../target/debug/wbuild/test_actor/test_actor.compact.wasm";

/// This covers several simpler tests, which all involve a single receiving actor
/// They're combined because these integration tests take a long time to build and run
/// Test cases covered:
/// - mint to test actor who rejects in receiver hook
/// - mint to self (token actor - should be rejected)
/// - mint to test actor who burns tokens upon receipt (calling Burn from within the hook)
/// - test actor transfers back to token actor (should be rejected)
/// - test actor transfers to self (zero amount)
/// - test actor transfers to self (non-zero amount)
/// - test actor transfers to self and rejects
#[test]
fn frc46_single_actor_tests() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let operator: [Account; 1] = tester.create_accounts().unwrap();

    let initial_token_state = TokenState::new(&blockstore).unwrap();

    // install actors required for our test: a token actor and one instance of the test actor
    let token_actor =
        tester.install_actor_with_state(BASIC_TOKEN_ACTOR_WASM, 10000, initial_token_state);
    let test_actor = tester.install_actor_stateless(TEST_ACTOR_WASM, 10010);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct actors
    let ret_val = tester.call_method(operator[0].1, token_actor, method_hash!("Constructor"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success());
    let ret_val = tester.call_method(operator[0].1, test_actor, method_hash!("Constructor"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success());

    // TEST: mint to test actor who rejects hook
    let ret_val = tester.mint_tokens(
        operator[0].1,
        token_actor,
        test_actor,
        TokenAmount::from_atto(100),
        action(TestAction::Reject),
    );
    assert!(!ret_val.msg_receipt.exit_code.is_success());

    // check balance of test actor, should be zero
    let balance = tester.get_balance(operator[0].1, token_actor, test_actor);
    assert_eq!(balance, TokenAmount::from_atto(0));

    // TEST: mint to self (token actor), should be rejected
    let ret_val = tester.mint_tokens(
        operator[0].1,
        token_actor,
        token_actor,
        TokenAmount::from_atto(100),
        action(TestAction::Reject),
    );
    // should fail because the token actor has no receiver hook
    assert!(!ret_val.msg_receipt.exit_code.is_success());

    // TEST: mint to test actor, hook burns tokens immediately
    let ret_val = tester.mint_tokens(
        operator[0].1,
        token_actor,
        test_actor,
        TokenAmount::from_atto(100),
        action(TestAction::Burn),
    );
    let mint_result: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
    // tokens were burned so supply reduces back to zero
    assert_eq!(mint_result.supply, TokenAmount::from_atto(0));

    // check balance of test actor, should also be zero
    let balance = tester.get_balance(operator[0].1, token_actor, test_actor);
    assert_eq!(balance, TokenAmount::from_atto(0));

    // TEST: test actor transfers to self (zero amount)
    let test_action = ActionParams {
        token_address: token_actor,
        action: TestAction::Transfer(test_actor, action(TestAction::Accept)),
    };
    let params = RawBytes::serialize(test_action).unwrap();
    let ret_val =
        tester.call_method(operator[0].1, test_actor, method_hash!("Action"), Some(params));
    assert!(ret_val.msg_receipt.exit_code.is_success());

    // balance should remain zero
    let balance = tester.get_balance(operator[0].1, token_actor, test_actor);
    assert_eq!(balance, TokenAmount::from_atto(0));

    // SETUP: we need a balance on the test actor for the next few tests
    let ret_val = tester.mint_tokens(
        operator[0].1,
        token_actor,
        test_actor,
        TokenAmount::from_atto(100),
        action(TestAction::Accept),
    );
    let mint_result: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
    assert_eq!(mint_result.supply, TokenAmount::from_atto(100));
    let balance = tester.get_balance(operator[0].1, token_actor, test_actor);
    assert_eq!(balance, TokenAmount::from_atto(100));

    // TEST: test actor transfers back to token actor (rejected, token actor has no hook)
    let test_action = ActionParams {
        token_address: token_actor,
        action: TestAction::Transfer(token_actor, RawBytes::default()),
    };
    let params = RawBytes::serialize(test_action).unwrap();
    let ret_val =
        tester.call_method(operator[0].1, test_actor, method_hash!("Action"), Some(params));
    assert!(!ret_val.msg_receipt.exit_code.is_success());
    // check that our test actor balance hasn't changed
    let balance = tester.get_balance(operator[0].1, token_actor, test_actor);
    assert_eq!(balance, TokenAmount::from_atto(100));

    // TEST: test actor transfers to self (non-zero amount)
    let test_action = ActionParams {
        token_address: token_actor,
        action: TestAction::Transfer(test_actor, action(TestAction::Accept)),
    };
    let params = RawBytes::serialize(test_action).unwrap();
    let ret_val =
        tester.call_method(operator[0].1, test_actor, method_hash!("Action"), Some(params));
    assert!(ret_val.msg_receipt.exit_code.is_success());
    // check that our test actor balance hasn't changed
    let balance = tester.get_balance(operator[0].1, token_actor, test_actor);
    assert_eq!(balance, TokenAmount::from_atto(100));
}
