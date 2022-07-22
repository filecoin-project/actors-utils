use fvm_ipld_encoding::Error as IpldError;
use fvm_ipld_encoding::RawBytes;
use fvm_sdk::{send, sys::ErrorNumber};
use fvm_shared::{address::Address, econ::TokenAmount, ActorID};
use num_traits::Zero;
use thiserror::Error;

use crate::receiver::types::TokenReceivedParams;

type Result<T> = std::result::Result<T, MethodCallError>;

#[derive(Error, Debug)]
pub enum MethodCallError {
    #[error("fvm syscall error: `{0}`")]
    Syscall(#[from] ErrorNumber),
    #[error("ipld serialization error: `{0}`")]
    Ipld(#[from] IpldError),
}

/// An abstraction used to send messages to other actors
pub trait MethodCaller {
    /// Call the receiver hook on a given actor
    ///
    /// Returns true if the receiver hook is called and exits without error, else returns false
    fn call_receiver_hook(
        &self,
        from: ActorID,
        to: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> Result<bool>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FvmMethodCaller {}

// TODO: this should be moved to the example actor so the dependency on fvm_sdk is dropped from the
// library
impl MethodCaller for FvmMethodCaller {
    fn call_receiver_hook(
        &self,
        from: ActorID,
        to: ActorID,
        value: &TokenAmount,
        data: &[u8],
    ) -> Result<bool> {
        // TODO: use fvm_dispatch here (when it supports compile time method resolution)
        // TODO: ^^ necessitates determining conventional method names for receiver hooks

        // currently, the method number comes from taking the name as "TokensReceived" and applying
        // the transformation described in https://github.com/filecoin-project/FIPs/pull/399
        const METHOD_NUM: u64 = 1361519036;
        let to = Address::new_id(to);

        let params = TokenReceivedParams {
            sender: Address::new_id(from),
            value: value.clone(),
            data: RawBytes::from(data.to_vec()),
        };
        let params = RawBytes::new(fvm_ipld_encoding::to_vec(&params)?);

        let receipt = send::send(&to, METHOD_NUM, params, TokenAmount::zero())?;

        Ok(receipt.exit_code.is_success())
    }
}

/// A fake method caller that can simulate the receiving actor return true or false
///
/// If call_receiver_hook is called with an empty data array, it will return true.
/// If call_receiver_hook is called with a non-empty data array, it will return false.
#[derive(Debug, Default, Clone, Copy)]
pub struct FakeMethodCaller {}

impl MethodCaller for FakeMethodCaller {
    fn call_receiver_hook(
        &self,
        _from: ActorID,
        _to: ActorID,
        _value: &TokenAmount,
        data: &[u8],
    ) -> Result<bool> {
        if data.len() == 0 {
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
