use super::Runtime;

use anyhow::{anyhow, Result};
use fvm_sdk as sdk;
use sdk::actor;
use sdk::message;

/// Provides access to the FVM which acts as the runtime for actors deployed on-chain
pub struct FvmRuntime {}

impl Runtime for FvmRuntime {
    fn caller(&self) -> u64 {
        message::caller()
    }

    fn resolve_address(&self, addr: &fvm_shared::address::Address) -> Result<u64> {
        actor::resolve_address(addr).ok_or_else(|| anyhow!("Failed to resolve address"))
    }
}
