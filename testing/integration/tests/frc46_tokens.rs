use cid::Cid;
use frc42_dispatch::method_hash;
use frc46_token::token::{state::TokenState, types::MintReturn};
use fvm::executor::{ApplyKind, Executor};
use fvm_integration_tests::dummy::DummyExterns;
use fvm_integration_tests::tester::Account;
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::tuple::*;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use helix_test_actors::BASIC_RECEIVING_ACTOR_BINARY;
use helix_test_actors::BASIC_TOKEN_ACTOR_BINARY;

mod common;
use common::construct_tester;

// Duplicated type from basic_token_actor
#[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
pub struct MintParams {
    pub initial_owner: Address,
    pub amount: TokenAmount,
    pub operator_data: RawBytes,
}

#[test]
fn it_mints_tokens() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let minter: [Account; 1] = tester.create_accounts().unwrap();

    // Set actor state
    let actor_state = TokenState::new(&blockstore).unwrap(); // TODO: this should probably not be exported from the package
    let state_cid = tester.set_state(&actor_state).unwrap();

    let actor_address = Address::new_id(10000);
    let receive_address = Address::new_id(10010);
    tester
        .set_actor_from_bin(BASIC_TOKEN_ACTOR_BINARY, state_cid, actor_address, TokenAmount::zero())
        .unwrap();
    tester
        .set_actor_from_bin(
            BASIC_RECEIVING_ACTOR_BINARY,
            Cid::default(),
            receive_address,
            TokenAmount::zero(),
        )
        .unwrap();

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // Helper to simplify sending messages
    let mut sequence = 0u64;
    let mut call_method = |from, to, method_num, params: Option<RawBytes>| {
        let message = Message {
            from,
            to,
            gas_limit: 99999999,
            method_num,
            sequence,
            params: params.unwrap_or_default(),
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
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Constructor"), None);
    println!("token actor constructor return data: {:#?}", &ret_val);

    let ret_val = call_method(minter[0].1, receive_address, method_hash!("Constructor"), None);
    println!("receiving actor constructor return data: {:#?}", &ret_val);

    // Mint some tokens
    let mint_params = MintParams {
        initial_owner: receive_address,
        amount: TokenAmount::from_atto(100),
        operator_data: RawBytes::default(),
    };
    let params = RawBytes::serialize(mint_params).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(params));
    println!("mint return data {:#?}", &ret_val);
    let return_data = ret_val.msg_receipt.return_data;
    if return_data.is_empty() {
        println!("return data was empty");
    } else {
        let mint_result: MintReturn = return_data.deserialize().unwrap();
        println!("new total supply: {:?}", &mint_result.supply);
    }

    // Check balance
    let params = RawBytes::serialize(receive_address).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("BalanceOf"), Some(params));
    println!("balance return data {:#?}", &ret_val);

    let return_data = ret_val.msg_receipt.return_data;
    let balance: TokenAmount = return_data.deserialize().unwrap();
    println!("balance: {balance:?}");
}
