use std::cell::RefCell;
use std::collections::HashMap;

use frc42_dispatch::method_hash;
use fvm_ipld_encoding::Error as IpldError;
use fvm_ipld_encoding::RawBytes;
use fvm_sdk::{actor, message, send, sys::ErrorNumber};
use fvm_shared::error::ExitCode;
use fvm_shared::receipt::Receipt;
use fvm_shared::MethodNum;
use fvm_shared::METHOD_SEND;
use fvm_shared::{address::Address, econ::TokenAmount, ActorID};
use num_traits::Zero;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, MessagingError>;

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

/// An abstraction used to send messages to other actors
pub trait Messaging {
    /// Returns the address of the current actor as an ActorID
    fn actor_id(&self) -> ActorID;

    /// Sends a message to an actor
    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: &RawBytes,
        value: &TokenAmount,
    ) -> Result<Receipt>;

    /// Attempts to resolve the given address to its ID address form
    ///
    /// Returns MessagingError::AddressNotResolved if the address could not be resolved
    fn resolve_id(&self, address: &Address) -> Result<ActorID>;

    /// Creates an account at a pubkey address and returns the ID address
    ///
    /// Returns MessagingError::AddressNotInitialized if the address could not be created
    fn initialize_account(&self, address: &Address) -> Result<ActorID>;

    /// Resolves an address to an ID address, sending a message to initialize an account there if
    /// it doesn't exist
    ///
    /// If the account cannot be created, this function returns MessagingError::AddressNotInitialized
    fn resolve_or_init(&self, address: &Address) -> Result<ActorID> {
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
}

/// This method number comes from taking the name as "Receive" and applying
/// the transformation described in [FRC-0042](https://github.com/filecoin-project/FIPs/blob/master/FRCs/frc-0042.md)
pub const RECEIVER_HOOK_METHOD_NUM: u64 = method_hash!("Receive");

#[derive(Debug, Default, Clone, Copy)]
pub struct FvmMessenger {}

impl Messaging for FvmMessenger {
    fn actor_id(&self) -> ActorID {
        message::receiver()
    }

    fn send(
        &self,
        to: &Address,
        method: MethodNum,
        params: &RawBytes,
        value: &TokenAmount,
    ) -> Result<Receipt> {
        Ok(send::send(to, method, params.clone(), value.clone())?)
    }

    fn resolve_id(&self, address: &Address) -> Result<ActorID> {
        actor::resolve_address(address).ok_or(MessagingError::AddressNotResolved(*address))
    }

    fn initialize_account(&self, address: &Address) -> Result<ActorID> {
        if let Err(e) = send::send(address, METHOD_SEND, Default::default(), TokenAmount::zero()) {
            return Err(e.into());
        }

        actor::resolve_address(address).ok_or(MessagingError::AddressNotInitialized(*address))
    }
}

/// A fake method caller
///
#[derive(Debug)]
pub struct FakeMessenger {
    pub last_message: RefCell<Option<RawBytes>>,
    address_resolver: RefCell<FakeAddressResolver>,
    actor_id: ActorID,
    abort_next_send: RefCell<bool>,
}

/// A mocked messenger that can be used to interact with other Actors
///
/// Can be used to test behaviour when other Actors abort when handling messages
impl FakeMessenger {
    /// Creates a new FakeMessenger with a given set of initialized accounts
    ///
    /// first_usable_actor_id is the first ActorID that has not been already allocated to an address
    /// i.e. in test fixtures where it may be useful to have statically allocated ID addresses, they
    /// should all have an ActorID strictly below first_usable_actor_id
    pub fn new(actor_id: ActorID, first_usable_actor_id: ActorID) -> Self {
        Self {
            actor_id,
            address_resolver: RefCell::new(FakeAddressResolver::new(first_usable_actor_id)),
            last_message: Default::default(),
            abort_next_send: RefCell::new(false),
        }
    }

