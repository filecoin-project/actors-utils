use std::env;

use cid::Cid;
use fvm::{
    executor::{ApplyKind, ApplyRet, Executor},
    externs::Externs,
};
use fvm_integration_tests::{bundle, tester::Tester};
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::RawBytes;
use fvm_shared::{
    address::Address, bigint::Zero, econ::TokenAmount, message::Message, state::StateTreeVersion,
    version::NetworkVersion,
};
use serde::Serialize;

/// Helper routines to simplify common operations with a Tester
pub trait TestHelpers {
    /// Call a method on an actor
    fn call_method(
        &mut self,
        from: Address,
        to: Address,
        method_num: u64,
        params: Option<RawBytes>,
    ) -> ApplyRet;

    /// Call a method on an actor and assert a successful result
    fn call_method_ok(
        &mut self,
        from: Address,
        to: Address,
        method_num: u64,
        params: Option<RawBytes>,
    ) -> ApplyRet;

    /// Install an actor with initial state and ID
    /// Returns the actor's address
    fn install_actor_with_state<S: Serialize>(
        &mut self,
        path: &str,
        actor_id: u64,
        state: S,
    ) -> Address;

    /// Install an actor with no initial state
    /// Takes ID and returns the new actor's address
    fn install_actor_stateless(&mut self, path: &str, actor_id: u64) -> Address;
}

pub fn load_actor_wasm(path: &str) -> Vec<u8> {
    let wasm_path = env::current_dir().unwrap().join(path).canonicalize().unwrap();

    std::fs::read(wasm_path).expect("unable to read actor file")
}

/// Construct a Tester with the provided blockstore
/// mainly cuts down on noise with importing the built-in actor bundle and network/state tree versions
pub fn construct_tester<BS: Blockstore + Clone, E: Externs>(blockstore: &BS) -> Tester<BS, E> {
    let bundle_root = bundle::import_bundle(&blockstore, actors_v9::BUNDLE_CAR).unwrap();

    Tester::new(NetworkVersion::V15, StateTreeVersion::V4, bundle_root, blockstore.clone()).unwrap()
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

    fn call_method_ok(
        &mut self,
        from: Address,
        to: Address,
        method_num: u64,
        params: Option<RawBytes>,
    ) -> ApplyRet {
        let ret = self.call_method(from, to, method_num, params);
        assert!(ret.msg_receipt.exit_code.is_success(), "call failed: {ret:?}");
        ret
    }

    fn install_actor_with_state<S: Serialize>(
        &mut self,
        path: &str,
        actor_id: u64,
        state: S,
    ) -> Address {
        let code = load_actor_wasm(path);
        let address = Address::new_id(actor_id);
        let state_cid = self.set_state(&state).unwrap();
        self.set_actor_from_bin(&code, state_cid, address, TokenAmount::zero()).unwrap();
        address
    }

    fn install_actor_stateless(&mut self, path: &str, actor_id: u64) -> Address {
        let code = load_actor_wasm(path);
        let address = Address::new_id(actor_id);
        self.set_actor_from_bin(&code, Cid::default(), address, TokenAmount::zero()).unwrap();
        address
    }
}

pub mod frc46_token {
    use frc42_dispatch::method_hash;
    use fvm::{executor::ApplyRet, externs::Externs};
    use fvm_integration_tests::tester::Tester;
    use fvm_ipld_blockstore::Blockstore;
    use fvm_ipld_encoding::{Cbor, RawBytes};
    use fvm_shared::{address::Address, bigint::Zero, econ::TokenAmount};
    use serde_tuple::{Deserialize_tuple, Serialize_tuple};

    use super::TestHelpers;

    // this is here so we don't need to link every test against the basic_token_actor
    // otherwise we can't link against test_actor or any other test/example actors,
    // because the invoke() functions will conflict at link time
    #[derive(Serialize_tuple, Deserialize_tuple, Clone, Debug)]
    pub struct MintParams {
        pub initial_owner: Address,
        pub amount: TokenAmount,
        pub operator_data: RawBytes,
    }

    impl Cbor for MintParams {}

    /// Helper routines to simplify common token operations
    pub trait TokenHelpers {
        /// Get balance from token actor for a given address
        /// This is a very common thing to check during tests
        fn token_balance(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
        ) -> TokenAmount;

        /// Mint tokens from token_actor to target address
        fn mint_tokens(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: TokenAmount,
            operator_data: RawBytes,
        ) -> ApplyRet;

        /// Mint tokens from token_actor to target address and assert a successful result
        fn mint_tokens_ok(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: TokenAmount,
            operator_data: RawBytes,
        ) -> ApplyRet;

        /// Check token balance, asserting that balance matches the provided amount
        fn assert_token_balance(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: TokenAmount,
        );

