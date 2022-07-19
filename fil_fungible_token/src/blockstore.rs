//! Blockstore implementation is borrowed from https://github.com/filecoin-project/builtin-actors/blob/6df845dcdf9872beb6e871205eb34dcc8f7550b5/runtime/src/runtime/actor_blockstore.rs
//! This impl will likely be made redundant if low-level SDKs export blockstore implementations
use std::convert::TryFrom;

use anyhow::{anyhow, Result};
use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_blockstore::Block;
use fvm_sdk::ipld;

/// A blockstore that delegates to IPLD syscalls.
#[derive(Default, Debug, Copy, Clone)]
pub struct Blockstore;

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

// TODO: put this somewhere more appropriate when a tests folder exists
/// An in-memory blockstore impl taken from filecoin-project/ref-fvm
#[derive(Debug, Default, Clone)]
pub struct MemoryBlockstore {
    blocks: RefCell<HashMap<Cid, Vec<u8>>>,
}

use std::{cell::RefCell, collections::HashMap};
impl MemoryBlockstore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl fvm_ipld_blockstore::Blockstore for MemoryBlockstore {
    fn has(&self, k: &Cid) -> Result<bool> {
        Ok(self.blocks.borrow().contains_key(k))
    }

    fn get(&self, k: &Cid) -> Result<Option<Vec<u8>>> {
        Ok(self.blocks.borrow().get(k).cloned())
    }

    fn put_keyed(&self, k: &Cid, block: &[u8]) -> Result<()> {
        self.blocks.borrow_mut().insert(*k, block.into());
        Ok(())
    }
}