    pub fn abort_next_send(&mut self) {
        self.abort_next_send.replace(true);
    }
}

impl Messaging for FakeMessenger {
    fn actor_id(&self) -> ActorID {
        self.actor_id
    }

    fn send(
        &self,
        _to: &Address,
        _method: MethodNum,
        params: &RawBytes,
        _value: &TokenAmount,
    ) -> Result<Receipt> {
        self.last_message.borrow_mut().replace(params.clone());

        if *self.abort_next_send.borrow() {
            self.abort_next_send.replace(false);
            return Ok(Receipt {
                exit_code: ExitCode::USR_UNSPECIFIED,
                gas_used: 0,
                return_data: Default::default(),
            });
        }

        Ok(Receipt { exit_code: ExitCode::OK, return_data: Default::default(), gas_used: 0 })
    }

    fn resolve_id(&self, address: &Address) -> Result<ActorID> {
        self.address_resolver.borrow().resolve_id(address)
    }

    fn initialize_account(&self, address: &Address) -> Result<ActorID> {
        self.address_resolver.borrow_mut().initialize_account(address)
    }
}

/// A fake address resolver that keeps track of addresses that keeps track of which addresses have
/// been initialised and their corresponding IDs
#[derive(Debug)]
pub struct FakeAddressResolver {
    next_actor_id: ActorID,
    initialized_accounts: HashMap<Address, ActorID>,
}

impl FakeAddressResolver {
    pub fn new(next_actor_id: ActorID) -> Self {
        Self { next_actor_id, initialized_accounts: HashMap::new() }
    }

    pub fn initialize_account(&mut self, address: &Address) -> Result<ActorID> {
        match address.payload() {
            fvm_shared::address::Payload::ID(id) => {
                panic!("attempting to initialise an already resolved id {}", id)
            }
            fvm_shared::address::Payload::Secp256k1(_) => Ok(self._initialize_address(address)?),
            fvm_shared::address::Payload::BLS(_) => Ok(self._initialize_address(address)?),
            fvm_shared::address::Payload::Actor(_) => {
                Err(MessagingError::AddressNotInitialized(*address))
            }
        }
    }

    pub fn resolve_id(&self, address: &Address) -> Result<ActorID> {
        // return an initialised address if it has been initialized before
        if self.initialized_accounts.contains_key(address) {
            return Ok(self.initialized_accounts[address]);
        }

        // else resolve it if it is an id address
        match address.payload() {
            fvm_shared::address::Payload::ID(id) => Ok(*id),
            _ => Err(MessagingError::AddressNotResolved(*address)),
        }
    }

    fn _initialize_address(&mut self, address: &Address) -> Result<ActorID> {
        let actor_id = self.next_actor_id;
        self.next_actor_id += 1;
        self.initialized_accounts.insert(*address, actor_id);
        Ok(actor_id)
    }
}

#[cfg(test)]
mod test_address_resolver {
    use fvm_shared::address::{Address, BLS_PUB_LEN};

    use super::FakeAddressResolver;

    /// Returns a static secp256k1 address
    fn secp_address(id: u8) -> Address {
        let key = vec![id; 65];
        Address::new_secp256k1(key.as_slice()).unwrap()
    }

    /// Returns a static BLS address
    fn bls_address(id: u8) -> Address {
        let key = vec![id; BLS_PUB_LEN];
        Address::new_bls(key.as_slice()).unwrap()
    }

    // Returns a new Actor address, that is uninitializable by the FakeMessenger
    fn actor_address(id: u8) -> Address {
        Address::new_actor(&[id])
    }

