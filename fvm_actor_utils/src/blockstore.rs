use anyhow::anyhow;
use anyhow::Result;
use cid::Cid;
use fvm_ipld_blockstore::Block;
use fvm_sdk::ipld;
use multihash_codetable::Code;

/// A blockstore that delegates to IPLD syscalls.
#[derive(Default, Debug, Copy, Clone)]
pub struct Blockstore;

/// Blockstore implementation is borrowed from [the builtin actors][source]. This impl will likely
/// be made redundant if low-level SDKs export blockstore implementations.
///
/// [source]: https://github.com/filecoin-project/builtin-actors/blob/6df845dcdf9872beb6e871205eb34dcc8f7550b5/runtime/src/runtime/actor_blockstore.rs
impl fvm_ipld_blockstore::Blockstore for Blockstore {
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
