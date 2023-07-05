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
    version::NetworkVersion, BLOCK_GAS_LIMIT,
};
use serde::Serialize;

pub mod frc46_token_helpers;
pub mod frc53_nft_helpers;

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
        code: &[u8],
        actor_id: u64,
        state: S,
    ) -> Address;

    /// Install an actor with no initial state
    /// Takes ID and returns the new actor's address
    fn install_actor_stateless(&mut self, code: &[u8], actor_id: u64) -> Address;
}

#[allow(dead_code)]
pub fn load_actor_wasm(path: &str) -> Vec<u8> {
    let wasm_path = env::current_dir().unwrap().join(path).canonicalize().unwrap();
    std::fs::read(wasm_path).expect("unable to read actor file")
}

/// Construct a Tester with the provided blockstore
/// mainly cuts down on noise with importing the built-in actor bundle and network/state tree versions
pub fn construct_tester<BS: Blockstore + Clone, E: Externs>(blockstore: &BS) -> Tester<BS, E> {
    let bundle_root = bundle::import_bundle(&blockstore, actors_v10::BUNDLE_CAR).unwrap();

    Tester::new(NetworkVersion::V18, StateTreeVersion::V5, bundle_root, blockstore.clone()).unwrap()
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
            gas_limit: BLOCK_GAS_LIMIT,
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
        assert!(ret.msg_receipt.exit_code.is_success(), "call failed: {ret:#?}");
        ret
    }

    fn install_actor_with_state<S: Serialize>(
        &mut self,
        code: &[u8],
        actor_id: u64,
        state: S,
    ) -> Address {
        let address = Address::new_id(actor_id);
        let state_cid = self.set_state(&state).unwrap();
        self.set_actor_from_bin(code, state_cid, address, TokenAmount::zero()).unwrap();
        address
    }

    fn install_actor_stateless(&mut self, code: &[u8], actor_id: u64) -> Address {
        let address = Address::new_id(actor_id);
        self.set_actor_from_bin(code, Cid::default(), address, TokenAmount::zero()).unwrap();
        address
    }
}
