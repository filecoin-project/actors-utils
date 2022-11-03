use frc42_dispatch::method_hash;
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::{
    tuple::{Deserialize_tuple, Serialize_tuple},
    RawBytes,
};
use fvm_shared::{address::Address, econ::TokenAmount, receipt::Receipt};
use serde::{Deserialize, Serialize};

mod common;
use common::{construct_tester, TestHelpers, TokenHelpers};
use factory_token::{token::BasicToken, ConstructorParams};

const FACTORY_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/factory_token/factory_token.compact.wasm";
const TEST_ACTOR_WASM: &str = "../../target/debug/wbuild/test_actor/test_actor.compact.wasm";

// NOTE: things here copied from the test actor, can't include it properly
// because its invoke function will conflict with our token actor one

/// Action to take in receiver hook or Action method
/// This gets serialized and sent along as operator_data
#[derive(Serialize, Deserialize, Debug)]
pub enum TestAction {
    /// Accept the tokens
    Accept,
    /// Reject the tokens (hook aborts)
    Reject,
    /// Transfer to another address (with operator_data that can provide further instructions)
    Transfer(Address, RawBytes),
    /// Burn incoming tokens
    Burn,
}

/// Params for Action method call
/// This gives us a way to supply the token address, since we won't get it as a sender like we do for hook calls
#[derive(Serialize_tuple, Deserialize_tuple, Debug)]
pub struct ActionParams {
    /// Address of the token actor
    pub token_address: Address,
    /// Action to take with our token balance. Only Transfer and Burn actions apply here.
    pub action: TestAction,
}

/// Helper for nesting calls to create action sequences
/// eg. transfer and then the receiver hook rejects:
/// action(TestAction::Transfer(
///         some_address,
///         action(TestAction::Reject),
///     ),
/// )
pub fn action(action: TestAction) -> RawBytes {
    RawBytes::serialize(action).unwrap()
}

fn action_params(token_address: Address, action: TestAction) -> RawBytes {
    RawBytes::serialize(ActionParams { token_address, action }).unwrap()
}

#[test]
fn factory_token() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let operator: [Account; 1] = tester.create_accounts().unwrap();

    let initial_token_state = BasicToken::new(&blockstore, String::new(), String::new(), 1, None);

    // install actors required for our test: a token actor and one instance of the test actor
    let token_actor =
        tester.install_actor_with_state(FACTORY_TOKEN_ACTOR_WASM, 10000, initial_token_state);

    // create a couple test actors
    let alice = tester.install_actor_stateless(TEST_ACTOR_WASM, 10010);
    let bob = tester.install_actor_stateless(TEST_ACTOR_WASM, 10020);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct token actor for a test token
    {
        let params = ConstructorParams {
            name: "Test Token".into(),
            symbol: "TEST".into(),
            granularity: 1,
            minter: Some(operator[0].1),
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

    // construct test actors
    {
        for actor in [alice, bob] {
            let ret_val =
                tester.call_method(operator[0].1, actor, method_hash!("Constructor"), None);
            assert!(
                ret_val.msg_receipt.exit_code.is_success(),
                "actor constructor returned {:#?}",
                ret_val
            );
        }
    }

    // mint some tokens to alice, who accepts
    {
        let ret_val = tester.mint_tokens(
            operator[0].1,
            token_actor,
            alice,
            TokenAmount::from_atto(100),
            action(TestAction::Accept),
        );
        assert!(ret_val.msg_receipt.exit_code.is_success(), "minting returned {:#?}", ret_val);

        // check balance of test actor, should be zero
        tester.assert_token_balance(operator[0].1, token_actor, alice, TokenAmount::from_atto(100));
    }

    // transfer those tokens from alice to bob, who accepts
    {
        let params =
            action_params(token_actor, TestAction::Transfer(bob, action(TestAction::Accept)));
        let ret_val =
            tester.call_method_ok(operator[0].1, alice, method_hash!("Action"), Some(params));
        // check the receipt we got in return data
        let receipt: Receipt = ret_val.msg_receipt.return_data.deserialize().unwrap();
        assert!(receipt.exit_code.is_success());
        // check balances
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
        tester.assert_token_balance(operator[0].1, token_actor, bob, TokenAmount::from_atto(100));
    }

    // mint some more to alice, who burns them upon receipt
    {
        let ret_val = tester.mint_tokens(
            operator[0].1,
            token_actor,
            alice,
            TokenAmount::from_atto(100),
            action(TestAction::Burn),
        );
        assert!(
            ret_val.msg_receipt.exit_code.is_success(),
            "second minting returned {:#?}",
            ret_val
        );

        // check balance of test actor, should be zero
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
    }

    // disable minting and attempt to mint afterwards
    {
        let ret_val =
            tester.call_method(operator[0].1, token_actor, method_hash!("DisableMint"), None);
        assert!(
            ret_val.msg_receipt.exit_code.is_success(),
            "actor constructor returned {:#?}",
            ret_val
        );

        // try minting some tokens, which should fail
        let ret_val = tester.mint_tokens(
            operator[0].1,
            token_actor,
            alice,
            TokenAmount::from_atto(100),
            action(TestAction::Accept),
        );
        assert!(
            !ret_val.msg_receipt.exit_code.is_success(),
            "third minting returned {:#?}",
            ret_val
        );

        // check balance of test actor, should be zero
        tester.assert_token_balance_zero(operator[0].1, token_actor, alice);
    }
}
