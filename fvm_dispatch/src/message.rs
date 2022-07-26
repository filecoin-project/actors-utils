use thiserror::Error;

use crate::hash::{Hasher, MethodNameErr, MethodResolver};

use fvm_ipld_encoding::RawBytes;
use fvm_sdk::{send, sys::ErrorNumber};
use fvm_shared::{address::Address, econ::TokenAmount, receipt::Receipt};

/// Utility to invoke standard methods on deployed actors
#[derive(Default)]
pub struct MethodMessenger<T: Hasher> {
    method_resolver: MethodResolver<T>,
}

#[derive(Error, PartialEq, Eq, Debug)]
pub enum MethodMessengerError {
    #[error("error when calculating method name: `{0}`")]
    MethodName(#[from] MethodNameErr),
    #[error("error sending message: `{0}`")]
    Syscall(#[from] ErrorNumber),
}

impl<T: Hasher> MethodMessenger<T> {
    /// Creates a new method messenger using a specified hashing function (blake2b by default)
    pub fn new(hasher: T) -> Self {
        Self { method_resolver: MethodResolver::new(hasher) }
    }

    /// Calls a method (by name) on a specified actor by constructing and publishing the underlying
    /// on-chain Message
    pub fn call_method(
        &self,
        to: &Address,
        method: &str,
        params: RawBytes,
        value: TokenAmount,
    ) -> Result<Receipt, MethodMessengerError> {
        let method = self.method_resolver.method_number(method)?;
        send::send(to, method, params, value).map_err(MethodMessengerError::from)
    }
}
