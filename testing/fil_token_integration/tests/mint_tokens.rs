use std::env;

use cid::Cid;
use fil_fungible_token::token::{
    state::TokenState,
    types::{MintParams, MintReturn},
};
use fvm::executor::{ApplyKind, Executor};
use fvm_dispatch::method_hash;
use fvm_integration_tests::tester::{Account, Tester};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser::BigIntDe;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use fvm_shared::state::StateTreeVersion;
use fvm_shared::version::NetworkVersion;

const BASIC_TOKEN_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_token_actor/basic_token_actor.compact.wasm";
const BASIC_RECEIVER_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_receiving_actor/basic_receiving_actor.compact.wasm";

#[test]
fn mint_tokens() {
    let blockstore = MemoryBlockstore::default();
    let mut tester =
        Tester::new(NetworkVersion::V15, StateTreeVersion::V4, blockstore.clone()).unwrap();

    let minter: [Account; 1] = tester.create_accounts().unwrap();

    // Get wasm bin
    let wasm_path =
        env::current_dir().unwrap().join(BASIC_TOKEN_ACTOR_WASM).canonicalize().unwrap();
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
    tester.instantiate_machine().unwrap();

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
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Constructor"), None);
    println!("token actor constructor return data: {:#?}", &ret_val);

    let ret_val = call_method(minter[0].1, receive_address, method_hash!("Constructor"), None);
    println!("receiving actor constructor return data: {:#?}", &ret_val);

    // Mint some tokens
    let mint_params =
        MintParams { initial_owner: receive_address.clone(), amount: TokenAmount::from(100) };
    let params = RawBytes::serialize(mint_params).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(params));
    println!("mint return data {:#?}", &ret_val);
    let return_data = ret_val.msg_receipt.return_data;
    let mint_result: MintReturn = return_data.deserialize().unwrap();
    println!(
        "minted {:?} with total supply of {:?}",
        &mint_result.newly_minted, &mint_result.total_supply
    );

    // Check balance
    //let params = RawBytes::serialize(minter[0].1).unwrap();
    let params = RawBytes::serialize(receive_address).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("BalanceOf"), Some(params));
    println!("balance return data {:#?}", &ret_val);

    let return_data = ret_val.msg_receipt.return_data;
    let balance: BigIntDe = return_data.deserialize().unwrap();
    println!("balance: {:?}", balance.0);
}
