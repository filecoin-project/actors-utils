use anyhow::{anyhow, Result};
use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_blockstore::Block;
use fvm_sdk::ipld;
use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::rc::Rc;

/// A blockstore that delegates to IPLD syscalls.
#[derive(Default, Debug, Copy, Clone)]
pub struct Blockstore;

/// Blockstore implementation is borrowed from https://github.com/filecoin-project/builtin-actors/blob/6df845dcdf9872beb6e871205eb34dcc8f7550b5/runtime/src/runtime/actor_blockstore.rs
/// This impl will likely be made redundant if low-level SDKs export blockstore implementations
impl fvm_ipld_blockstore::Blockstore for Blockstore {
    fn get(&self, cid: &Cid) -> Result<Option<Vec<u8>>> {
        // If this fails, the _CID_ is invalid. I.e., we have a bug.
        ipld::get(cid)
            .map(Some)
            .map_err(|e| anyhow!("get failed with {:?} on CID '{}'", e, cid))
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

/// An in-memory blockstore impl that shares underlying memory when cloned
///
/// This is useful in tests to simulate a blockstore which pipes syscalls to the fvm_ipld_blockstore
#[derive(Debug, Default, Clone)]
pub struct SharedMemoryBlockstore {
    blocks: Rc<RefCell<HashMap<Cid, Vec<u8>>>>,
}

impl SharedMemoryBlockstore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl fvm_ipld_blockstore::Blockstore for SharedMemoryBlockstore {
    fn has(&self, k: &Cid) -> Result<bool> {
        Ok(RefCell::borrow(&self.blocks).contains_key(k))
    }

    fn get(&self, k: &Cid) -> Result<Option<Vec<u8>>> {
        Ok(RefCell::borrow(&self.blocks).get(k).cloned())
    }

    fn put_keyed(&self, k: &Cid, block: &[u8]) -> Result<()> {
        RefCell::borrow_mut(&self.blocks).insert(*k, block.into());
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use fvm_ipld_blockstore::Blockstore;
    use fvm_ipld_encoding::CborStore;
    use fvm_shared::bigint::{bigint_ser::BigIntDe, BigInt};

    use super::*;

    #[test]
    fn it_shares_memory_under_clone() {
        let bs = SharedMemoryBlockstore::new();
        let a_number = BigIntDe(BigInt::from(123));
        let cid = bs.put_cbor(&a_number, Code::Blake2b256).unwrap();

        let bs_cloned = bs.clone();
        assert_eq!(bs.blocks, bs_cloned.blocks);
        assert_eq!(bs.get(&cid).unwrap(), bs_cloned.get(&cid).unwrap())
    }
}
