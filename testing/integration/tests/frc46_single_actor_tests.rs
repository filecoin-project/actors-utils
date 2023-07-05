use frc42_dispatch::method_hash;
use frc46_token::token::types::MintReturn;
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{econ::TokenAmount, receipt::Receipt};

mod common;
use common::frc46_token_helpers::TokenHelper;
use common::{construct_tester, TestHelpers};
use frc46_test_actor::{action, ActionParams, TestAction};
use token_impl::ConstructorParams;

const FACTORY_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/frc46_factory_token/frc46_factory_token.compact.wasm";
const TEST_ACTOR_WASM: &str =
    "../../target/debug/wbuild/frc46_test_actor/frc46_test_actor.compact.wasm";

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

    // install actors required for our test: a token actor and one instance of the test actor
    let token_actor = tester.install_actor_stateless(FACTORY_TOKEN_ACTOR_WASM, 10000);
    let frc46_test_actor = tester.install_actor_stateless(TEST_ACTOR_WASM, 10010);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct our TEST token
    {
        let params = ConstructorParams {
            name: "Test Token".into(),
            symbol: "TEST".into(),
            granularity: 1,
            minter: operator[0].1,
        };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method(
            operator[0].1,
            token_actor,
            method_hash!("Constructor"),
            Some(params),
        );
        assert!(
            ret_val.msg_receipt.exit_code.is_success(),
            "token constructor returned {ret_val:#?}",
        );
    }

    // construct actor
    {
        let ret_val =
            tester.call_method(operator[0].1, frc46_test_actor, method_hash!("Constructor"), None);
        assert!(ret_val.msg_receipt.exit_code.is_success());
    }

    // TEST: mint to test actor who rejects hook
    {
        let ret_val = tester.mint_tokens(
            operator[0].1,
            token_actor,
            frc46_test_actor,
            TokenAmount::from_atto(100),
            action(TestAction::Reject),
        );
        assert!(!ret_val.msg_receipt.exit_code.is_success());

        // check balance of test actor, should be zero
        tester.assert_token_balance_zero(operator[0].1, token_actor, frc46_test_actor);
    }

    // TEST: mint to self (token actor), should be rejected
    {
        let ret_val = tester.mint_tokens(
            operator[0].1,
            token_actor,
            token_actor,
            TokenAmount::from_atto(100),
            action(TestAction::Reject),
        );
        // should fail because the token actor has no receiver hook
        assert!(!ret_val.msg_receipt.exit_code.is_success());
    }

    // TEST: mint to test actor, hook burns tokens immediately
    {
        let ret_val = tester.mint_tokens_ok(
            operator[0].1,
            token_actor,
            frc46_test_actor,
            TokenAmount::from_atto(100),
            action(TestAction::Burn),
        );
        let mint_result: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
        // tokens were burned so supply reduces back to zero
        assert_eq!(mint_result.supply, TokenAmount::from_atto(0));

        // check balance of test actor, should also be zero
        tester.assert_token_balance_zero(operator[0].1, token_actor, frc46_test_actor);
    }

    // TEST: test actor transfers to self (zero amount)
    {
        let test_action = ActionParams {
            token_address: token_actor,
            action: TestAction::Transfer(frc46_test_actor, action(TestAction::Accept)),
        };
        let params = RawBytes::serialize(test_action).unwrap();
        tester.call_method_ok(
            operator[0].1,
            frc46_test_actor,
            method_hash!("Action"),
            Some(params),
        );

        // balance should remain zero
        tester.assert_token_balance_zero(operator[0].1, token_actor, frc46_test_actor);
    }

    // SETUP: we need a balance on the test actor for the next few tests
    {
        let ret_val = tester.mint_tokens_ok(
            operator[0].1,
            token_actor,
            frc46_test_actor,
            TokenAmount::from_atto(100),
            action(TestAction::Accept),
        );
        let mint_result: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
        assert_eq!(mint_result.supply, TokenAmount::from_atto(100));
        tester.assert_token_balance(
            operator[0].1,
            token_actor,
            frc46_test_actor,
            TokenAmount::from_atto(100),
        );
    }

    // TEST: test actor transfers back to token actor (rejected, token actor has no hook)
    {
        let test_action = ActionParams {
            token_address: token_actor,
            action: TestAction::Transfer(token_actor, RawBytes::default()),
        };
        let params = RawBytes::serialize(test_action).unwrap();
        let ret_val = tester.call_method_ok(
            operator[0].1,
            frc46_test_actor,
            method_hash!("Action"),
            Some(params),
        );
        // action call should succeed, we'll dig into the return data to see the transfer call failure

        // return data is the Receipt from calling Transfer, which should show failure
        let receipt: Receipt = ret_val.msg_receipt.return_data.deserialize().unwrap();
        assert!(!receipt.exit_code.is_success());
        // check that our test actor balance hasn't changed
        tester.assert_token_balance(
            operator[0].1,
            token_actor,
            frc46_test_actor,
            TokenAmount::from_atto(100),
        );
    }

    // TEST: test actor transfers to self (non-zero amount)
    {
        let test_action = ActionParams {
            token_address: token_actor,
            action: TestAction::Transfer(frc46_test_actor, action(TestAction::Accept)),
        };
        let params = RawBytes::serialize(test_action).unwrap();
        tester.call_method_ok(
            operator[0].1,
            frc46_test_actor,
            method_hash!("Action"),
            Some(params),
        );

        // check that our test actor balance hasn't changed
        tester.assert_token_balance(
            operator[0].1,
            token_actor,
            frc46_test_actor,
            TokenAmount::from_atto(100),
        );
    }

    // TEST: test actor transfers to self (non-zero amount) and rejects
    {
        let test_action = ActionParams {
            token_address: token_actor,
            action: TestAction::Transfer(frc46_test_actor, action(TestAction::Reject)),
        };
        let params = RawBytes::serialize(test_action).unwrap();
        tester.call_method_ok(
            operator[0].1,
            frc46_test_actor,
            method_hash!("Action"),
            Some(params),
        );

        // check that our test actor balance hasn't changed
        tester.assert_token_balance(
            operator[0].1,
            token_actor,
            frc46_test_actor,
            TokenAmount::from_atto(100),
        );
    }
}
