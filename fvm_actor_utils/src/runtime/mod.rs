use cid::Cid;
use fvm_ipld_encoding::RawBytes;
use fvm_sdk::{error::NoStateError, SyscallResult};
use fvm_shared::{address::Address, econ::TokenAmount, receipt::Receipt, ActorID, MethodNum};

mod fvm_runtime;
mod test_runtime;

pub use fvm_runtime::FvmRuntime;

/// Runtime is the abstract interface that an FVM actor uses to interact with the rest of the system
///
/// The methods available on runtime are a subset of the methods exported by `fvm_sdk`
pub trait Runtime {
    /// Get the IPLD root CID. Fails if the actor doesn't have state (before the first call to
    /// `set_root` and after actor deletion).
    fn root(&self) -> Result<Cid, NoStateError>;

    /// Returns the ID address of the actor
    fn receiver(&self) -> ActorID;

    /// Sends a message to an actor
    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: RawBytes,
        value: TokenAmount,
    ) -> SyscallResult<Receipt>;

    /// Resolves the ID address of an actor.
    ///
    /// Returns None if the address cannot be resolved. Successfully resolving an address doesn't
    /// necessarily mean the actor exists (e.g., if the addresss was already an actor ID).
    fn resolve_address(&self, addr: &Address) -> Option<ActorID>;
}
