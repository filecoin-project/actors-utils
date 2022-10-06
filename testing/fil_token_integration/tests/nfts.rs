use std::env;

use cid::Cid;
use frc42_dispatch::method_hash;
use frcxx_nft::state::TokenID;
use fvm::executor::{ApplyKind, Executor};
use fvm_integration_tests::bundle;
use fvm_integration_tests::dummy::DummyExterns;
use fvm_integration_tests::tester::{Account, Tester};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use fvm_shared::state::StateTreeVersion;
use fvm_shared::version::NetworkVersion;
use serde_tuple::{Deserialize_tuple, Serialize_tuple};

/// Copied from basic_nft_actor
#[derive(Serialize_tuple, Deserialize_tuple, Debug, Clone)]
pub struct MintParams {
    metadata_id: Cid,
}

const BASIC_NFT_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_nft_actor/basic_nft_actor.compact.wasm";
const BASIC_RECEIVER_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_receiving_actor/basic_receiving_actor.compact.wasm";

#[test]
fn it_mints_nfts() {
    let blockstore = MemoryBlockstore::default();
    let bundle_root = bundle::import_bundle(&blockstore, actors_v9::BUNDLE_CAR).unwrap();
    let mut tester =
        Tester::new(NetworkVersion::V15, StateTreeVersion::V4, bundle_root, blockstore).unwrap();

    let minter: [Account; 1] = tester.create_accounts().unwrap();

    // Get wasm bin
    let wasm_path = env::current_dir().unwrap().join(BASIC_NFT_ACTOR_WASM).canonicalize().unwrap();
    let wasm_bin = std::fs::read(wasm_path).expect("Unable to read token actor file");
    let rcvr_path =
        env::current_dir().unwrap().join(BASIC_RECEIVER_ACTOR_WASM).canonicalize().unwrap();
    let rcvr_bin = std::fs::read(rcvr_path).expect("Unable to read receiver actor file");

    let actor_address = Address::new_id(10000);
    let receive_address = Address::new_id(10010);
    tester
        .set_actor_from_bin(&wasm_bin, Cid::default(), actor_address, TokenAmount::zero())
        .unwrap();
    tester
        .set_actor_from_bin(&rcvr_bin, Cid::default(), receive_address, TokenAmount::zero())
        .unwrap();

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

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
    call_method(minter[0].1, actor_address, method_hash!("Constructor"), None);

    // TODO: assert that minting calls out to hook

    // Mint a single token
    let mint_params = MintParams { metadata_id: Cid::default() };
    let mint_params = RawBytes::serialize(&mint_params).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(mint_params));
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let mint_result = ret_val.msg_receipt.return_data.deserialize::<TokenID>().unwrap();
    assert_eq!(mint_result, 0);

    // TODO: check metadata, ownership data etc. is updated
    // Check the total supply increased
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 1);

    let mint_params = MintParams { metadata_id: Cid::default() };
    let mint_params = RawBytes::serialize(&mint_params).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(mint_params));
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let mint_result = ret_val.msg_receipt.return_data.deserialize::<TokenID>().unwrap();
    assert_eq!(mint_result, 1);

    // TODO: check metadata, ownership data etc. is updated
    // Check the total supply increased
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 2);
}

#[test]
fn it_burns_tokens() {
    let blockstore = MemoryBlockstore::default();
    let bundle_root = bundle::import_bundle(&blockstore, actors_v9::BUNDLE_CAR).unwrap();
    let mut tester =
        Tester::new(NetworkVersion::V15, StateTreeVersion::V4, bundle_root, blockstore).unwrap();

    let minter: [Account; 1] = tester.create_accounts().unwrap();

    // Get wasm bin
    let wasm_path = env::current_dir().unwrap().join(BASIC_NFT_ACTOR_WASM).canonicalize().unwrap();
    let wasm_bin = std::fs::read(wasm_path).expect("Unable to read token actor file");
    let rcvr_path =
        env::current_dir().unwrap().join(BASIC_RECEIVER_ACTOR_WASM).canonicalize().unwrap();
    let rcvr_bin = std::fs::read(rcvr_path).expect("Unable to read receiver actor file");

    let actor_address = Address::new_id(10000);
    let receive_address = Address::new_id(10010);
    tester
        .set_actor_from_bin(&wasm_bin, Cid::default(), actor_address, TokenAmount::zero())
        .unwrap();
    tester
        .set_actor_from_bin(&rcvr_bin, Cid::default(), receive_address, TokenAmount::zero())
        .unwrap();

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

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
    call_method(minter[0].1, actor_address, method_hash!("Constructor"), None);

    // Mint a single token
    let mint_params = MintParams { metadata_id: Cid::default() };
    let mint_params = RawBytes::serialize(&mint_params).unwrap();
    call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(mint_params));

    // Attempt to burn a non-existent token
    let burn_params: TokenID = 100;
    let burn_params = RawBytes::serialize(&burn_params).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Burn"), Some(burn_params));
    // call should fail
    assert!(!ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);

    // Check the total supply didn't change
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 1);

    // Burn the correct token
    let burn_params: TokenID = 0;
    let burn_params = RawBytes::serialize(&burn_params).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Burn"), Some(burn_params));
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);

    // TODO: check metadata, ownership data etc. is updated
    // Check the total supply decreased
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 0);

    // Cannot burn the same token again
    // Burn the correct token
    let burn_params: TokenID = 0;
    let burn_params = RawBytes::serialize(&burn_params).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Burn"), Some(burn_params));
    // call should fail
    assert!(!ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);

    // Minting the next token uses the next ID
    let mint_params = MintParams { metadata_id: Cid::default() };
    let mint_params = RawBytes::serialize(&mint_params).unwrap();
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(mint_params));
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let mint_result = ret_val.msg_receipt.return_data.deserialize::<TokenID>().unwrap();
    assert_eq!(mint_result, 1);

    // Check the total supply increased
    let ret_val = call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 1);
}
