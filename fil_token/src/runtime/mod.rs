mod fvm;
pub use fvm::*;

use anyhow::Result;
use fvm_shared::address::Address;

/// Abstraction of the runtime that an actor is executed in, providing access to syscalls and
/// features of the FVM
pub trait Runtime {
    /// Get the direct-caller that invoked the current actor
    fn caller(&self) -> u64;

    /// Attempts to resolve an address to an ActorID
    fn resolve_address(&self, addr: &Address) -> Result<u64>;
}
