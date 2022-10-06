use cid::Cid;
use frc42_dispatch::method_hash;
use frcxx_nft::state::TokenID;
use fvm_integration_tests::dummy::DummyExterns;
use fvm_integration_tests::tester::Account;
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use serde_tuple::{Deserialize_tuple, Serialize_tuple};

mod common;
use common::{construct_tester, TestHelpers};

/// Copied from basic_nft_actor
#[derive(Serialize_tuple, Deserialize_tuple, Debug, Clone)]
pub struct MintParams {
    metadata_id: Cid,
}

const BASIC_NFT_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_nft_actor/basic_nft_actor.compact.wasm";
// const BASIC_RECEIVER_ACTOR_WASM: &str =
//     "../../target/debug/wbuild/basic_receiving_actor/basic_receiving_actor.compact.wasm";

#[test]
fn it_mints_nfts() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);
    let minter: [Account; 1] = tester.create_accounts().unwrap();

    let actor_address = tester.install_actor_stateless(BASIC_NFT_ACTOR_WASM, 10000);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // Construct the token actor
    tester.call_method(minter[0].1, actor_address, method_hash!("Constructor"), None);

    // TODO: assert that minting calls out to hook

    // Mint a single token
    let mint_params = MintParams { metadata_id: Cid::default() };
    let mint_params = RawBytes::serialize(&mint_params).unwrap();
    let ret_val =
        tester.call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(mint_params));
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let mint_result = ret_val.msg_receipt.return_data.deserialize::<TokenID>().unwrap();
    assert_eq!(mint_result, 0);

    // TODO: check metadata, ownership data etc. is updated
    // Check the total supply increased
    let ret_val = tester.call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 1);

    // Mint a second token
    let mint_params = MintParams { metadata_id: Cid::default() };
    let mint_params = RawBytes::serialize(&mint_params).unwrap();
    let ret_val =
        tester.call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(mint_params));
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let mint_result = ret_val.msg_receipt.return_data.deserialize::<TokenID>().unwrap();
    assert_eq!(mint_result, 1);

    // TODO: check metadata, ownership data etc. is updated
    // Check the total supply increased
    let ret_val = tester.call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 2);

    // Attempt to burn a non-existent token
    let burn_params: TokenID = 100;
    let burn_params = RawBytes::serialize(&burn_params).unwrap();
    let ret_val =
        tester.call_method(minter[0].1, actor_address, method_hash!("Burn"), Some(burn_params));
    // call should fail
    assert!(!ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);

    // Check the total supply didn't change
    let ret_val = tester.call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 2);

    // Burn the correct token
    let burn_params: TokenID = 0;
    let burn_params = RawBytes::serialize(&burn_params).unwrap();
    let ret_val =
        tester.call_method(minter[0].1, actor_address, method_hash!("Burn"), Some(burn_params));
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);

    // TODO: check metadata, ownership data etc. is updated
    // Check the total supply decreased
    let ret_val = tester.call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 1);

    // Cannot burn the same token again
    // Burn the correct token
    let burn_params: TokenID = 0;
    let burn_params = RawBytes::serialize(&burn_params).unwrap();
    let ret_val =
        tester.call_method(minter[0].1, actor_address, method_hash!("Burn"), Some(burn_params));
    // call should fail
    assert!(!ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);

    // Minting the next token uses the next ID
    let mint_params = MintParams { metadata_id: Cid::default() };
    let mint_params = RawBytes::serialize(&mint_params).unwrap();
    let ret_val =
        tester.call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(mint_params));
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let mint_result = ret_val.msg_receipt.return_data.deserialize::<TokenID>().unwrap();
    assert_eq!(mint_result, 2);

    // Check the total supply increased
    let ret_val = tester.call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
    assert!(ret_val.msg_receipt.exit_code.is_success(), "{:#?}", ret_val);
    let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
    assert_eq!(total_supply, 2);
}
