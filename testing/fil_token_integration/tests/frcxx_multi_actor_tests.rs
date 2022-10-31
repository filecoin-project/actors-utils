use cid::Cid;
use frc42_dispatch::method_hash;
use frcxx_nft::state::NFTState;
use frcxx_nft::types::{MintReturn, TransferReturn};
use fvm_integration_tests::{dummy::DummyExterns, tester::Account};
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, receipt::Receipt};

mod common;
use common::frcxx_nft::{MintParams, NFTHelpers};
use common::{construct_tester, TestHelpers};
use frcxx_test_actor::{action, ActionParams, TestAction};

const BASIC_NFT_ACTOR_WASM: &str =
    "../../target/debug/wbuild/basic_nft_actor/basic_nft_actor.compact.wasm";
const TEST_ACTOR_WASM: &str =
    "../../target/debug/wbuild/frcxx_test_actor/frcxx_test_actor.compact.wasm";

fn action_params(token_address: Address, action: TestAction) -> RawBytes {
    RawBytes::serialize(ActionParams { token_address, action }).unwrap()
}

#[test]
fn frcxx_multi_actor_tests() {
    let blockstore = MemoryBlockstore::default();
    let mut tester = construct_tester(&blockstore);

    let operator: [Account; 1] = tester.create_accounts().unwrap();
    let op_addr = operator[0].1;

    let initial_nft_state = NFTState::new(&blockstore).unwrap();

    let token_actor =
        tester.install_actor_with_state(BASIC_NFT_ACTOR_WASM, 10000, initial_nft_state.clone());
    // we'll use up to four actors for some of these tests, though most use only two
    let alice = tester.install_actor_stateless(TEST_ACTOR_WASM, 10010);
    let bob = tester.install_actor_stateless(TEST_ACTOR_WASM, 10011);
    let carol = tester.install_actor_stateless(TEST_ACTOR_WASM, 10012);
    let dave = tester.install_actor_stateless(TEST_ACTOR_WASM, 10013);

    // instantiate machine
    tester.instantiate_machine(DummyExterns).unwrap();

    // construct actors
    for actor in [token_actor, alice, bob, carol, dave] {
        let ret_val = tester.call_method(op_addr, actor, method_hash!("Constructor"), None);
        assert!(ret_val.msg_receipt.exit_code.is_success());
    }

    // TEST: mint to token contract itself
    {
        let mint_params = MintParams {
            initial_owner: token_actor,
            metadata: vec![Cid::default()],
            operator_data: RawBytes::default(),
        };

        let params = RawBytes::serialize(mint_params).unwrap();
        let ret_val = tester.call_method(op_addr, token_actor, method_hash!("Mint"), Some(params));

        assert!(!ret_val.msg_receipt.exit_code.is_success());
        tester.assert_nft_total_supply_zero(op_addr, token_actor);
        tester.assert_nft_balance_zero(op_addr, token_actor, bob);

        // Total Supply: 0
        // Alice: 0
        // Bob: 0
        // Next ID: 0
    }

    // TEST: transfer to token contract itself
    {
        let action_params = action_params(
            token_actor,
            TestAction::Transfer(token_actor, vec![], RawBytes::default()),
        );
        let ret_val =
            tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(action_params));

        // the return value should be a failure
        let action_return: Receipt = ret_val.msg_receipt.return_data.deserialize().unwrap();
        assert!(!action_return.exit_code.is_success());

        // balances are unchanged
        tester.assert_nft_total_supply_zero(op_addr, token_actor);
        tester.assert_nft_balance_zero(op_addr, token_actor, alice);
        tester.assert_nft_balance_zero(op_addr, token_actor, token_actor);

        // Total Supply: 0
        // Alice: 0
        // Bob: 0
        // Next ID: 0
    }

    // TEST: mint to alice who rejects in receive hook
    {
        let mint_params = MintParams {
            initial_owner: alice,
            metadata: vec![Cid::default()],
            operator_data: action(TestAction::Reject),
        };
        let params = RawBytes::serialize(mint_params).unwrap();
        let ret_val = tester.call_method(op_addr, token_actor, method_hash!("Mint"), Some(params));

        // method fails
        assert!(!ret_val.msg_receipt.exit_code.is_success());
        // balances are unchanged
        tester.assert_nft_total_supply_zero(op_addr, token_actor);
        tester.assert_nft_balance_zero(op_addr, token_actor, bob);

        // Total Supply: 0
        // Alice: 0
        // Bob: 0
        // Next ID: 0
    }

    // TEST: alice transfers zero-amount to herself, accepting
    {
        let action_params = action_params(
            token_actor,
            TestAction::Transfer(alice, vec![], action(TestAction::Accept)),
        );
        let ret_val =
            tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(action_params));

        // the return value should update the new state
        let action_return: Receipt = ret_val.msg_receipt.return_data.deserialize().unwrap();
        let transfer_return: TransferReturn = action_return.return_data.deserialize().unwrap();
        assert_eq!(transfer_return.from_balance, 0);
        assert_eq!(transfer_return.to_balance, 0);
        assert_eq!(transfer_return.token_ids, Vec::<u64>::default());

        // method succeeds
        assert!(ret_val.msg_receipt.exit_code.is_success());
        // balances are unchanged
        tester.assert_nft_total_supply_zero(op_addr, token_actor);
        tester.assert_nft_balance_zero(op_addr, token_actor, alice);

        // Total Supply: 0
        // Alice: 0
        // Bob: 0
        // Next ID: 0
    }

    // TEST: alice transfers zero-amount to herself, rejecting
    {
        let action_params = action_params(
            token_actor,
            TestAction::Transfer(alice, vec![], action(TestAction::Reject)),
        );
        let ret_val =
            tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(action_params));

        // the return value should be a failure
        let action_return: Receipt = ret_val.msg_receipt.return_data.deserialize().unwrap();
        assert!(!action_return.exit_code.is_success());

        // balances are unchanged
        tester.assert_nft_total_supply_zero(op_addr, token_actor);
        tester.assert_nft_balance_zero(op_addr, token_actor, alice);

        // Total Supply: 0
        // Alice: 0
        // Bob: 0
        // Next ID: 0
    }

    // TEST: mint to alice who burns the token
    {
        // this is the first mint (so will produce token id 0)
        let ret_val =
            tester.mint_nfts_ok(op_addr, token_actor, alice, 1, action(TestAction::Burn(vec![0])));

        // the return value shows the completed state
        let mint_return: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
        assert_eq!(mint_return.balance, 0);
        // although it was subsequently burned, the token_id 0 was successfully minted
        assert_eq!(mint_return.token_ids, vec![0]);

        // balances are unchanged
        tester.assert_nft_total_supply_zero(op_addr, token_actor);
        tester.assert_nft_balance_zero(op_addr, token_actor, alice);

        // Total Supply: 0
        // Alice: 0
        // Bob: 0
        // Next ID: 1
    }

    // TEST: mint to alice who transfers to bob in hook
    {
        // this is the second mint (so will produce token id 1)
        let ret_val = tester.mint_nfts_ok(
            op_addr,
            token_actor,
            alice,
            1,
            action(TestAction::Transfer(bob, vec![1], action(TestAction::Accept))),
        );

        // the return value shows the completed state
        let mint_return: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
        // alice's balance is zero since she transferred it on to bob
        assert_eq!(mint_return.balance, 0);
        assert_eq!(mint_return.token_ids, vec![1]);

        // check global state
        tester.assert_nft_total_supply(op_addr, token_actor, 1);
        tester.assert_nft_balance_zero(op_addr, token_actor, alice);
        tester.assert_nft_balance(op_addr, token_actor, bob, 1);
        // ended up in bob's ownership
        tester.assert_nft_owner(op_addr, token_actor, 1, bob.id().unwrap());

        // Total Supply: 1
        // Alice: []
        // Bob: [1]
        // Next ID: 2
    }

    // TEST: mint to alice who transfers to bob in hook, but bob rejects it
    {
        // this is the third mint (so will produce token id 2)
        let ret_val = tester.mint_nfts_ok(
            op_addr,
            token_actor,
            alice,
            1,
            action(TestAction::Transfer(bob, vec![2], action(TestAction::Reject))),
        );

        // the return value shows the completed state
        let mint_return: MintReturn = ret_val.msg_receipt.return_data.deserialize().unwrap();
        // the token remains with alice
        assert_eq!(mint_return.balance, 1);
        assert_eq!(mint_return.token_ids, vec![2]);

        // check global state
        tester.assert_nft_total_supply(op_addr, token_actor, 2);
        tester.assert_nft_balance(op_addr, token_actor, alice, 1);
        tester.assert_nft_balance(op_addr, token_actor, bob, 1);
        // ownership remained with alice
        tester.assert_nft_owner(op_addr, token_actor, 2, alice.id().unwrap());

        // Total Supply: 2
        // Alice: [2]
        // Bob: [1]
        // Next ID: 3
    }

    // TEST: alice transfers non-zero amount to self
    {
        let params = action_params(
            token_actor,
            TestAction::Transfer(alice, vec![2], action(TestAction::Accept)),
        );
        let ret_val = tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(params));

        // the return value should update the new state
        let action_return: Receipt = ret_val.msg_receipt.return_data.deserialize().unwrap();
        let transfer_return: TransferReturn = action_return.return_data.deserialize().unwrap();
        assert_eq!(transfer_return.from_balance, 1);
        assert_eq!(transfer_return.to_balance, 1);
        assert_eq!(transfer_return.token_ids, vec![2]);

        // check global state
        tester.assert_nft_total_supply(op_addr, token_actor, 2);
        tester.assert_nft_balance(op_addr, token_actor, alice, 1);
        tester.assert_nft_balance(op_addr, token_actor, bob, 1);

        // Total Supply: 2
        // Alice: [2]
        // Bob: [1]
        // Next ID: 3
    }

    // TEST: alice sends bob a transfer of zero amount, bob rejects
    {
        let params = action_params(
            token_actor,
            TestAction::Transfer(bob, vec![], action(TestAction::Reject)),
        );
        let ret_val = tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(params));

        // we told bob to reject, so the action call should return success but give us the error result as return data
        // check the receipt we got in return data
        let bob_receipt = ret_val.msg_receipt.return_data.deserialize::<Receipt>().unwrap();
        assert!(!bob_receipt.exit_code.is_success());

        // state unchanged
        tester.assert_nft_total_supply(op_addr, token_actor, 2);
        tester.assert_nft_balance(op_addr, token_actor, alice, 1);
        tester.assert_nft_balance(op_addr, token_actor, bob, 1);

        // Total Supply: 2
        // Alice: [2]
        // Bob: [1]
        // Next ID: 3
    }

    {
        // now tell bob to accept it
        let params = action_params(
            token_actor,
            TestAction::Transfer(bob, vec![], action(TestAction::Accept)),
        );
        let ret_val = tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(params));

        // check the receipt we got in return data
        let bob_receipt = ret_val.msg_receipt.return_data.deserialize::<Receipt>().unwrap();
        assert!(bob_receipt.exit_code.is_success());
        let transfer_return: TransferReturn = bob_receipt.return_data.deserialize().unwrap();
        assert_eq!(transfer_return.from_balance, 1);
        assert_eq!(transfer_return.to_balance, 1);
        assert_eq!(transfer_return.token_ids, Vec::<u64>::default());

        // state unchanged
        tester.assert_nft_total_supply(op_addr, token_actor, 2);
        tester.assert_nft_balance(op_addr, token_actor, alice, 1);
        tester.assert_nft_balance(op_addr, token_actor, bob, 1);

        // Total Supply: 2
        // Alice: [2]
        // Bob: [1]
        // Next ID: 3
    }

    {
        // mint some tokens to alice so we can test batch transfers
        let ret_val =
            tester.mint_nfts_ok(op_addr, token_actor, alice, 3, action(TestAction::Accept));
        let mint_return = ret_val.msg_receipt.return_data.deserialize::<MintReturn>().unwrap();
        assert_eq!(mint_return.supply, 5); // 2 from before, 3 from this mint
        assert_eq!(mint_return.balance, 4); // 1 from before, 3 from this mint
        assert_eq!(mint_return.token_ids, vec![3, 4, 5]); // three minted total from before (one was burned)

        // check state after minting
        tester.assert_nft_total_supply(op_addr, token_actor, 5);
        tester.assert_nft_balance(op_addr, token_actor, alice, 4);
        tester.assert_nft_balance(op_addr, token_actor, bob, 1);

        // check ownership
        tester.assert_nft_owner(op_addr, token_actor, 3, alice.id().unwrap());
        tester.assert_nft_owner(op_addr, token_actor, 4, alice.id().unwrap());
        tester.assert_nft_owner(op_addr, token_actor, 5, alice.id().unwrap());

        // Total Supply: 5
        // Alice: [2, 3, 4, 5]
        // Bob: [1]
        // Next ID: 6
    }

    // TEST: transfer from alice to bob who will reject them
    {
        // send to bob who will reject them
        let params = action_params(
            token_actor,
            TestAction::Transfer(bob, vec![2], action(TestAction::Reject)),
        );
        let ret_val = tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(params));
        // check the receipt we got in return data
        let receipt = ret_val.msg_receipt.return_data.deserialize::<Receipt>().unwrap();
        assert!(!receipt.exit_code.is_success());

        // check state is unchanged
        tester.assert_nft_total_supply(op_addr, token_actor, 5);
        tester.assert_nft_balance(op_addr, token_actor, alice, 4);
        tester.assert_nft_balance(op_addr, token_actor, bob, 1);

        // check ownership was unchanged
        tester.assert_nft_owner(op_addr, token_actor, 3, alice.id().unwrap());
        tester.assert_nft_owner(op_addr, token_actor, 4, alice.id().unwrap());
        tester.assert_nft_owner(op_addr, token_actor, 5, alice.id().unwrap());

        // Total Supply: 5
        // Alice: [2, 3, 4, 5]
        // Bob: [1]
        // Next ID: 6
    }

    // TEST: transfer from alice to bob who will accept it
    {
        let params = action_params(
            token_actor,
            TestAction::Transfer(bob, vec![2], action(TestAction::Accept)),
        );
        let ret_val = tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(params));
        // check the receipt we got in return data
        let action_receipt = ret_val.msg_receipt.return_data.deserialize::<Receipt>().unwrap();

        assert!(action_receipt.exit_code.is_success());

        // check the transfer return data
        let transfer_return: TransferReturn = action_receipt.return_data.deserialize().unwrap();
        assert_eq!(transfer_return.from_balance, 3);
        assert_eq!(transfer_return.to_balance, 2);
        assert_eq!(transfer_return.token_ids, vec![2]);

        // check the updated state
        tester.assert_nft_total_supply(op_addr, token_actor, 5);
        tester.assert_nft_balance(op_addr, token_actor, alice, 3);
        tester.assert_nft_balance(op_addr, token_actor, bob, 2);

        // check ownership was unchanged
        tester.assert_nft_owner(op_addr, token_actor, 2, bob.id().unwrap());

        // Total Supply: 5
        // Alice: [3, 4, 5]
        // Bob: [1, 2]
        // Next ID: 6
    }

    // TEST: transfer a batch from alice to bob who will burn some of it
    {
        let params = action_params(
            token_actor,
            // transfer 3 tokens, have bob burn the first 2
            TestAction::Transfer(bob, vec![3, 4, 5], action(TestAction::Burn(vec![3, 4]))),
        );
        let ret_val = tester.call_method_ok(op_addr, alice, method_hash!("Action"), Some(params));
        // check the receipt we got in return data
        let action_receipt = ret_val.msg_receipt.return_data.deserialize::<Receipt>().unwrap();

        assert!(action_receipt.exit_code.is_success());

        // check the transfer return data
        let transfer_return: TransferReturn = action_receipt.return_data.deserialize().unwrap();
        assert_eq!(transfer_return.from_balance, 0); // 3 to start - 3 transferred
        assert_eq!(transfer_return.to_balance, 3); // 2 to start + 3 transferred - 2 burned
        assert_eq!(transfer_return.token_ids, vec![3, 4, 5]); // all 3 tokens were transferred

        // check the updated state
        tester.assert_nft_total_supply(op_addr, token_actor, 3); // 5 to start - 2 burned
        tester.assert_nft_balance(op_addr, token_actor, alice, 0);
        tester.assert_nft_balance(op_addr, token_actor, bob, 3);

        // check ownership was updated
        tester.assert_nft_owner(op_addr, token_actor, 5, bob.id().unwrap());

        // Total Supply: 3
        // Alice: []
        // Bob: [1, 2, 5]
        // Next ID: 6
    }
}
