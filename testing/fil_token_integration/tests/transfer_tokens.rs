use frc42_dispatch::method_hash;
use frc46_token::token::{state::TokenState, types::MintReturn};
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::{
    tuple::{Deserialize_tuple, Serialize_tuple},
    RawBytes,
};
use fvm_shared::address::Address;
use fvm_shared::econ::TokenAmount;

mod common;
use common::{construct_tester, TestHelpers, TokenHelpers};

const BASIC_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_token_actor/basic_token_actor.compact.wasm";
const BASIC_TRANSFER_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_transfer_actor/basic_transfer_actor.compact.wasm";
const BASIC_RECEIVER_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_receiving_actor/basic_receiving_actor.compact.wasm";

#[derive(Serialize_tuple, Deserialize_tuple)]
struct TransferActorState {
    operator_address: Option<Address>,
    token_address: Option<Address>,
}

#[test]
fn transfer_tokens() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let operator: [Account; 1] = tester.create_accounts().unwrap();

    let token_state = TokenState::new(&blockstore).unwrap();
    let transfer_state = TransferActorState { operator_address: None, token_address: None };

    let token_address = tester.install_actor_with_state(BASIC_TOKEN_ACTOR_WASM, 10000, token_state);
    let transfer_address =
        tester.install_actor_with_state(BASIC_TRANSFER_ACTOR_WASM, 10010, transfer_state);
    let receiver_address = tester.install_actor_stateless(BASIC_RECEIVER_ACTOR_WASM, 10020);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct actors
    for actor in [token_address, transfer_address, receiver_address] {
        let ret_val = tester.call_method(operator[0].1, actor, method_hash!("Constructor"), None);
        assert!(ret_val.msg_receipt.exit_code.is_success());
    }

    // mint some tokens
    let ret_val = tester.mint_tokens(
        operator[0].1,
        token_address,
        transfer_address,
        TokenAmount::from_atto(100),
        RawBytes::default(),
    );
    println!("minting return data {:#?}", &ret_val);
    let mint_result: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
    println!("minted - total supply: {:?}", &mint_result.supply);
    assert_eq!(mint_result.supply, TokenAmount::from_atto(100));

    // check balance of transfer actor
    let balance = tester.get_balance(operator[0].1, token_address, transfer_address);
    println!("balance held by transfer actor: {:?}", balance);
    assert_eq!(balance, TokenAmount::from_atto(100));

    // forward from transfer to receiving actor
    let params = RawBytes::serialize(receiver_address).unwrap();
    let ret_val =
        tester.call_method(operator[0].1, transfer_address, method_hash!("Forward"), Some(params));
    println!("forwarding return data {:#?}", &ret_val);

    // check balance of receiver actor
    let balance = tester.get_balance(operator[0].1, token_address, transfer_address);
    println!("balance held by transfer actor: {:?}", balance);
    assert_eq!(balance, TokenAmount::from_atto(0));

    let balance = tester.get_balance(operator[0].1, token_address, receiver_address);
    println!("balance held by receiver actor: {:?}", balance);
    assert_eq!(balance, TokenAmount::from_atto(100));
}
