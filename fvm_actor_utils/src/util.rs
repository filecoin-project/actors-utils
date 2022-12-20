use cid::Cid;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_blockstore::MemoryBlockstore;
use fvm_ipld_encoding::ipld_block::IpldBlock;
use fvm_shared::receipt::Receipt;
use fvm_shared::MethodNum;
use fvm_shared::METHOD_SEND;
use fvm_shared::{address::Address, econ::TokenAmount, ActorID};
use num_traits::Zero;
use thiserror::Error;

use crate::messaging::{Messaging, MessagingError, Result as MessagingResult};
use crate::runtime::FvmRuntime;
use crate::runtime::NoStateError;
use crate::runtime::Runtime;
use crate::runtime::TestRuntime;

#[derive(Error, Clone, Debug)]
pub enum ActorError {
    #[error("root state not found {0}")]
    NoState(#[from] NoStateError),
}

type ActorResult<T> = std::result::Result<T, ActorError>;

/// ActorHelper contains utils to help access the underlying execution environment (runtime and blockstore)
#[derive(Clone, Debug)]
pub struct ActorHelper<R: Runtime, BS: Blockstore> {
    pub runtime: R,
    pub blockstore: BS,
}

impl<R: Runtime, BS: Blockstore> ActorHelper<R, BS> {
    pub fn new_test_helper() -> ActorHelper<TestRuntime, MemoryBlockstore> {
        ActorHelper { runtime: TestRuntime::default(), blockstore: MemoryBlockstore::default() }
    }

    pub fn new_fvm_helper() -> ActorHelper<FvmRuntime, crate::blockstore::Blockstore> {
        ActorHelper {
            runtime: FvmRuntime::default(),
            blockstore: crate::blockstore::Blockstore::default(),
        }
    }

    /// Returns the address of the current actor as an ActorID
    pub fn actor_id(&self) -> ActorID {
        self.runtime.receiver()
    }

    /// Sends a message to an actor
    pub fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: Option<IpldBlock>,
        value: TokenAmount,
    ) -> MessagingResult<Receipt> {
        Ok(self.runtime.send(to, method, params, value)?)
    }

    /// Attempts to resolve the given address to its ID address form
    ///
    /// Returns MessagingError::AddressNotResolved if the address could not be resolved
    pub fn resolve_id(&self, address: &Address) -> MessagingResult<ActorID> {
        self.runtime.resolve_address(address).ok_or(MessagingError::AddressNotResolved(*address))
    }

    /// Resolves an address to an ID address, sending a message to initialize an account there if
    /// it doesn't exist
    ///
    /// If the account cannot be created, this function returns MessagingError::AddressNotInitialized
    pub fn resolve_or_init(&self, address: &Address) -> MessagingResult<ActorID> {
        let id = match self.resolve_id(address) {
            Ok(addr) => addr,
            Err(MessagingError::AddressNotResolved(_e)) => self.initialize_account(address)?,
            Err(e) => return Err(e),
        };
        Ok(id)
    }

    pub fn initialize_account(&self, address: &Address) -> MessagingResult<ActorID> {
        self.send(address, METHOD_SEND, Default::default(), TokenAmount::zero())?;
        match self.resolve_id(address) {
            Ok(id) => Ok(id),
            Err(MessagingError::AddressNotResolved(e)) => {
                // if we can't resolve after the send, then the account was not initialized
                Err(MessagingError::AddressNotInitialized(e))
            }
            Err(e) => Err(e),
        }
    }

    /// Get the root cid of the actor's state
    pub fn root_cid(&self) -> ActorResult<Cid> {
        Ok(self.runtime.root().map_err(|_err| NoStateError)?)
    }

    /// Attempts to compare two addresses, seeing if they would resolve to the same Actor without
    /// actually instantiating accounts for them
    ///
    /// If a and b are of the same type, simply do an equality check. Otherwise, attempt to resolve
    /// to an ActorID and compare
    pub fn same_address(&self, address_a: &Address, address_b: &Address) -> bool {
        let protocol_a = address_a.protocol();
        let protocol_b = address_b.protocol();
        if protocol_a == protocol_b {
            address_a == address_b
        } else {
            // attempt to resolve both to ActorID
            let id_a = match self.resolve_id(address_a) {
                Ok(id) => id,
                Err(_) => return false,
            };
            let id_b = match self.resolve_id(address_b) {
                Ok(id) => id,
                Err(_) => return false,
            };
            id_a == id_b
        }
    }

    pub fn bs(&self) -> &BS {
        &self.blockstore
    }
}

/// Convenience impl encapsulating the blockstore functionality
impl<R: Runtime, BS: Blockstore> Blockstore for ActorHelper<R, BS> {
    fn get(&self, k: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        self.blockstore.get(k)
    }

    fn put_keyed(&self, k: &Cid, block: &[u8]) -> anyhow::Result<()> {
        self.blockstore.put_keyed(k, block)
    }
}

impl<R: Runtime, BS: Blockstore> Messaging for ActorHelper<R, BS> {
    fn send(
        &self,
        to: &Address,
        method: fvm_shared::MethodNum,
        params: Option<IpldBlock>,
        value: &fvm_shared::econ::TokenAmount,
    ) -> crate::messaging::Result<Receipt> {
        let res = self.runtime.send(to, method, params, value.clone());
        Ok(res?)
    }
}
