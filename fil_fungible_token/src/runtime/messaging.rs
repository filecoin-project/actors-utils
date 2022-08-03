use std::cell::RefCell;
use std::collections::HashMap;

use fvm_ipld_encoding::Error as IpldError;
use fvm_ipld_encoding::RawBytes;
use fvm_sdk::{actor, send, sys::ErrorNumber};
use fvm_shared::error::ExitCode;
use fvm_shared::receipt::Receipt;
use fvm_shared::METHOD_SEND;
use fvm_shared::{address::Address, econ::TokenAmount, ActorID};
use num_traits::Zero;
use thiserror::Error;

use crate::receiver::types::TokenReceivedParams;

pub type Result<T> = std::result::Result<T, MessagingError>;

#[derive(Error, Debug)]
pub enum MessagingError {
    #[error("fvm syscall error: `{0}`")]
    Syscall(#[from] ErrorNumber),
    #[error("address not initialized: `{0}`")]
    AddressNotInitialized(Address),
    #[error("ipld serialization error: `{0}`")]
    Ipld(#[from] IpldError),
}

/// An abstraction used to send messages to other actors
pub trait Messaging {
    /// Call the receiver hook on a given actor, specifying the amount of tokens pending to be sent
    /// and the sender and receiver
    ///
    /// Returns true if the receiver hook is called and exits without error, else returns false
    fn call_receiver_hook(
        &self,
        from: ActorID,
        to: ActorID,
        token_value: &TokenAmount,
        data: &[u8],
    ) -> Result<Receipt>;

    /// Resolves the given address to its ID address form
    ///
    /// Returns MessagingError::AddressNotInitialised if the address could not be resolved
    fn resolve_id(&self, address: &Address) -> Result<ActorID>;

    /// Creates an account at a pubkey address and returns the ID address
    ///
    /// Returns MessagingError::AddressNotInitialized if the address could not be created
    fn initialize_account(&self, address: &Address) -> Result<ActorID>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FvmMessenger {}

impl Messaging for FvmMessenger {
    fn call_receiver_hook(
        &self,
        from: ActorID,
        to: ActorID,
        token_value: &TokenAmount,
        data: &[u8],
    ) -> Result<Receipt> {
        // TODO: use fvm_dispatch here (when it supports compile time method resolution)
        // TODO: ^^ necessitates determining conventional method names for receiver hooks

        // currently, the method number comes from taking the name as "TokensReceived" and applying
        // the transformation described in https://github.com/filecoin-project/FIPs/pull/399
        const METHOD_NUM: u64 = 1361519036;
        let to = Address::new_id(to);

        let params = TokenReceivedParams {
            sender: Address::new_id(from),
            value: token_value.clone(),
            data: RawBytes::from(data.to_vec()),
        };
        let params = RawBytes::new(fvm_ipld_encoding::to_vec(&params)?);

        Ok(send::send(&to, METHOD_NUM, params, TokenAmount::zero())?)
    }

    fn resolve_id(&self, address: &Address) -> Result<ActorID> {
        actor::resolve_address(address).ok_or(MessagingError::AddressNotInitialized(*address))
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
/// # call_receiver_hook
/// If call_receiver_hook is called with an empty data array, it will return true.
/// If call_receiver_hook is called with a non-empty data array, it will return false.
///
/// # resolve_id
/// If Address is id-address or secp-address, it resolves by returing FAKE_RESOLVED_ID
/// If Address is actor-address or bls-address, it returns an error simulating an uninitialised address
///
/// # initialise_account
/// Simulates the initialisation of an account
/// - ID, SECP addresses panic as they should not be called given that resolve_id gave back an ActorID
/// - BLS addresses give back FAKE_INITIALIZED_ID
/// - Actor addresses are uninitialised and give back an error
#[derive(Debug)]
pub struct FakeMessenger {
    address_resolver: RefCell<FakeAddressResolver>,
}

impl FakeMessenger {
    /// Creates a new FakeMessenger with a given set of initialized accounts
    ///
    /// first_usable_actor_id is the first ActorID that has not been already allocated to an address
    /// i.e. in test fixtures where it may be useful to have statically allocated ID addresses, they
    /// should all have an ActorID strictly below first_usable_actor_id
    pub fn new(first_usable_actor_id: ActorID) -> Self {
        Self { address_resolver: RefCell::new(FakeAddressResolver::new(first_usable_actor_id)) }
    }
}

impl Messaging for FakeMessenger {
    fn call_receiver_hook(
        &self,
        _from: ActorID,
        _to: ActorID,
        _value: &TokenAmount,
        data: &[u8],
    ) -> Result<Receipt> {
        if data.is_empty() {
            Ok(Receipt {
                exit_code: ExitCode::OK,
                return_data: RawBytes::new(data.to_vec()),
                gas_used: 0,
            })
        } else {
            Ok(Receipt {
                exit_code: ExitCode::SYS_INVALID_RECEIVER,
                return_data: RawBytes::new(data.to_vec()),
                gas_used: 0,
            })
        }
    }

    fn resolve_id(&self, address: &Address) -> Result<ActorID> {
        self.address_resolver.borrow().resolve_id(address)
    }

    fn initialize_account(&self, address: &Address) -> Result<ActorID> {
        self.address_resolver.borrow_mut().initialize_account(address)
    }
}

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
            _ => Err(MessagingError::AddressNotInitialized(*address)),
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

    use crate::runtime::messaging::Messaging;

    use super::FakeMessenger;

    /// Simple test checking that the fake messenger uses the address resolver to resolve addresses
    /// The resolution of addresses is tested in the test_address_resolver module
    #[test]
    fn it_resolves_addresses_with_fake_address_resolver() {
        let m = FakeMessenger::new(1);
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
