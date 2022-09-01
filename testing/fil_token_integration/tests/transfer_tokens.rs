use std::env;

use basic_token_actor::MintParams;
use cid::Cid;
use fil_fungible_token::token::{state::TokenState, types::MintReturn};
use frc42_dispatch::method_hash;
use fvm::{
    executor::{ApplyKind, ApplyRet, Executor},
    externs::Externs,
};
use fvm_integration_tests::{
    bundle,
    dummy::DummyExterns,
    tester::{Account, Tester},
};
use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
use fvm_ipld_encoding::{
    tuple::{Deserialize_tuple, Serialize_tuple},
    RawBytes,
};
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use fvm_shared::state::StateTreeVersion;
use fvm_shared::version::NetworkVersion;

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

fn load_actor_wasm(path: &str) -> Vec<u8> {
    let wasm_path = env::current_dir().unwrap().join(path).canonicalize().unwrap();

    std::fs::read(wasm_path).expect("unable to read actor file")
}

trait TestHelpers {
    fn call_method(
        &mut self,
        from: Address,
        to: Address,
        method_num: u64,
        params: Option<RawBytes>,
    ) -> ApplyRet;
    fn check_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    ) -> TokenAmount;
}
impl<B: Blockstore, E: Externs> TestHelpers for Tester<B, E> {
    fn call_method(
        &mut self,
        from: Address,
        to: Address,
        method_num: u64,
        params: Option<RawBytes>,
    ) -> ApplyRet {
        static mut SEQUENCE: u64 = 0u64;
        let message = Message {
            from,
            to,
            gas_limit: 99999999,
            method_num,
            sequence: unsafe { SEQUENCE },
            params: if let Some(params) = params { params } else { RawBytes::default() },
            ..Message::default()
        };
        unsafe {
            SEQUENCE += 1;
        }
        self.executor.as_mut().unwrap().execute_message(message, ApplyKind::Explicit, 100).unwrap()
    }

    fn check_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    ) -> TokenAmount {
        let params = RawBytes::serialize(target).unwrap();
        let ret_val =
            self.call_method(operator, token_actor, method_hash!("BalanceOf"), Some(params));
        println!("balance return data {:#?}", &ret_val);
        ret_val.msg_receipt.return_data.deserialize::<TokenAmount>().unwrap()
    }
}

#[test]
fn transfer_tokens() {
    let blockstore = MemoryBlockstore::default();
    let bundle_root = bundle::import_bundle(&blockstore, actors_v10::BUNDLE_CAR).unwrap();
    let mut tester =
        Tester::new(NetworkVersion::V15, StateTreeVersion::V4, bundle_root, blockstore.clone())
            .unwrap();

    let operator: [Account; 1] = tester.create_accounts().unwrap();

    // token actor
    let token_bin = load_actor_wasm(BASIC_TOKEN_ACTOR_WASM);
    // transfer actor
    let transfer_bin = load_actor_wasm(BASIC_TRANSFER_ACTOR_WASM);
    // account actors
    let receiver_bin = load_actor_wasm(BASIC_RECEIVER_ACTOR_WASM);

    let token_state = TokenState::new(&blockstore).unwrap();
    let token_cid = tester.set_state(&token_state).unwrap();

    // transfer actor state
    let transfer_state = TransferActorState { operator_address: None, token_address: None };
    let transfer_cid = tester.set_state(&transfer_state).unwrap();

    let token_address = Address::new_id(10000);
    let transfer_address = Address::new_id(10010);
    let receiver_address = Address::new_id(10020);
    tester.set_actor_from_bin(&token_bin, token_cid, token_address, TokenAmount::zero()).unwrap();
    tester
        .set_actor_from_bin(&transfer_bin, transfer_cid, transfer_address, TokenAmount::zero())
        .unwrap();
    tester
        .set_actor_from_bin(&receiver_bin, Cid::default(), receiver_address, TokenAmount::zero())
        .unwrap();

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct actors
    let ret_val =
        tester.call_method(operator[0].1, token_address, method_hash!("Constructor"), None);
    println!("token actor constructor return data: {:#?}", &ret_val);

    let ret_val =
        tester.call_method(operator[0].1, transfer_address, method_hash!("Constructor"), None);
    println!("transfer actor constructor return data: {:#?}", &ret_val);

    let ret_val =
        tester.call_method(operator[0].1, receiver_address, method_hash!("Constructor"), None);
    println!("receiving actor constructor return data: {:#?}", &ret_val);

    // mint some tokens
    let mint_params =
        MintParams { initial_owner: transfer_address, amount: TokenAmount::from_atto(100) };
    let params = RawBytes::serialize(mint_params).unwrap();
    let ret_val =
        tester.call_method(operator[0].1, token_address, method_hash!("Mint"), Some(params));
    println!("minting return data {:#?}", &ret_val);
    let mint_result: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
    println!("minted - total supply: {:?}", &mint_result.supply);

    // check balance of transfer actor
    let balance = tester.check_balance(operator[0].1, token_address, transfer_address);
    println!("balance held by transfer actor: {:?}", balance);

    // forward from transfer to receiving actor
    let params = RawBytes::serialize(receiver_address).unwrap();
    let ret_val =
        tester.call_method(operator[0].1, transfer_address, method_hash!("Forward"), Some(params));
    println!("forwarding return data {:#?}", &ret_val);

    // check balance of receiver actor
    let balance = tester.check_balance(operator[0].1, token_address, transfer_address);
    println!("balance held by transfer actor: {:?}", balance);

    let balance = tester.check_balance(operator[0].1, token_address, receiver_address);
    println!("balance held by receiver actor: {:?}", balance);
}
