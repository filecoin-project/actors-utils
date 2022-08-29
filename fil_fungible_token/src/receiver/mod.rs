use fvm_ipld_encoding::RawBytes;
use fvm_shared::{address::Address, econ::TokenAmount, error::ExitCode};
use num_traits::Zero;
use types::TokensReceivedParams;

use crate::runtime::messaging::{Messaging, RECEIVER_HOOK_METHOD_NUM};
use crate::token::TokenError;

pub mod types;

#[derive(Debug)]
pub struct ReceiverHookGuard<T> {
    address: Address,
    params: Option<TokensReceivedParams>,
    return_value: Option<T>,
}

impl<T> ReceiverHookGuard<T> {
    pub fn new(address: Address, params: TokensReceivedParams, return_value: T) -> Self {
        ReceiverHookGuard { address, params: Some(params), return_value: Some(return_value) }
    }
    pub fn call(&mut self, msg: &dyn Messaging) -> std::result::Result<T, TokenError> {
        if self.params.is_none() {
            return Err(TokenError::ReceiverHookGuardAlreadyCalled);
        }

        // this will leave self.params set to None, so a further attempt to call() will fail
        let params = self.params.take().unwrap();

        let receipt = msg.send(
            &self.address,
            RECEIVER_HOOK_METHOD_NUM,
            &RawBytes::serialize(&params)?,
            &TokenAmount::zero(),
        )?;

        match receipt.exit_code {
            ExitCode::OK => Ok(self.return_value.take().unwrap()),
            abort_code => Err(TokenError::ReceiverHook {
                from: params.from,
                to: params.to,
                operator: params.operator,
                amount: params.amount,
                exit_code: abort_code,
            }),
        }
    }
}

impl<T> std::ops::Drop for ReceiverHookGuard<T> {
    fn drop(&mut self) {
        if self.params.is_some() {
            panic!("dropped before receiver hook was called");
        }
    }
}
