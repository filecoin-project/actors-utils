use std::{cell::RefCell, collections::HashMap};

use cid::Cid;
use fvm_ipld_encoding::{ipld_block::IpldBlock, RawBytes};
use fvm_shared::{
    address::Address, econ::TokenAmount, error::ErrorNumber, error::ExitCode, receipt::Receipt,
    ActorID,
};

use super::Syscalls;

#[derive(Clone, Default, Debug)]
pub struct TestMessage {
    pub method: u64,
    pub params: Option<IpldBlock>,
    pub value: TokenAmount,
}

#[derive(Clone, Default, Debug)]
pub struct FakeSyscalls {
    /// The root of the calling actor
    pub root: Cid,
    /// The f0 ID of the calling actor
    pub actor_id: ActorID,

    /// A map of addresses that were instantiated in this runtime
    pub addresses: RefCell<HashMap<Address, ActorID>>,
    /// The next-to-allocate f0 address
    pub next_actor_id: RefCell<ActorID>,

    /// The last message sent via this runtime
    pub last_message: RefCell<Option<TestMessage>>,
    /// Flag to control message success
    pub abort_next_send: RefCell<bool>,
}

impl Syscalls for FakeSyscalls {
    fn root(&self) -> Result<Cid, super::NoStateError> {
        Ok(self.root)
    }

    fn receiver(&self) -> fvm_shared::ActorID {
        self.actor_id
    }

    fn send(
        &self,
        to: &fvm_shared::address::Address,
        method: fvm_shared::MethodNum,
        params: Option<fvm_ipld_encoding::ipld_block::IpldBlock>,
        value: fvm_shared::econ::TokenAmount,
    ) -> Result<Receipt, ErrorNumber> {
        if *self.abort_next_send.borrow() {
            self.abort_next_send.replace(false);
            Err(ErrorNumber::AssertionFailed)
        } else {
            // sending to an address instantiates it if it isn't already
            let mut map = self.addresses.borrow_mut();

            match to.payload() {
                // TODO: in a real system, this is fallible if the address does not exist
                // This impl assumes that any f0 form address is in the map/instantiated but does not check so
                // Sending to actors should succeed if the actor exists but not instantiate it
                fvm_shared::address::Payload::ID(_) | fvm_shared::address::Payload::Actor(_) => {
                    Ok(())
                }
                // Sending to public keys should instantiate the actor
                fvm_shared::address::Payload::Secp256k1(_)
                | fvm_shared::address::Payload::BLS(_) => {
                    if !map.contains_key(to) {
                        let actor_id = self.next_actor_id.replace_with(|old| *old + 1);
                        map.insert(*to, actor_id);
                    }
                    Ok(())
                }
            }?;

            // save the fake message as being sent
            let message = TestMessage { method, params: params.clone(), value };
            self.last_message.replace(Some(message));

            // derive fake return data, echoing the params
            let return_data = params.map(|b| RawBytes::from(b.data)).unwrap_or(RawBytes::default());

            Ok(Receipt { exit_code: ExitCode::OK, return_data, gas_used: 0 })
        }
    }

    fn resolve_address(&self, addr: &Address) -> Option<ActorID> {
        // if it is already an ID-address, just return it
        if let fvm_shared::address::Payload::ID(id) = addr.payload() {
            return Some(*id);
        }

        let map = self.addresses.borrow();
        map.get(addr).copied()
    }
}
