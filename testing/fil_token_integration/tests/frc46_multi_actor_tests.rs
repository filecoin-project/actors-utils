use frc42_dispatch::method_hash;
use frc46_token::token::{state::TokenState, types::TransferReturn};
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, bigint::Zero, econ::TokenAmount, receipt::Receipt};

mod common;
use common::frc46_token_helpers::TokenHelper;
use common::{construct_tester, TestHelpers};
use frc46_test_actor::{action, ActionParams, TestAction};
use token_impl::{ConstructorParams, FactoryToken};

const FACTORY_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/frc46_factory_token/frc46_factory_token.compact.wasm";
const TEST_ACTOR_WASM: &str =
    "../../target/debug/wbuild/frc46_test_actor/frc46_test_actor.compact.wasm";

fn action_params(token_address: Address, action: TestAction) -> RawBytes {
    RawBytes::serialize(ActionParams { token_address, action }).unwrap()
}

#[test]
fn frc46_multi_actor_tests() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let operator: [Account; 1] = tester.create_accounts().unwrap();

    let initial_token_state = FactoryToken {
        token: TokenState::new(&blockstore).unwrap(),
        name: String::new(),
        symbol: String::new(),
        granularity: 1,
        minter: None,
    };

    let token_actor =
        tester.install_actor_with_state(FACTORY_TOKEN_ACTOR_WASM, 10000, initial_token_state);
    // we'll use up to four actors for some of these tests, though most use only two
    let alice = tester.install_actor_stateless(TEST_ACTOR_WASM, 10010);
    let bob = tester.install_actor_stateless(TEST_ACTOR_WASM, 10011);
    let carol = tester.install_actor_stateless(TEST_ACTOR_WASM, 10012);
    let dave = tester.install_actor_stateless(TEST_ACTOR_WASM, 10013);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct TEST token actor
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
            "token constructor returned {:#?}",
            ret_val
        );
    }

    // construct actors
    for actor in [alice, bob, carol, dave] {
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
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
        tester.assert_token_balance_zero(operator[0].1, token_actor, bob);
    }
    {
        // this time tell bob to accept it
        let params =
            action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Accept)));
        tester.call_method_ok(operator[0].1, alice, method_hash!("Action"), Some(params));

        // balance should remain zero
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
        tester.assert_token_balance_zero(operator[0].1, token_actor, bob);
    }
    // TEST: alice sends bob a transfer of a non-zero amounnt. As before, we'll reject it the first time then accept
    {
        // mint some tokens to alice first
        let _ = tester.mint_tokens_ok(
            operator[0].1,
            token_actor,
            alice,
            TokenAmount::from_atto(100),
            action(TestAction::Accept),
        );
        tester.assert_token_balance(operator[0].1, token_actor, alice, TokenAmount::from_atto(100));
        // now send to bob, who will reject them
        let params =
            action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Reject)));
        let ret_val =
            tester.call_method_ok(operator[0].1, alice, method_hash!("Action"), Some(params));
        // check the receipt we got in return data
        let receipt = ret_val.msg_receipt.return_data.deserialize::<Receipt>().unwrap();
        assert!(!receipt.exit_code.is_success());
        // alice should keep the tokens, while bob has nothing
        tester.assert_token_balance(operator[0].1, token_actor, alice, TokenAmount::from_atto(100));
        tester.assert_token_balance_zero(operator[0].1, token_actor, bob);
    }
    {
        // transfer to bob who will accept it this time
        let params =
            action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Accept)));
        tester.call_method_ok(operator[0].1, alice, method_hash!("Action"), Some(params));
        // check balances
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
        tester.assert_token_balance(operator[0].1, token_actor, bob, TokenAmount::from_atto(100));
    }

    // TEST: mint to alice who transfers to bob inside receiver hook, bob accepts
    {
        tester.mint_tokens_ok(
            operator[0].1,
            token_actor,
            alice,
            TokenAmount::from_atto(100),
            action(TestAction::Transfer(bob, action(TestAction::Accept))),
        );
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
        tester.assert_token_balance(operator[0].1, token_actor, bob, TokenAmount::from_atto(200));
    }

    // TEST: mint to alice who transfers to bob inside receiver hook, bob rejects
    {
        tester.mint_tokens_ok(
            operator[0].1,
            token_actor,
            alice,
            TokenAmount::from_atto(100),
            action(TestAction::Transfer(bob, action(TestAction::Reject))),
        );
        // mint succeeds but the transfer inside the receiver hook would have failed
        // alice should keep tokens in this case
        tester.assert_token_balance(operator[0].1, token_actor, alice, TokenAmount::from_atto(100));
        // bob's balance should remain unchanged
        tester.assert_token_balance(operator[0].1, token_actor, bob, TokenAmount::from_atto(200));
    }

    // TEST: alice transfers to bob, bob transfers to carol (from hook), carol accepts
    {
        let params = action_params(
            token_actor,
            TestAction::Transfer(
                bob,
                action(TestAction::Transfer(carol, action(TestAction::Accept))),
            ),
        );
        let ret_val =
            tester.call_method_ok(operator[0].1, alice, method_hash!("Action"), Some(params));
        // check the receipt we got in return data
        let receipt: Receipt = ret_val.msg_receipt.return_data.deserialize().unwrap();
        assert!(receipt.exit_code.is_success());
        // check the transfer result (from alice to bob)
        let bob_transfer: TransferReturn = receipt.return_data.deserialize().unwrap();
        assert_eq!(bob_transfer.from_balance, TokenAmount::zero());
        assert_eq!(bob_transfer.to_balance, TokenAmount::from_atto(200));
        // now extract the bob->carol receipt and transfer data contained within
        let bob_receipt: Receipt = bob_transfer.recipient_data.deserialize().unwrap();
        let carol_transfer: TransferReturn = bob_receipt.return_data.deserialize().unwrap();
        assert_eq!(carol_transfer.from_balance, TokenAmount::from_atto(200));
        assert_eq!(carol_transfer.to_balance, TokenAmount::from_atto(100));

        // check balances - alice should be empty, bob should keep 200, carol sitting on 100
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
        tester.assert_token_balance(operator[0].1, token_actor, bob, TokenAmount::from_atto(200));
        tester.assert_token_balance(operator[0].1, token_actor, carol, TokenAmount::from_atto(100));
    }

    // TEST: alice transfers to bob, bob transfers to carol (from hook), carol burns (from hook)
    {
        // mint some more to alice first
        tester.mint_tokens_ok(
            operator[0].1,
            token_actor,
            alice,
            TokenAmount::from_atto(100),
            action(TestAction::Accept),
        );
        // now transfer alice->bob->carol and have carol burn the incoming balance
        let params = action_params(
            token_actor,
            TestAction::Transfer(
                bob,
                action(TestAction::Transfer(carol, action(TestAction::Burn))),
            ),
        );
        let ret_val =
            tester.call_method_ok(operator[0].1, alice, method_hash!("Action"), Some(params));

        // check the receipt we got in return data
        let receipt: Receipt = ret_val.msg_receipt.return_data.deserialize().unwrap();
        assert!(receipt.exit_code.is_success());
        // check the transfer result (from alice to bob)
        let bob_transfer: TransferReturn = receipt.return_data.deserialize().unwrap();
        assert_eq!(bob_transfer.from_balance, TokenAmount::zero());
        assert_eq!(bob_transfer.to_balance, TokenAmount::from_atto(200));
        // now extract the bob->carol receipt and transfer data contained within
        let bob_receipt: Receipt = bob_transfer.recipient_data.deserialize().unwrap();
        let carol_transfer: TransferReturn = bob_receipt.return_data.deserialize().unwrap();
        assert_eq!(carol_transfer.from_balance, TokenAmount::from_atto(200));
        assert_eq!(carol_transfer.to_balance, TokenAmount::from_atto(100));

        // check balances - alice should be empty, bob should keep 200, carol sitting on 100
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
        tester.assert_token_balance(operator[0].1, token_actor, bob, TokenAmount::from_atto(200));
        tester.assert_token_balance(operator[0].1, token_actor, carol, TokenAmount::from_atto(100));
    }
}
