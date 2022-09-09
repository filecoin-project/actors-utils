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
use fvm_ipld_encoding::RawBytes;
use fvm_shared::address::Address;
use fvm_shared::bigint::Zero;
use fvm_shared::econ::TokenAmount;
use fvm_shared::message::Message;
use fvm_shared::state::StateTreeVersion;
use fvm_shared::version::NetworkVersion;
use serde::{Deserialize, Serialize};

pub fn load_actor_wasm(path: &str) -> Vec<u8> {
    let wasm_path = env::current_dir().unwrap().join(path).canonicalize().unwrap();

    std::fs::read(wasm_path).expect("unable to read actor file")
}

pub trait TestHelpers {
    /// Call a method on an actor
    fn call_method(
        &mut self,
        from: Address,
        to: Address,
        method_num: u64,
        params: Option<RawBytes>,
    ) -> ApplyRet;

    /// Get balance from token actor for a given address
    /// This is a very common thing to check during tests
    fn get_balance(
        &mut self,
        operator: Address,
        token_actor: Address,
        target: Address,
    ) -> TokenAmount;

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

    fn get_balance(
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
