use cid::Cid;
use fvm_ipld_encoding::ipld_block::IpldBlock;
use fvm_shared::{address::Address, econ::TokenAmount, error::ErrorNumber, ActorID, MethodNum};
use thiserror::Error;

use crate::messaging::Response;

pub mod fake_syscalls;
pub mod fvm_syscalls;

/// Copied to avoid linking against `fvm_sdk` for non-WASM targets
#[derive(Copy, Clone, Debug, Error)]
#[error("actor does not exist in state-tree")]
pub struct NoStateError;

/// The Syscalls trait defines methods available to the actor from its execution environment.
///
/// The methods available are a subset of the methods exported by `fvm_sdk`
pub trait Syscalls {
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
        params: Option<IpldBlock>,
        value: TokenAmount,
    ) -> Result<Response, ErrorNumber>;

    /// Resolves the ID address of an actor.
    ///
    /// Returns None if the address cannot be resolved. Successfully resolving an address doesn't
    /// necessarily mean the actor exists (e.g., if the addresss was already an actor ID).
    fn resolve_address(&self, addr: &Address) -> Option<ActorID>;
}
