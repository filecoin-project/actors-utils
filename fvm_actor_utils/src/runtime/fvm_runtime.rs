use anyhow::anyhow;
use anyhow::Result;
use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_blockstore::Block;
use fvm_ipld_encoding::RawBytes;
use fvm_sdk;
use fvm_sdk::ipld;
use fvm_shared::{address::Address, MethodNum};

use super::Runtime;

/// Runtime that delegates to fvm_sdk allowing actors to be deployed on-chain
#[derive(Default, Debug, Clone)]
pub struct FvmRuntime {}

impl Runtime for FvmRuntime {
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
        params: RawBytes,
        value: fvm_shared::econ::TokenAmount,
    ) -> fvm_sdk::SyscallResult<fvm_shared::receipt::Receipt> {
        fvm_sdk::send::send(to, method, params, value)
    }

    fn resolve_address(&self, addr: &Address) -> Option<fvm_shared::ActorID> {
        fvm_sdk::actor::resolve_address(addr)
    }
}

impl fvm_ipld_blockstore::Blockstore for FvmRuntime {
    fn get(&self, cid: &Cid) -> Result<Option<Vec<u8>>> {
        // If this fails, the _CID_ is invalid. I.e., we have a bug.
        ipld::get(cid).map(Some).map_err(|e| anyhow!("get failed with {:?} on CID '{}'", e, cid))
    }

    fn put_keyed(&self, k: &Cid, block: &[u8]) -> Result<()> {
        let code = Code::try_from(k.hash().code()).map_err(|e| anyhow!(e.to_string()))?;
        let k2 = self.put(code, &Block::new(k.codec(), block))?;
        if k != &k2 {
            return Err(anyhow!("put block with cid {} but has cid {}", k, k2));
        }
        Ok(())
    }

    fn put<D>(&self, code: Code, block: &Block<D>) -> Result<Cid>
    where
        D: AsRef<[u8]>,
    {
        // TODO: Don't hard-code the size. Unfortunately, there's no good way to get it from the
        //  codec at the moment.
        const SIZE: u32 = 32;
        let k = ipld::put(code.into(), SIZE, block.codec, block.data.as_ref())
            .map_err(|e| anyhow!("put failed with {:?}", e))?;
        Ok(k)
    }
}
