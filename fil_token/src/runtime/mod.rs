use anyhow::Result;
use fvm_shared::address::Address;

pub trait Runtime {
    fn caller(&self) -> u64;

    fn resolve_address(&self, addr: &Address) -> Result<u64>;
}
