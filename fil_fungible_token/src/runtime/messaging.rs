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

type Result<T> = std::result::Result<T, MessagingError>;

#[derive(Error, Debug)]
pub enum MessagingError {
    #[error("fvm syscall error: `{0}`")]
    Syscall(#[from] ErrorNumber),
    #[error("address not initialised: `{0}`")]
    AddressNotInitialised(Address),
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

    /// Resolves the given address to it's ID address form
    fn resolve_id(&self, address: &Address) -> Result<ActorID>;

    ///  Creates an account at a pubkey address and returns the ID address
    fn initialise_account(&self, address: &Address) -> Result<ActorID>;
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
        actor::resolve_address(address).ok_or(MessagingError::AddressNotInitialised(*address))
    }

    fn initialise_account(&self, address: &Address) -> Result<ActorID> {
        if let Err(e) = send::send(address, METHOD_SEND, Default::default(), TokenAmount::zero()) {
            return Err(e.into());
        }

        actor::resolve_address(address).ok_or(MessagingError::AddressNotInitialised(*address))
    }
}

/// A fake method caller that can simulate the receiving actor return true or false
///
/// If call_receiver_hook is called with an empty data array, it will return true.
/// If call_receiver_hook is called with a non-empty data array, it will return false.
#[derive(Debug, Default, Clone, Copy)]
pub struct FakeMessenger {}

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
        // assume all mock addresses are already in ID form
        address.id().map_err(|_e| MessagingError::AddressNotInitialised(*address))
    }

    fn initialise_account(&self, address: &Address) -> Result<ActorID> {
        match address.payload() {
            fvm_shared::address::Payload::ID(id) => Ok(*id),
            fvm_shared::address::Payload::Secp256k1(secp) => Ok((*secp.get(0).unwrap()).into()),
            fvm_shared::address::Payload::Actor(_) => {
                Err(MessagingError::AddressNotInitialised(*address))
            }
            fvm_shared::address::Payload::BLS(bls) => Ok((*bls.get(0).unwrap()).into()),
        }
    }
}
