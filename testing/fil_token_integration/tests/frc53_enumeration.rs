use frc42_dispatch::method_hash;
use frc53_nft::state::NFTState;
use frc53_nft::types::{
    ListAccountOperatorsParams, ListAccountOperatorsReturn, ListOperatorTokensParams,
    ListOperatorTokensReturn, ListOwnedTokensParams, ListOwnedTokensReturn,
    ListTokenOperatorsParams, ListTokenOperatorsReturn, ListTokensParams, ListTokensReturn,
};
use fvm_actor_utils::shared_blockstore::SharedMemoryBlockstore;
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_bitfield::bitfield;
use fvm_ipld_encoding::RawBytes;

mod common;
use common::{construct_tester, TestHelpers};

const BASIC_NFT_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_nft_actor/basic_nft_actor.compact.wasm";

#[test]
pub fn test_nft_enumerations() {
    let blockstore = SharedMemoryBlockstore::default();

    // Create testing accounts
    let mut tester = construct_tester(&blockstore);
    let [alice, bob, operator]: [Account; 3] = tester.create_accounts().unwrap();

    // Create a new actor with prefilled state
    let mut state = NFTState::new(&blockstore).unwrap();
    // Mint four tokens for alice
    state
        .mint_tokens(
            &blockstore,
            alice.0,
            vec![
                String::from("alice0"),
                String::from("alice1"),
                String::from("alice2"),
                String::from("alice3"),
            ],
        )
        .unwrap();
    // Burn alice's first token
    state.burn_tokens(&blockstore, alice.0, &[0], |_token_data, _token_id| Ok(())).unwrap();
    // Mint a token for bob
    state.mint_tokens(&blockstore, bob.0, vec![String::from("bob4")]).unwrap();
    // Set the operator as an operator for one out of alice's three tokens
    state
        .approve_for_tokens(&blockstore, operator.0, &[1], |_token_data, _token_id| Ok(()))
        .unwrap();
    // Set the operator as an account-level operator for bob
    state.approve_for_owner(&blockstore, bob.0, operator.0).unwrap();

    // Install the actor with the seeded state
    let actor_address = tester.install_actor_with_state(BASIC_NFT_ACTOR_WASM, 10_000, state);

    // Instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // List all the tokens
    {
        let list_tokens_params = ListTokensParams { cursor: RawBytes::default(), limit: u64::MAX };
        let list_tokens_params = RawBytes::serialize(list_tokens_params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListTokens"),
            Some(list_tokens_params),
        );

        let list_tokens_result =
            ret_val.msg_receipt.return_data.deserialize::<ListTokensReturn>().unwrap();
        assert_eq!(list_tokens_result.tokens, bitfield![0, 1, 1, 1, 1]);
        assert!(list_tokens_result.next_cursor.is_none());
    }

    // List the tokens in pairs
    {
        // List the first two token ids
        let list_tokens_params = ListTokensParams { cursor: RawBytes::default(), limit: 2 };
        let list_tokens_params = RawBytes::serialize(list_tokens_params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListTokens"),
            Some(list_tokens_params),
        );
        let list_tokens_result =
            ret_val.msg_receipt.return_data.deserialize::<ListTokensReturn>().unwrap();
        assert_eq!(list_tokens_result.tokens, bitfield![0, 1, 1]);
        assert!(list_tokens_result.next_cursor.is_some());

        // Attempt to list the next (final) two tokens
        let list_tokens_params =
            ListTokensParams { cursor: list_tokens_result.next_cursor.unwrap(), limit: 2 };
        let list_tokens_params = RawBytes::serialize(list_tokens_params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListTokens"),
            Some(list_tokens_params),
        );
        let list_tokens_result =
            ret_val.msg_receipt.return_data.deserialize::<ListTokensReturn>().unwrap();
        // the first three are empty because they come before the cursor
        assert_eq!(list_tokens_result.tokens, bitfield![0, 0, 0, 1, 1]);
        // There are no more
        assert!(list_tokens_result.next_cursor.is_none());
    }

    // List owned tokens
    {
        // List all the tokens minted to alice
        let params =
            ListOwnedTokensParams { owner: alice.1, cursor: RawBytes::default(), limit: u64::MAX };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListOwnedTokens"),
            Some(params),
        );
        let list_tokens_result =
            ret_val.msg_receipt.return_data.deserialize::<ListOwnedTokensReturn>().unwrap();
        assert_eq!(list_tokens_result.tokens, bitfield![0, 1, 1, 1]);
        assert!(list_tokens_result.next_cursor.is_none());

        // Check that bob has the fifth token
        let params =
            ListOwnedTokensParams { owner: bob.1, cursor: RawBytes::default(), limit: u64::MAX };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListOwnedTokens"),
            Some(params),
        );
        let list_tokens_result =
            ret_val.msg_receipt.return_data.deserialize::<ListOwnedTokensReturn>().unwrap();
        assert_eq!(list_tokens_result.tokens, bitfield![0, 0, 0, 0, 1]);
    }

    // List owned tokens in varying page sizes
    {
        // List first two tokens of alice's tokens
        let params =
            ListOwnedTokensParams { owner: alice.1, cursor: RawBytes::default(), limit: 2 };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListOwnedTokens"),
            Some(params),
        );
        let call_result =
            ret_val.msg_receipt.return_data.deserialize::<ListOwnedTokensReturn>().unwrap();
        assert_eq!(call_result.tokens, bitfield![0, 1, 1]);
        assert!(call_result.next_cursor.is_some());

        // Attempt to list the next ten of alice's tokens
        let params = ListOwnedTokensParams {
            owner: alice.1,
            cursor: call_result.next_cursor.unwrap(),
            limit: 10,
        };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListOwnedTokens"),
            Some(params),
        );
        // Should only receive one more and an empty cursor
        let call_result =
            ret_val.msg_receipt.return_data.deserialize::<ListOwnedTokensReturn>().unwrap();
        assert_eq!(call_result.tokens, bitfield![0, 0, 0, 1]);
        assert!(call_result.next_cursor.is_none());
    }

    // List token operators
    {
        // List all the operators for alice's first token
        let params =
            ListTokenOperatorsParams { token_id: 1, cursor: RawBytes::default(), limit: u64::MAX };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListTokenOperators"),
            Some(params),
        );
        let call_result =
            ret_val.msg_receipt.return_data.deserialize::<ListTokenOperatorsReturn>().unwrap();
        // The operator is approved for token 1
        assert!(call_result.operators.get(operator.0));
        assert_eq!(call_result.operators.len(), 1);
        assert!(call_result.next_cursor.is_none());

        // List all the operators for alice's second
        let params =
            ListTokenOperatorsParams { token_id: 2, cursor: RawBytes::default(), limit: u64::MAX };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListTokenOperators"),
            Some(params),
        );
        let call_result =
            ret_val.msg_receipt.return_data.deserialize::<ListTokenOperatorsReturn>().unwrap();
        // No-one is approved for token 2
        assert!(!call_result.operators.get(operator.0));
        assert_eq!(call_result.operators.len(), 0);
        assert!(call_result.next_cursor.is_none());

        // List all the operators for bob's token
        let params =
            ListTokenOperatorsParams { token_id: 4, cursor: RawBytes::default(), limit: u64::MAX };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListTokenOperators"),
            Some(params),
        );
        let call_result =
            ret_val.msg_receipt.return_data.deserialize::<ListTokenOperatorsReturn>().unwrap();
        // Even though the operator is an account-level operator, they are not specifically approved for token 4
        assert!(!call_result.operators.get(operator.0));
        assert_eq!(call_result.operators.len(), 0);
        assert!(call_result.next_cursor.is_none());
    }

    // List OperatorTokens
    {
        // List all the tokens operators token-level approved ids
        let params = ListOperatorTokensParams {
            operator: operator.1,
            cursor: RawBytes::default(),
            limit: u64::MAX,
        };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListOperatorTokens"),
            Some(params),
        );
        let call_result =
            ret_val.msg_receipt.return_data.deserialize::<ListOperatorTokensReturn>().unwrap();
        // Approved for the first non-burned token
        assert_eq!(call_result.tokens, bitfield![0, 1]);
    }

    // List AccountOperators
    {
        // List all the operators for alice
        let params = ListAccountOperatorsParams {
            cursor: RawBytes::default(),
            limit: u64::MAX,
            owner: alice.1,
        };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListAccountOperators"),
            Some(params),
        );
        let call_result =
            ret_val.msg_receipt.return_data.deserialize::<ListAccountOperatorsReturn>().unwrap();
        // The operator is not account-level approved for alice
        assert!(call_result.operators.is_empty());
        assert!(call_result.next_cursor.is_none());

        // List all the operators for bob
        let params = ListAccountOperatorsParams {
            cursor: RawBytes::default(),
            limit: u64::MAX,
            owner: bob.1,
        };
        let params = RawBytes::serialize(params).unwrap();
        let ret_val = tester.call_method_ok(
            operator.1,
            actor_address,
            method_hash!("ListAccountOperators"),
            Some(params),
        );
        let call_result =
            ret_val.msg_receipt.return_data.deserialize::<ListAccountOperatorsReturn>().unwrap();
        // The operator is approved for bob
        assert!(call_result.operators.get(operator.0));
        assert_eq!(call_result.operators.len(), 1);
        assert!(call_result.next_cursor.is_none());
    }
}