    #[test]
    fn it_creates_incrementing_addresses() {
        let mut ar = FakeAddressResolver::new(1);
        let secp_1 = &secp_address(1);
        let secp_2 = &secp_address(2);
        let bls_1 = &bls_address(1);
        let bls_2 = &bls_address(2);
        let actor_1 = &actor_address(1);

        // none resolvable initially
        ar.resolve_id(secp_1).unwrap_err();
        ar.resolve_id(secp_2).unwrap_err();
        ar.resolve_id(bls_1).unwrap_err();
        ar.resolve_id(bls_2).unwrap_err();
        ar.resolve_id(actor_1).unwrap_err();

        // creates new actor ids
        assert_eq!(ar.initialize_account(secp_1).unwrap(), 1);
        assert_eq!(ar.initialize_account(secp_2).unwrap(), 2);
        assert_eq!(ar.initialize_account(bls_1).unwrap(), 3);
        assert_eq!(ar.initialize_account(bls_2).unwrap(), 4);

        // cannot assign actor id to an account address
        ar.initialize_account(actor_1).unwrap_err();
    }

    #[test]
    fn it_retrieves_initialised_addresses() {
        let mut ar = FakeAddressResolver::new(1);
        let secp_1 = &secp_address(1);

        // cannot initially resolve
        ar.resolve_id(secp_1).unwrap_err();

        // initialize it
        ar.initialize_account(secp_1).unwrap();

        // resolves now
        assert_eq!(ar.resolve_id(secp_1).unwrap(), 1);
    }

    #[test]
    fn it_doesnt_collide_with_reserved_address_space() {
        let mut ar = FakeAddressResolver::new(10);
        let secp_1 = &secp_address(1);

        // cannot initially resolve
        ar.resolve_id(secp_1).unwrap_err();

        // initialize it
        ar.initialize_account(secp_1).unwrap();

        // resolves now
        assert_eq!(ar.resolve_id(secp_1).unwrap(), 10);
    }

    #[test]
    fn it_resolves_id_addresses() {
        let ar = FakeAddressResolver::new(10);
        let id_address = &Address::new_id(4);

        // cannot initially resolve
        assert_eq!(ar.resolve_id(id_address).unwrap(), 4);
    }
}

#[cfg(test)]
mod test_fake_messenger {
    /// Returns a static secp256k1 address
    fn secp_address(id: u8) -> Address {
        let key = vec![id; 65];
        Address::new_secp256k1(key.as_slice()).unwrap()
    }

    /// Returns a static BLS address
    fn bls_address(id: u8) -> Address {
        let key = vec![id; BLS_PUB_LEN];
        Address::new_bls(key.as_slice()).unwrap()
    }

    // Returns a new Actor address, that is uninitializable by the FakeMessenger
    fn actor_address(id: u8) -> Address {
        Address::new_actor(&[id])
    }
    use fvm_shared::address::{Address, BLS_PUB_LEN};

    use crate::messaging::{FakeMessenger, Messaging};

    /// Simple test checking that the fake messenger uses the address resolver to resolve addresses
    /// The resolution of addresses is tested in the test_address_resolver module
    #[test]
    fn it_resolves_addresses_with_fake_address_resolver() {
        let m = FakeMessenger::new(0, 1);
        let secp_1 = &secp_address(1);
        let secp_2 = &secp_address(2);
        let bls_1 = &bls_address(1);
        let bls_2 = &bls_address(2);
        let actor_1 = &actor_address(1);

        // none resolvable initially
        m.resolve_id(secp_1).unwrap_err();
        m.resolve_id(secp_2).unwrap_err();
        m.resolve_id(bls_1).unwrap_err();
        m.resolve_id(bls_2).unwrap_err();
        m.resolve_id(actor_1).unwrap_err();

        // creates new actor ids
        assert_eq!(m.initialize_account(secp_1).unwrap(), 1);
        assert_eq!(m.initialize_account(secp_2).unwrap(), 2);
        assert_eq!(m.initialize_account(bls_1).unwrap(), 3);
        assert_eq!(m.initialize_account(bls_2).unwrap(), 4);

        // cannot assign actor id to an account address
        m.initialize_account(actor_1).unwrap_err();

        assert_eq!(m.resolve_id(&Address::new_id(1)).unwrap(), 1);
    }
}
