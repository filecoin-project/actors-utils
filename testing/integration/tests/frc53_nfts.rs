use frc42_dispatch::method_hash;
use frc53_nft::{state::TokenID, types::MintReturn};
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;

mod common;
use common::frc53_nft_helpers::{MintParams, NFTHelper};
use common::{construct_tester, TestHelpers};
use helix_test_actors::{BASIC_NFT_ACTOR_BINARY, BASIC_RECEIVING_ACTOR_BINARY};

#[test]
fn test_nft_actor() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);
    let minter: [Account; 1] = tester.create_accounts().unwrap();

    let actor_address = tester.install_actor_stateless(BASIC_NFT_ACTOR_BINARY, 10_000);
    let receiver_address = tester.install_actor_stateless(BASIC_RECEIVING_ACTOR_BINARY, 10_001);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // Construct the token actor
    tester.call_method_ok(minter[0].1, actor_address, method_hash!("Constructor"), None);
    tester.call_method_ok(minter[0].1, receiver_address, method_hash!("Constructor"), None);

    {
        // Mint a single token
        let mint_params = MintParams {
            initial_owner: receiver_address,
            metadata: vec![String::from("metadata")],
            operator_data: RawBytes::default(),
        };
        let mint_params = RawBytes::serialize(&mint_params).unwrap();
        let ret_val = tester.call_method_ok(
            minter[0].1,
            actor_address,
            method_hash!("Mint"),
            Some(mint_params),
        );
        let mint_result = ret_val.msg_receipt.return_data.deserialize::<MintReturn>().unwrap();
        assert_eq!(mint_result.token_ids, vec![0]);
        assert_eq!(mint_result.balance, 1);
        assert_eq!(mint_result.supply, 1);

        // Check the total supply increased
        let ret_val =
            tester.call_method_ok(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
        let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
        assert_eq!(total_supply, 1);

        // Check the balance is correct
        tester.assert_nft_balance(minter[0].1, actor_address, receiver_address, 1);
        // Check the owner is correct
        tester.assert_nft_owner(minter[0].1, actor_address, 0, receiver_address.id().unwrap());
        // Check metatdata is correct
        tester.assert_nft_metadata(minter[0].1, actor_address, 0, "metadata".into())
    }

    {
        // Mint a second token
        let mint_params = MintParams {
            initial_owner: receiver_address,
            metadata: vec![String::from("metadata2")],
            operator_data: RawBytes::default(),
        };
        let mint_params = RawBytes::serialize(&mint_params).unwrap();
        let ret_val = tester.call_method_ok(
            minter[0].1,
            actor_address,
            method_hash!("Mint"),
            Some(mint_params),
        );
        let mint_result = ret_val.msg_receipt.return_data.deserialize::<MintReturn>().unwrap();
        assert_eq!(mint_result.token_ids, vec![1]);
        assert_eq!(mint_result.balance, 2);
        assert_eq!(mint_result.supply, 2);

        // Check the total supply increased
        let ret_val =
            tester.call_method_ok(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
        let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
        assert_eq!(total_supply, 2);

        // Check the balance increased
        tester.assert_nft_balance(minter[0].1, actor_address, receiver_address, 2);
        // Check the owner is correct
        tester.assert_nft_owner(minter[0].1, actor_address, 1, receiver_address.id().unwrap());
        // Check metatdata is correct
        tester.assert_nft_metadata(minter[0].1, actor_address, 1, "metadata2".into())
    }

    {
        // Attempt to burn a non-existent token
        let burn_params: Vec<TokenID> = vec![100];
        let burn_params = RawBytes::serialize(burn_params).unwrap();
        let ret_val =
            tester.call_method(minter[0].1, actor_address, method_hash!("Burn"), Some(burn_params));
        // call should fail
        assert!(!ret_val.msg_receipt.exit_code.is_success(), "{ret_val:#?}");

        // Check the total supply didn't change
        let ret_val =
            tester.call_method_ok(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
        let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
        assert_eq!(total_supply, 2);

        // Check the balance didn't change
        tester.assert_nft_balance(minter[0].1, actor_address, receiver_address, 2);
    }

    {
        // Attempt to burn the correct token but without permission
        let burn_params: Vec<TokenID> = vec![0];
        let burn_params = RawBytes::serialize(burn_params).unwrap();
        let ret_val =
            tester.call_method(minter[0].1, actor_address, method_hash!("Burn"), Some(burn_params));
        assert!(!ret_val.msg_receipt.exit_code.is_success(), "{ret_val:#?}");

        // Check the total supply didn't change
        let ret_val =
            tester.call_method_ok(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
        let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
        assert_eq!(total_supply, 2);

        // Check the balance didn't change
        tester.assert_nft_balance(minter[0].1, actor_address, receiver_address, 2);
    }

    {
        // Minting multiple tokens produces sequential ids
        let mint_params = MintParams {
            initial_owner: receiver_address,
            metadata: vec![String::default(), String::default()],
            operator_data: RawBytes::default(),
        };
        let mint_params = RawBytes::serialize(&mint_params).unwrap();
        let ret_val =
            tester.call_method(minter[0].1, actor_address, method_hash!("Mint"), Some(mint_params));
        assert!(ret_val.msg_receipt.exit_code.is_success(), "{ret_val:#?}");
        let mint_result = ret_val.msg_receipt.return_data.deserialize::<MintReturn>().unwrap();
        assert_eq!(mint_result.token_ids, vec![2, 3]);
        assert_eq!(mint_result.balance, 4);
        assert_eq!(mint_result.supply, 4);

        // Check the total supply increased by two
        let ret_val =
            tester.call_method(minter[0].1, actor_address, method_hash!("TotalSupply"), None);
        assert!(ret_val.msg_receipt.exit_code.is_success(), "{ret_val:#?}");
        let total_supply = ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap();
        // Check the owner is correct
        tester.assert_nft_owner(minter[0].1, actor_address, 2, receiver_address.id().unwrap());
        tester.assert_nft_owner(minter[0].1, actor_address, 3, receiver_address.id().unwrap());
        assert_eq!(total_supply, 4);
    }
}
