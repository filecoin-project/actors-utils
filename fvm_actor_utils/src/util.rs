use anyhow::anyhow;
use cid::multihash::Code;
use cid::Cid;
use fvm_ipld_blockstore::Block;
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::Error as IpldError;
use fvm_ipld_encoding::RawBytes;
use fvm_sdk::error::NoStateError;
use fvm_sdk::{send, sys::ErrorNumber};
use fvm_shared::error::ExitCode;
use fvm_shared::receipt::Receipt;
use fvm_shared::MethodNum;
use fvm_shared::METHOD_SEND;
use fvm_shared::{address::Address, econ::TokenAmount, ActorID};
use num_traits::Zero;
use thiserror::Error;

use crate::runtime;
use crate::runtime::Runtime;

mod test_util;
pub use test_util::TestActor;

pub type MessagingResult<T> = std::result::Result<T, MessagingError>;

#[derive(Error, Debug)]
pub enum MessagingError {
    #[error("fvm syscall error: `{0}`")]
    Syscall(#[from] ErrorNumber),
    #[error("address could not be resolved: `{0}`")]
    AddressNotResolved(Address),
    #[error("address could not be initialized: `{0}`")]
    AddressNotInitialized(Address),
    #[error("ipld serialization error: `{0}`")]
    Ipld(#[from] IpldError),
}

impl From<&MessagingError> for ExitCode {
    fn from(error: &MessagingError) -> Self {
        match error {
            MessagingError::Syscall(e) => match e {
                ErrorNumber::IllegalArgument => ExitCode::USR_ILLEGAL_ARGUMENT,
                ErrorNumber::Forbidden | ErrorNumber::IllegalOperation => ExitCode::USR_FORBIDDEN,
                ErrorNumber::AssertionFailed => ExitCode::USR_ASSERTION_FAILED,
                ErrorNumber::InsufficientFunds => ExitCode::USR_INSUFFICIENT_FUNDS,
                ErrorNumber::IllegalCid | ErrorNumber::NotFound | ErrorNumber::InvalidHandle => {
                    ExitCode::USR_NOT_FOUND
                }
                ErrorNumber::Serialization | ErrorNumber::IllegalCodec => {
                    ExitCode::USR_SERIALIZATION
                }
                _ => ExitCode::USR_UNSPECIFIED,
            },
            MessagingError::AddressNotResolved(_) | MessagingError::AddressNotInitialized(_) => {
                ExitCode::USR_NOT_FOUND
            }
            MessagingError::Ipld(_) => ExitCode::USR_SERIALIZATION,
        }
    }
}

#[derive(Error, Clone, Debug)]
pub enum ActorError {
    #[error("root state not found {0}")]
    NoState(#[from] NoStateError),
}

type ActorResult<T> = std::result::Result<T, ActorError>;

pub trait Actor {
    /// Returns the address of the current actor as an ActorID
    fn actor_id(&self) -> ActorID;

    /// Sends a message to an actor
    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: RawBytes,
        value: TokenAmount,
    ) -> MessagingResult<Receipt>;

    /// Attempts to resolve the given address to its ID address form
    ///
    /// Returns MessagingError::AddressNotResolved if the address could not be resolved
    fn resolve_id(&self, address: &Address) -> MessagingResult<ActorID>;

    /// Creates an account at a pubkey address and returns the ID address
    ///
    /// Returns MessagingError::AddressNotInitialized if the address could not be created
    fn initialize_account(&self, address: &Address) -> MessagingResult<ActorID>;

    /// Resolves an address to an ID address, sending a message to initialize an account there if
    /// it doesn't exist
    ///
    /// If the account cannot be created, this function returns MessagingError::AddressNotInitialized
    fn resolve_or_init(&self, address: &Address) -> MessagingResult<ActorID> {
        let id = match self.resolve_id(address) {
            Ok(addr) => addr,
            Err(MessagingError::AddressNotResolved(_e)) => self.initialize_account(address)?,
            Err(e) => return Err(e),
        };
        Ok(id)
    }

    /// Attempts to compare two addresses, seeing if they would resolve to the same Actor without
    /// actually instantiating accounts for them
    ///
    /// If a and b are of the same type, simply do an equality check. Otherwise, attempt to resolve
    /// to an ActorID and compare
    fn same_address(&self, address_a: &Address, address_b: &Address) -> bool {
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

    /// Get the root cid of the actor's state
    fn root_cid(&self) -> ActorResult<Cid>;
}

#[derive(Default, Debug, Clone)]
pub struct FvmActor {
    fvm_runtime: runtime::FvmRuntime,
}

impl Actor for FvmActor {
    fn actor_id(&self) -> ActorID {
        self.fvm_runtime.receiver()
    }

    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: RawBytes,
        value: TokenAmount,
    ) -> MessagingResult<Receipt> {
        Ok(self.fvm_runtime.send(to, method, params, value)?)
    }

    fn resolve_id(&self, address: &Address) -> MessagingResult<ActorID> {
        self.fvm_runtime
            .resolve_address(address)
            .ok_or(MessagingError::AddressNotResolved(*address))
    }

    fn initialize_account(&self, address: &Address) -> MessagingResult<ActorID> {
        if let Err(e) = send::send(address, METHOD_SEND, Default::default(), TokenAmount::zero()) {
            return Err(e.into());
        }

        self.resolve_id(address)
    }

    fn root_cid(&self) -> ActorResult<Cid> {
        Ok(self.fvm_runtime.root()?)
    }
}

impl Blockstore for FvmActor {
    fn get(&self, cid: &Cid) -> anyhow::Result<Option<Vec<u8>>> {
        // If this fails, the _CID_ is invalid. I.e., we have a bug.
        fvm_sdk::ipld::get(cid)
            .map(Some)
            .map_err(|e| anyhow!("get failed with {:?} on CID '{}'", e, cid))
    }

    fn put_keyed(&self, k: &Cid, block: &[u8]) -> anyhow::Result<()> {
        let code = Code::try_from(k.hash().code()).map_err(|e| anyhow!(e.to_string()))?;
        let k2 = self.put(code, &Block::new(k.codec(), block))?;
        if k != &k2 {
            return Err(anyhow!("put block with cid {} but has cid {}", k, k2));
        }
        Ok(())
    }

    fn put<D>(&self, code: Code, block: &Block<D>) -> anyhow::Result<Cid>
    where
        D: AsRef<[u8]>,
    {
        // TODO: Don't hard-code the size. Unfortunately, there's no good way to get it from the
        //  codec at the moment.
        const SIZE: u32 = 32;
        let k = fvm_sdk::ipld::put(code.into(), SIZE, block.codec, block.data.as_ref())
            .map_err(|e| anyhow!("put failed with {:?}", e))?;
        Ok(k)
    }
}
