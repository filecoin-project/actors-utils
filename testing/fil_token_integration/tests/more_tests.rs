use basic_token_actor::MintParams;
use frc42_dispatch::method_hash;
use frc46_token::token::{state::TokenState, types::MintReturn};
use fvm_integration_tests::{
    bundle,
    dummy::DummyExterns,
    tester::{Account, Tester},
};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::{
    tuple::{Deserialize_tuple, Serialize_tuple},
    RawBytes,
};
use fvm_shared::{
    address::Address, econ::TokenAmount, state::StateTreeVersion, version::NetworkVersion,
};
use serde::{Deserialize, Serialize};

mod common;
use common::TestHelpers;

const BASIC_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_token_actor/basic_token_actor.compact.wasm";
const TEST_ACTOR_WASM: &str = "../../target/debug/wbuild/test_actor/test_actor.compact.wasm";

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
    token_address: Address,
    /// Action to take with our token balance. Only Transfer and Burn actions apply here.
    action: TestAction,
}

/// Helper for nesting calls to create action sequences
/// eg. transfer and then the receiver hook rejects:
/// action(TestAction::Transfer(
///         some_address,
///         action(TestAction::Reject),
///     ),
/// )
fn action(action: TestAction) -> RawBytes {
    RawBytes::serialize(action).unwrap()
}

#[test]
fn more_tests() {
    let blockstore = MemoryBlockstore::default();
    let bundle_root = bundle::import_bundle(&blockstore, actors_v10::BUNDLE_CAR).unwrap();
    let mut tester =
        Tester::new(NetworkVersion::V15, StateTreeVersion::V4, bundle_root, blockstore.clone())
            .unwrap();

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

    // mint some tokens
    // TODO: add operator data to MintParams
    let mint_params = MintParams { initial_owner: test_actor, amount: TokenAmount::from_atto(100) };
    let params = RawBytes::serialize(mint_params).unwrap();
    let ret_val =
        tester.call_method(operator[0].1, token_actor, method_hash!("Mint"), Some(params));
    println!("minting return data {:#?}", &ret_val);
    let mint_result: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
    println!("minted - total supply: {:?}", &mint_result.supply);

    // check balance of transfer actor
    let balance = tester.get_balance(operator[0].1, token_actor, test_actor);
    println!("balance held by transfer actor: {:?}", balance);
}
