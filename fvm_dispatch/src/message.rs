use crate::hash::{Hasher, MethodHasher};

use fvm_ipld_encoding::RawBytes;
use fvm_sdk::{send, SyscallResult};
use fvm_shared::{address::Address, econ::TokenAmount, receipt::Receipt};

#[derive(Default)]
pub struct MethodDispatcher<T: Hasher> {
    method_hasher: MethodHasher<T>,
}

impl<T: Hasher> MethodDispatcher<T> {
    pub fn new(hasher: T) -> Self {
        Self {
            method_hasher: MethodHasher::new(hasher),
        }
    }

    pub fn call_method(
        &self,
        to: &Address,
        method: &str,
        params: RawBytes,
        value: TokenAmount,
    ) -> SyscallResult<Receipt> {
        let method = self.method_hasher.method_number(method);
        send::send(to, method, params, value)
    }
}
