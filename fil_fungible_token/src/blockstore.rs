use anyhow::Result;
use cid::Cid;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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
    use cid::multihash::Code;
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
