use std::{error::Error, fmt::Display};

use crate::hash::{Hasher, MethodNameErr, MethodResolver};

use fvm_ipld_encoding::RawBytes;
use fvm_sdk::{send, sys::ErrorNumber};
use fvm_shared::{address::Address, econ::TokenAmount, receipt::Receipt};

/// Utility to invoke standard methods on deployed actors
#[derive(Default)]
pub struct MethodMessenger<T: Hasher> {
    method_resolver: MethodResolver<T>,
}

#[derive(PartialEq, Debug, Clone)]
pub enum MethodMessengerError {
    MethodName(MethodNameErr),
    Syscall(ErrorNumber),
}

impl Display for MethodMessengerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MethodMessengerError::Syscall(e) => write!(f, "Error when sending message: {:?}", e),
            MethodMessengerError::MethodName(e) => {
                write!(f, "Error calculating method name: {:?}", e)
            }
        }
    }
}

impl Error for MethodMessengerError {}

impl From<ErrorNumber> for MethodMessengerError {
    fn from(e: ErrorNumber) -> Self {
        Self::Syscall(e)
    }
}

impl From<MethodNameErr> for MethodMessengerError {
    fn from(e: MethodNameErr) -> Self {
        Self::MethodName(e)
    }
}

impl<T: Hasher> MethodMessenger<T> {
    /// Creates a new method messenger using a specified hashing function (blake2b by default)
    pub fn new(hasher: T) -> Self {
        Self {
            method_resolver: MethodResolver::new(hasher),
        }
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
