use std::rc::Rc;

use anyhow::Result;
use cid::Cid;
use fvm_ipld_blockstore::MemoryBlockstore;

/// A shared wrapper around [`MemoryBlockstore`].
///
/// Clones of it will reference the same underlying [`MemoryBlockstore`], allowing for more complex
/// unit testing.
#[derive(Debug, Clone)]
pub struct SharedMemoryBlockstore {
    store: Rc<MemoryBlockstore>,
}

impl SharedMemoryBlockstore {
    pub fn new() -> Self {
        Self { store: Rc::new(MemoryBlockstore::new()) }
    }
}

impl Default for SharedMemoryBlockstore {
    fn default() -> Self {
        Self::new()
    }
}

// blockstore implementation, passes calls through to the underlying MemoryBlockstore
impl fvm_ipld_blockstore::Blockstore for SharedMemoryBlockstore {
    /// Gets the block from the blockstore.
    fn get(&self, k: &Cid) -> Result<Option<Vec<u8>>> {
        self.store.get(k)
    }

    /// Put a block with a pre-computed cid.
    ///
    /// If you don't yet know the CID, use put. Some blockstores will re-compute the CID internally
    /// even if you provide it.
    ///
    /// If you _do_ already know the CID, use this method as some blockstores _won't_ recompute it.
    fn put_keyed(&self, k: &Cid, block: &[u8]) -> Result<()> {
        self.store.put_keyed(k, block)
    }

    /// Checks if the blockstore has the specified block.
    fn has(&self, k: &Cid) -> Result<bool> {
        self.store.has(k)
    }
}
