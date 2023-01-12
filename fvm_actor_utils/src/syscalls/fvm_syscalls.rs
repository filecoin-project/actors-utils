use anyhow::Result;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::ipld_block::IpldBlock;
use fvm_sdk;
use fvm_shared::response::Response;
use fvm_shared::{address::Address, MethodNum};

use super::Syscalls;
use crate::util::ActorRuntime;

/// Runtime that delegates to fvm_sdk allowing actors to be deployed on-chain
#[derive(Default, Debug, Clone, Copy)]
pub struct FvmSyscalls {}

impl Syscalls for FvmSyscalls {
    fn root(&self) -> Result<cid::Cid, super::NoStateError> {
        fvm_sdk::sself::root().map_err(|_| super::NoStateError)
    }

    fn receiver(&self) -> fvm_shared::ActorID {
        fvm_sdk::message::receiver()
    }

    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: Option<IpldBlock>,
        value: fvm_shared::econ::TokenAmount,
    ) -> fvm_sdk::SyscallResult<Response> {
        fvm_sdk::send::send(to, method, params, value)
    }

    fn resolve_address(&self, addr: &Address) -> Option<fvm_shared::ActorID> {
        fvm_sdk::actor::resolve_address(addr)
    }
}

impl<S: Syscalls, BS: Blockstore> ActorRuntime<S, BS> {
    pub fn new_fvm_runtime() -> ActorRuntime<FvmSyscalls, crate::blockstore::Blockstore> {
        ActorRuntime {
            syscalls: FvmSyscalls::default(),
            blockstore: crate::blockstore::Blockstore::default(),
        }
    }
}
