use std::env;

use cid::Cid;
use frc42_dispatch::method_hash;
use fvm::{
    executor::{ApplyKind, ApplyRet, Executor},
    externs::Externs,
};
use fvm_integration_tests::{bundle, tester::Tester};
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::{
    tuple::{Deserialize_tuple, Serialize_tuple},
    Cbor, RawBytes,
};
use fvm_shared::{
    address::Address, bigint::Zero, econ::TokenAmount, message::Message, state::StateTreeVersion,
    version::NetworkVersion,
};
use serde::Serialize;

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

pub fn load_actor_wasm(path: &str) -> Vec<u8> {
    let wasm_path = env::current_dir().unwrap().join(path).canonicalize().unwrap();

    std::fs::read(wasm_path).expect("unable to read actor file")
}

/// Construct a Tester with the provided blockstore
/// mainly cuts down on noise with importing the built-in actor bundle and network/state tree versions
pub fn construct_tester<BS: Blockstore + Clone, E: Externs>(blockstore: &BS) -> Tester<BS, E> {
    let bundle_root = bundle::import_bundle(&blockstore, actors_v10::BUNDLE_CAR).unwrap();

    Tester::new(NetworkVersion::V15, StateTreeVersion::V4, bundle_root, blockstore.clone()).unwrap()
}

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

/// Helper routines to simplify common token operations
pub trait TokenHelpers {
    /// Get balance from token actor for a given address
    /// This is a very common thing to check during tests
    fn get_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    ) -> TokenAmount;

    fn mint_tokens(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
        amount: TokenAmount,
        operator_data: RawBytes,
    ) -> ApplyRet;
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

impl<B: Blockstore, E: Externs> TokenHelpers for Tester<B, E> {
    fn get_balance(
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
}
