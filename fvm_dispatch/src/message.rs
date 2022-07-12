use crate::hash::{Hasher, MethodResolver};

use fvm_ipld_encoding::RawBytes;
use fvm_sdk::{send, SyscallResult};
use fvm_shared::{address::Address, econ::TokenAmount, receipt::Receipt};

/// Utility to invoke standard methods on actors by publishing an on-chain Message
#[derive(Default)]
pub struct MethodMessenger<T: Hasher> {
    method_resolver: MethodResolver<T>,
}

impl<T: Hasher> MethodMessenger<T> {
    pub fn new(hasher: T) -> Self {
        Self {
            method_resolver: MethodResolver::new(hasher),
        }
    }

    pub fn call_method(
        &self,
        to: &Address,
        method: &str,
        params: RawBytes,
        value: TokenAmount,
    ) -> SyscallResult<Receipt> {
        let method = self.method_resolver.method_number(method);
        send::send(to, method, params, value)
    }
}