        /// Check token balance, asserting a zero balance
        fn assert_token_balance_zero(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
        );
    }

    impl<B: Blockstore, E: Externs> TokenHelpers for Tester<B, E> {
        fn token_balance(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
        ) -> TokenAmount {
            let params = RawBytes::serialize(target).unwrap();
            let ret_val =
                self.call_method(operator, token_actor, method_hash!("BalanceOf"), Some(params));
            ret_val.msg_receipt.return_data.deserialize::<TokenAmount>().unwrap()
        }

        fn mint_tokens(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: TokenAmount,
            operator_data: RawBytes,
        ) -> ApplyRet {
            let mint_params = MintParams { initial_owner: target, amount, operator_data };
            let params = RawBytes::serialize(mint_params).unwrap();
            self.call_method(operator, token_actor, method_hash!("Mint"), Some(params))
        }

        fn mint_tokens_ok(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: TokenAmount,
            operator_data: RawBytes,
        ) -> ApplyRet {
            let ret = self.mint_tokens(operator, token_actor, target, amount, operator_data);
            assert!(ret.msg_receipt.exit_code.is_success());
            ret
        }

        fn assert_token_balance(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: TokenAmount,
        ) {
            let balance = self.token_balance(operator, token_actor, target);
            assert_eq!(balance, amount);
        }

        fn assert_token_balance_zero(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
        ) {
            let balance = self.token_balance(operator, token_actor, target);
            assert_eq!(balance, TokenAmount::zero());
        }
    }
}

pub mod frcxx_nft {
    use cid::Cid;
    use frc42_dispatch::method_hash;
    use fvm::{executor::ApplyRet, externs::Externs};
    use fvm_integration_tests::tester::Tester;
    use fvm_ipld_blockstore::Blockstore;
    use fvm_ipld_encoding::{Cbor, RawBytes};
    use fvm_shared::address::Address;
    use serde_tuple::{Deserialize_tuple, Serialize_tuple};

    use super::TestHelpers;

    // this is here so we don't need to link every test against the basic_token_actor
    // otherwise we can't link against frc46_test_actor or any other test/example actors,
    // because the invoke() functions will conflict at link time
    #[derive(Serialize_tuple, Deserialize_tuple, Debug, Clone)]
    pub struct MintParams {
        initial_owner: Address,
        metadata: Vec<Cid>,
        operator_data: RawBytes,
    }

    impl Cbor for MintParams {}

    pub trait NFTHelpers {
        /// Get balance from token actor for a given address
        /// This is a very common thing to check during tests
        fn nft_balance(&mut self, operator: Address, token_actor: Address, target: Address) -> u64;

        /// Mint tokens from token_actor to target address
        fn mint_nfts(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: usize,
            operator_data: RawBytes,
        ) -> ApplyRet;

        /// Mint tokens from token_actor to target address and assert a successful result
        fn mint_nfts_ok(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: usize,
            operator_data: RawBytes,
        ) -> ApplyRet;

        /// Check token balance, asserting that balance matches the provided amount
        fn assert_nft_balance(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: u64,
        );

        /// Check token balance, asserting a zero balance
        fn assert_nft_balance_zero(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
        );
    }

    impl<BS: Blockstore, E: Externs> NFTHelpers for Tester<BS, E> {
        fn nft_balance(&mut self, operator: Address, token_actor: Address, target: Address) -> u64 {
            let params = RawBytes::serialize(target).unwrap();
            let ret_val =
                self.call_method(operator, token_actor, method_hash!("Balance"), Some(params));
            ret_val.msg_receipt.return_data.deserialize::<u64>().unwrap()
        }

        fn mint_nfts(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: usize,
            operator_data: RawBytes,
        ) -> ApplyRet {
            let params = RawBytes::serialize(MintParams {
                initial_owner: target,
                metadata: vec![Cid::default(); amount],
                operator_data,
            })
            .unwrap();

            self.call_method(operator, token_actor, method_hash!("Mint"), Some(params))
        }

        fn mint_nfts_ok(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: usize,
            operator_data: RawBytes,
        ) -> ApplyRet {
            let ret = self.mint_nfts(operator, token_actor, target, amount, operator_data);
            assert!(ret.msg_receipt.exit_code.is_success());
            ret
        }

        fn assert_nft_balance(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
            amount: u64,
        ) {
            let balance = self.nft_balance(operator, token_actor, target);
            assert_eq!(balance, amount);
        }

        fn assert_nft_balance_zero(
            &mut self,
            operator: Address,
            token_actor: Address,
            target: Address,
        ) {
            let balance = self.nft_balance(operator, token_actor, target);
            assert_eq!(balance, 0);
        }
    }
}
